//! (X)HTML → [`Block`]s. Parsed with html5ever (via `scraper`), which tolerates
//! the malformed markup and undeclared entities common in real-world EPUBs.

use scraper::{Html, Node};

use crate::{Block, Chapter};

pub(crate) fn chapter_from_html(html: &str, title: Option<String>) -> Chapter {
    chapters_from_html(html, title, &[])
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Converts one HTML file into chapters, splitting at the element ids in
/// `anchors` (`(id, title)` pairs from the table of contents). EPUBs often
/// pack several chapters into one file and point the TOC at `file#id`.
pub(crate) fn chapters_from_html(
    html: &str,
    title: Option<String>,
    anchors: &[(String, String)],
) -> Vec<Chapter> {
    let doc = Html::parse_document(html);
    let doc_title = doc
        .select(&scraper::Selector::parse("title").expect("valid selector"))
        .next()
        .map(|t| collapse_whitespace(&t.text().collect::<String>()))
        .filter(|t| !t.is_empty());
    let mut walker = Walker {
        anchors: anchors
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.as_str(), i))
            .collect(),
        ..Default::default()
    };
    walker.walk(doc.tree.root());
    walker.flush();

    let mut chapters = Vec::new();
    let mut blocks = walker.blocks;
    // Split from the back so earlier indexes stay valid.
    for (at, anchor) in walker.splits.into_iter().rev() {
        let tail = blocks.split_off(at.min(blocks.len()));
        chapters.push(Chapter {
            title: Some(anchors[anchor].1.clone()),
            blocks: tail,
        });
    }
    if !blocks.is_empty() || chapters.is_empty() {
        chapters.push(Chapter {
            title: title.or(doc_title),
            blocks,
        });
    }
    chapters.reverse();
    chapters.retain(|c| !c.blocks.is_empty());
    if chapters.is_empty() {
        chapters.push(Chapter::default());
    }
    chapters
}

/// Marks `<br>` in the text buffer; source newlines are ordinary whitespace.
/// A private-use character, so `split_whitespace` leaves it alone.
const LINE_BREAK: char = '\u{E000}';

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Paragraph,
    Heading(u8),
    Quote,
    Pre,
}

#[derive(Default)]
struct Walker<'a> {
    blocks: Vec<Block>,
    buf: String,
    kind: Vec<Kind>,
    anchors: std::collections::HashMap<&'a str, usize>,
    /// (block index where a chapter starts, anchor index), in document order.
    splits: Vec<(usize, usize)>,
}

impl Walker<'_> {
    fn current_kind(&self) -> Kind {
        self.kind.last().copied().unwrap_or(Kind::Paragraph)
    }

    fn walk(&mut self, node: ego_tree::NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(text) => self.buf.push_str(text),
            Node::Element(el) => {
                let name = el.name();
                if let Some(&anchor) = el.id().and_then(|id| self.anchors.get(id))
                    && !self.splits.iter().any(|(_, a)| *a == anchor)
                {
                    self.flush();
                    self.splits.push((self.blocks.len(), anchor));
                }
                match name {
                    "head" | "script" | "style" | "title" | "svg" | "nav" => return,
                    "br" => {
                        self.buf.push(LINE_BREAK);
                        return;
                    }
                    "hr" => {
                        self.flush();
                        self.blocks.push(Block::Separator);
                        return;
                    }
                    _ => {}
                }
                let kind = match name {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        Some(Kind::Heading(name.as_bytes()[1] - b'0'))
                    }
                    "blockquote" => Some(Kind::Quote),
                    "pre" => Some(Kind::Pre),
                    _ if is_block(name) => Some(self.current_kind()),
                    _ => None,
                };
                if let Some(kind) = kind {
                    self.flush();
                    self.kind.push(kind);
                }
                for child in node.children() {
                    self.walk(child);
                }
                if kind.is_some() {
                    self.flush();
                    self.kind.pop();
                }
            }
            _ => {
                for child in node.children() {
                    self.walk(child);
                }
            }
        }
    }

    fn flush(&mut self) {
        let raw = std::mem::take(&mut self.buf);
        let kind = self.current_kind();
        if kind == Kind::Pre {
            let text = raw.replace(LINE_BREAK, "\n").trim_matches('\n').to_owned();
            if !text.trim().is_empty() {
                self.blocks.push(Block::Preformatted(text));
            }
            return;
        }
        let text = raw
            .split(LINE_BREAK)
            .map(collapse_whitespace)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return;
        }
        self.blocks.push(match kind {
            Kind::Heading(level) => Block::Heading { level, text },
            Kind::Quote => Block::Quote(text),
            _ => Block::Paragraph(text),
        });
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "aside"
            | "header"
            | "footer"
            | "li"
            | "dt"
            | "dd"
            | "tr"
            | "figcaption"
            | "body"
            | "table"
            | "ul"
            | "ol"
    )
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_file_at_toc_anchors() {
        let html = r#"<html><body>
            <p>Front matter</p>
            <div id="c1"><h2>One</h2><p>a</p></div>
            <p>still one</p>
            <h2 id="c2">Two</h2><p>b</p>
            <p id="unlisted">not a split</p>
            </body></html>"#;
        let anchors = [
            ("c1".to_owned(), "Chapter 1".to_owned()),
            ("c2".to_owned(), "Chapter 2".to_owned()),
        ];
        let chapters = chapters_from_html(html, Some("Start".into()), &anchors);
        let summary: Vec<_> = chapters
            .iter()
            .map(|c| {
                (
                    c.title.as_deref().unwrap(),
                    c.blocks.iter().map(Block::text).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Start", vec!["Front matter"]),
                ("Chapter 1", vec!["One", "a", "still one"]),
                ("Chapter 2", vec!["Two", "b", "not a split"]),
            ]
        );
        // Anchor at the very top: no empty leading chapter.
        let chapters = chapters_from_html(r#"<h1 id="c1">X</h1>"#, None, &anchors[..1]);
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title.as_deref(), Some("Chapter 1"));
    }

    #[test]
    fn converts_common_markup_to_blocks() {
        let html = r#"<?xml version="1.0"?><html><head><title>Ch 1</title>
            <style>p{}</style></head><body>
            <h2>Chapter <em>One</em></h2>
            <p>It was a   <b>dark</b>
               night&nbsp;and&mdash;cold.</p>
            <div>Loose text<p>nested para</p>tail</div>
            <blockquote><p>Quoted</p></blockquote>
            <hr/>
            <pre>  code
  block</pre>
            <p>line<br/>break</p>
            </body></html>"#;
        let ch = chapter_from_html(html, None);
        assert_eq!(ch.title.as_deref(), Some("Ch 1"));
        assert_eq!(
            ch.blocks,
            [
                Block::Heading {
                    level: 2,
                    text: "Chapter One".into()
                },
                Block::Paragraph("It was a dark night and\u{2014}cold.".into()),
                Block::Paragraph("Loose text".into()),
                Block::Paragraph("nested para".into()),
                Block::Paragraph("tail".into()),
                Block::Quote("Quoted".into()),
                Block::Separator,
                Block::Preformatted("  code\n  block".into()),
                Block::Paragraph("line\nbreak".into()),
            ]
        );
    }
}

use std::path::Path;

use crate::{Block, Chapter, Document, Result};

/// Splits plain text into paragraphs on blank lines. Lines that look like
/// chapter markers ("Chapter 3", "CHAPTER IV") start a new chapter.
pub(crate) fn read_document(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(Document {
        title: crate::file_stem(path),
        chapters: parse_text(&text),
    })
}

pub(crate) fn parse_text(text: &str) -> Vec<Chapter> {
    let text = text.replace("\r\n", "\n");
    let mut chapters = vec![Chapter::default()];
    for para in text.split("\n\n") {
        let para = para
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned();
        if para.is_empty() {
            continue;
        }
        if is_chapter_heading(&para) {
            if !chapters.last().is_some_and(|c| c.blocks.is_empty()) {
                chapters.push(Chapter::default());
            }
            let chapter = chapters.last_mut().expect("non-empty");
            chapter.title = Some(para.clone());
            chapter.blocks.push(Block::Heading {
                level: 2,
                text: para,
            });
        } else if para == "* * *" || para == "***" {
            chapters
                .last_mut()
                .expect("non-empty")
                .blocks
                .push(Block::Separator);
        } else {
            chapters
                .last_mut()
                .expect("non-empty")
                .blocks
                .push(Block::Paragraph(para));
        }
    }
    chapters.retain(|c| !c.blocks.is_empty());
    chapters
}

fn is_chapter_heading(para: &str) -> bool {
    if para.len() > 60 {
        return false;
    }
    let lower = para.to_lowercase();
    ["chapter ", "part ", "book ", "prologue", "epilogue"]
        .iter()
        .any(|p| lower.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_paragraphs_and_chapters() {
        let chapters = parse_text(
            "Preface text\nwrapped.\r\n\r\nChapter 1\n\nIt begins.\n\n\n\nCHAPTER II\n\nMore.",
        );
        assert_eq!(chapters.len(), 3);
        assert_eq!(
            chapters[0].blocks,
            [Block::Paragraph("Preface text wrapped.".into())]
        );
        assert_eq!(chapters[1].title.as_deref(), Some("Chapter 1"));
        assert_eq!(chapters[1].blocks[1], Block::Paragraph("It begins.".into()));
        assert_eq!(chapters[2].title.as_deref(), Some("CHAPTER II"));
    }
}

/// A format-independent, reflowable view of a book's text.
///
/// This is deliberately simpler than HTML/CSS: it is what the GPUI reader can
/// lay out natively, and what text-based conversions are built from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub title: String,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chapter {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading { level: u8, text: String },
    Paragraph(String),
    Quote(String),
    Preformatted(String),
    Separator,
}

impl Block {
    pub fn text(&self) -> &str {
        match self {
            Block::Heading { text, .. }
            | Block::Paragraph(text)
            | Block::Quote(text)
            | Block::Preformatted(text) => text,
            Block::Separator => "",
        }
    }
}

impl Chapter {
    /// Chapter title, falling back to its first heading.
    pub fn display_title(&self) -> Option<&str> {
        self.title.as_deref().or_else(|| {
            self.blocks.iter().find_map(|b| match b {
                Block::Heading { text, .. } => Some(text.as_str()),
                _ => None,
            })
        })
    }
}

impl Document {
    /// Plain-text rendering, used by EPUB → TXT conversion.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for chapter in &self.chapters {
            for block in &chapter.blocks {
                match block {
                    Block::Separator => out.push_str("* * *"),
                    Block::Quote(text) => {
                        let quoted: Vec<_> = text.lines().map(|l| format!("    {l}")).collect();
                        out.push_str(&quoted.join("\n"));
                    }
                    other => out.push_str(other.text()),
                }
                out.push_str("\n\n");
            }
            out.push('\n');
        }
        out.trim_end().to_owned() + "\n"
    }
}

//! Ebook format support: metadata and cover extraction, plus a simplified
//! content model ([`Document`]) that the desktop reader renders.

mod document;
pub mod epub;
mod html;
mod pdf;
mod txt;

use std::path::Path;

use nalanda_core::{BookFormat, Metadata};

pub use document::{Block, Chapter, Document};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xml: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("pdf: {0}")]
    Pdf(#[from] lopdf::Error),
    #[error("malformed {format}: {reason}")]
    Malformed { format: BookFormat, reason: String },
    #[error("unsupported format: {0}")]
    Unsupported(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Cover image bytes together with their media type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub data: Vec<u8>,
    pub media_type: String,
}

impl Cover {
    pub fn extension(&self) -> &'static str {
        match self.media_type.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            _ => "jpg",
        }
    }
}

/// Everything Nalanda can learn from a file before importing it.
#[derive(Debug, Clone)]
pub struct BookInfo {
    pub format: BookFormat,
    pub metadata: Metadata,
    pub cover: Option<Cover>,
}

/// Reads metadata (and the cover, where the format embeds one).
///
/// Formats without a native reader still succeed with metadata guessed from
/// the file name, so any ebook can be added to a library.
pub fn read_book_info(path: &Path) -> Result<BookInfo> {
    let format = BookFormat::from_path(path)
        .ok_or_else(|| Error::Unsupported(path.display().to_string()))?;
    let (mut metadata, cover) = match format {
        BookFormat::Epub | BookFormat::Kepub => {
            let mut book = epub::Epub::open(path)?;
            let cover = book.cover()?;
            (book.metadata().clone(), cover)
        }
        BookFormat::Pdf => (pdf::read_metadata(path)?, None),
        _ => (Metadata::default(), None),
    };
    fill_from_file_name(&mut metadata, path);
    Ok(BookInfo {
        format,
        metadata,
        cover,
    })
}

/// Extracts readable content for the built-in reader.
pub fn read_document(path: &Path) -> Result<Document> {
    match BookFormat::from_path(path) {
        Some(BookFormat::Epub | BookFormat::Kepub) => epub::Epub::open(path)?.document(),
        Some(BookFormat::Txt) => txt::read_document(path),
        Some(BookFormat::Html) => {
            let html = std::fs::read_to_string(path)?;
            Ok(Document {
                title: file_stem(path),
                chapters: vec![html::chapter_from_html(&html, None)],
            })
        }
        other => Err(Error::Unsupported(format!(
            "the built-in reader cannot open {}",
            other.map_or_else(|| "this file".to_owned(), |f| f.to_string())
        ))),
    }
}

/// Calibre-style fallback: "Author - Title.epub" or just "Title.epub".
fn fill_from_file_name(metadata: &mut Metadata, path: &Path) {
    let stem = file_stem(path);
    let (author, title) = match stem.split_once(" - ") {
        Some((a, t)) => (Some(a.trim()), t.trim()),
        None => (None, stem.trim()),
    };
    if metadata.title.trim().is_empty() {
        metadata.title = title.to_owned();
    }
    if metadata.authors.is_empty()
        && let Some(author) = author
    {
        metadata.authors.push(author.to_owned());
    }
}

fn file_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let cut = if lower.ends_with(".kepub.epub") {
        name.len() - ".kepub.epub".len()
    } else {
        name.rfind('.').unwrap_or(name.len())
    };
    name[..cut].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_fallback_splits_author_and_title() {
        let mut m = Metadata::default();
        fill_from_file_name(&mut m, Path::new("/x/Jane Austen - Emma.kepub.epub"));
        assert_eq!(m.title, "Emma");
        assert_eq!(m.authors, ["Jane Austen"]);

        let mut m = Metadata {
            title: "Kept".into(),
            ..Default::default()
        };
        fill_from_file_name(&mut m, Path::new("Other - Name.txt"));
        assert_eq!(m.title, "Kept");
        assert_eq!(m.authors, ["Other"]);
    }
}

//! Domain model shared by every Nalanda component.
//!
//! These types double as the wire format of the HTTP API (`nalanda-server`), so
//! the desktop app, the server and any future mobile client agree on one shape.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub type BookId = i64;

/// Ebook file formats Nalanda knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Epub,
    /// Kobo's EPUB variant (`.kepub.epub`).
    Kepub,
    Pdf,
    Mobi,
    Azw3,
    Fb2,
    Txt,
    Html,
    Rtf,
    Docx,
    Cbz,
    Cbr,
}

impl BookFormat {
    pub const ALL: [BookFormat; 12] = [
        Self::Epub,
        Self::Kepub,
        Self::Pdf,
        Self::Mobi,
        Self::Azw3,
        Self::Fb2,
        Self::Txt,
        Self::Html,
        Self::Rtf,
        Self::Docx,
        Self::Cbz,
        Self::Cbr,
    ];

    /// File extension without the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Kepub => "kepub.epub",
            Self::Pdf => "pdf",
            Self::Mobi => "mobi",
            Self::Azw3 => "azw3",
            Self::Fb2 => "fb2",
            Self::Txt => "txt",
            Self::Html => "html",
            Self::Rtf => "rtf",
            Self::Docx => "docx",
            Self::Cbz => "cbz",
            Self::Cbr => "cbr",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Epub | Self::Kepub => "application/epub+zip",
            Self::Pdf => "application/pdf",
            Self::Mobi => "application/x-mobipocket-ebook",
            Self::Azw3 => "application/vnd.amazon.ebook",
            Self::Fb2 => "application/x-fictionbook+xml",
            Self::Txt => "text/plain; charset=utf-8",
            Self::Html => "text/html; charset=utf-8",
            Self::Rtf => "application/rtf",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Cbz => "application/vnd.comicbook+zip",
            Self::Cbr => "application/vnd.comicbook-rar",
        }
    }

    /// Detects the format from a file name, handling the double `.kepub.epub` extension.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let name = path.as_ref().file_name()?.to_str()?.to_ascii_lowercase();
        if name.ends_with(".kepub.epub") || name.ends_with(".kepub") {
            return Some(Self::Kepub);
        }
        let ext = name.rsplit_once('.')?.1;
        match ext {
            "htm" | "xhtml" => Some(Self::Html),
            "azw" => Some(Self::Azw3),
            other => other.parse().ok(),
        }
    }
}

impl fmt::Display for BookFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Kepub => "KEPUB",
            other => return f.write_str(&other.extension().to_ascii_uppercase()),
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFormat(pub String);

impl fmt::Display for UnknownFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown ebook format `{}`", self.0)
    }
}

impl std::error::Error for UnknownFormat {}

impl FromStr for BookFormat {
    type Err = UnknownFormat;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().trim_start_matches('.').to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|f| f.extension() == s || (*f == Self::Kepub && s == "kepub"))
            .ok_or(UnknownFormat(s))
    }
}

/// Descriptive metadata for a book, independent of where it is stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub title: String,
    pub authors: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    /// Publication date as found in the source, usually ISO 8601.
    pub published: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Scheme → value, e.g. `isbn` → `9780141439518`.
    pub identifiers: BTreeMap<String, String>,
}

impl Metadata {
    pub fn authors_display(&self) -> String {
        if self.authors.is_empty() {
            "Unknown".to_owned()
        } else {
            self.authors.join(" & ")
        }
    }

    /// Calibre-style sort key: "Jane Austen" → "Austen, Jane".
    pub fn author_sort(&self) -> String {
        self.authors
            .iter()
            .map(|a| author_sort(a))
            .collect::<Vec<_>>()
            .join(" & ")
    }

    pub fn isbn(&self) -> Option<&str> {
        self.identifiers.get("isbn").map(String::as_str)
    }
}

/// "Jane Austen" → "Austen, Jane"; names already containing a comma are kept.
pub fn author_sort(name: &str) -> String {
    let name = name.trim();
    if name.contains(',') {
        return name.to_owned();
    }
    match name.rsplit_once(' ') {
        Some((first, last)) => format!("{last}, {first}"),
        None => name.to_owned(),
    }
}

/// One stored file of a book in a given format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatFile {
    pub format: BookFormat,
    pub size: u64,
}

/// A book as stored in a library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub id: BookId,
    pub metadata: Metadata,
    pub formats: Vec<FormatFile>,
    pub has_cover: bool,
    pub added_at: chrono::DateTime<chrono::Utc>,
    pub modified_at: chrono::DateTime<chrono::Utc>,
}

impl Book {
    pub fn has_format(&self, format: BookFormat) -> bool {
        self.formats.iter().any(|f| f.format == format)
    }

    /// The format a reader should open, in order of how well Nalanda renders it.
    pub fn preferred_reading_format(&self) -> Option<BookFormat> {
        [BookFormat::Epub, BookFormat::Kepub, BookFormat::Txt]
            .into_iter()
            .find(|f| self.has_format(*f))
    }
}

/// Request body for `POST /api/books/{id}/convert`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertRequest {
    pub to: BookFormat,
    /// Source format; defaults to the best available one.
    pub from: Option<BookFormat>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_detection_handles_kepub_and_aliases() {
        assert_eq!(
            BookFormat::from_path("a/B.kepub.epub"),
            Some(BookFormat::Kepub)
        );
        assert_eq!(BookFormat::from_path("a/B.EPUB"), Some(BookFormat::Epub));
        assert_eq!(BookFormat::from_path("x.htm"), Some(BookFormat::Html));
        assert_eq!(BookFormat::from_path("x.azw"), Some(BookFormat::Azw3));
        assert_eq!(BookFormat::from_path("noext"), None);
        assert_eq!("kepub".parse(), Ok(BookFormat::Kepub));
        assert_eq!(".PDF".parse(), Ok(BookFormat::Pdf));
        assert!("doc".parse::<BookFormat>().is_err());
    }

    #[test]
    fn author_sort_moves_last_name_first() {
        assert_eq!(author_sort("Jane Austen"), "Austen, Jane");
        assert_eq!(author_sort("Austen, Jane"), "Austen, Jane");
        assert_eq!(author_sort("Homer"), "Homer");
    }
}

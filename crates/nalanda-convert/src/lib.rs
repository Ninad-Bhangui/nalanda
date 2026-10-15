//! Format conversion.
//!
//! [`Converters`] holds an ordered list of backends and uses the first one
//! that supports a given `from → to` pair:
//!
//! 1. [`NativeConverter`]: pure Rust, handles text-centric conversions
//!    (EPUB/KEPUB/HTML/TXT → EPUB/KEPUB/TXT).
//! 2. [`CalibreConverter`]: shells out to calibre's `ebook-convert` when it is
//!    installed, covering everything else (MOBI, AZW3, PDF, DOCX, FB2, ...).

use std::path::{Path, PathBuf};
use std::process::Command;

use nalanda_core::{BookFormat, Metadata};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no converter available for {from} → {to}{hint}")]
    Unsupported {
        from: BookFormat,
        to: BookFormat,
        hint: &'static str,
    },
    #[error(transparent)]
    Format(#[from] nalanda_formats::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ebook-convert failed: {0}")]
    External(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub trait Converter: Send + Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, from: BookFormat, to: BookFormat) -> bool;
    /// Converts `input` into `output`. `metadata` is embedded where the target supports it.
    fn convert(&self, input: &Path, output: &Path, metadata: &Metadata) -> Result<()>;
}

/// Pure-Rust conversions built on [`nalanda_formats::Document`].
///
/// These keep text, headings and structure, not styling or images, which is
/// enough for reflowable reading on e-ink devices.
pub struct NativeConverter;

impl NativeConverter {
    const INPUTS: [BookFormat; 4] = [
        BookFormat::Epub,
        BookFormat::Kepub,
        BookFormat::Txt,
        BookFormat::Html,
    ];
    const OUTPUTS: [BookFormat; 3] = [BookFormat::Epub, BookFormat::Kepub, BookFormat::Txt];
}

impl Converter for NativeConverter {
    fn name(&self) -> &'static str {
        "Nalanda (built-in)"
    }

    fn supports(&self, from: BookFormat, to: BookFormat) -> bool {
        from != to && Self::INPUTS.contains(&from) && Self::OUTPUTS.contains(&to)
    }

    fn convert(&self, input: &Path, output: &Path, metadata: &Metadata) -> Result<()> {
        let from = BookFormat::from_path(input).unwrap_or(BookFormat::Txt);
        // EPUB ↔ KEPUB is a container-level copy: Kobo reads plain EPUB content.
        let is_epub = |f| matches!(f, BookFormat::Epub | BookFormat::Kepub);
        let to = BookFormat::from_path(output).unwrap_or(BookFormat::Txt);
        if is_epub(from) && is_epub(to) {
            std::fs::copy(input, output)?;
            return Ok(());
        }
        let doc = nalanda_formats::read_document(input)?;
        match to {
            BookFormat::Txt => std::fs::write(output, doc.to_plain_text())?,
            _ => {
                let cover = if is_epub(from) {
                    nalanda_formats::read_book_info(input)?.cover
                } else {
                    None
                };
                let mut metadata = metadata.clone();
                if metadata.title.is_empty() {
                    metadata.title = doc.title.clone();
                }
                nalanda_formats::epub::write_epub(
                    output,
                    &metadata,
                    &doc.chapters,
                    cover.as_ref(),
                )?;
            }
        }
        Ok(())
    }
}

/// Wraps calibre's `ebook-convert` command-line tool.
pub struct CalibreConverter {
    binary: PathBuf,
}

impl CalibreConverter {
    /// Finds `ebook-convert` on `PATH` or in calibre's default install locations.
    pub fn detect() -> Option<Self> {
        let exe = if cfg!(windows) {
            "ebook-convert.exe"
        } else {
            "ebook-convert"
        };
        let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).map(|d| d.join(exe)).collect())
            .unwrap_or_default();
        candidates.extend(
            [
                "/Applications/calibre.app/Contents/MacOS/ebook-convert",
                "/opt/calibre/ebook-convert",
                r"C:\Program Files\Calibre2\ebook-convert.exe",
            ]
            .map(PathBuf::from),
        );
        candidates
            .into_iter()
            .find(|p| p.is_file())
            .map(|binary| Self { binary })
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl Converter for CalibreConverter {
    fn name(&self) -> &'static str {
        "calibre ebook-convert"
    }

    fn supports(&self, from: BookFormat, to: BookFormat) -> bool {
        // ebook-convert cannot write CBR/RTF-from-comics etc.; it reads everything we list.
        from != to && !matches!(to, BookFormat::Cbr | BookFormat::Kepub)
    }

    fn convert(&self, input: &Path, output: &Path, metadata: &Metadata) -> Result<()> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg(input).arg(output);
        if !metadata.title.is_empty() {
            cmd.arg("--title").arg(&metadata.title);
        }
        if !metadata.authors.is_empty() {
            cmd.arg("--authors").arg(metadata.authors.join(" & "));
        }
        let out = cmd.output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
            return Err(Error::External(
                tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            ));
        }
        Ok(())
    }
}

/// The ordered set of available converters.
pub struct Converters {
    backends: Vec<Box<dyn Converter>>,
}

impl Default for Converters {
    /// Built-in converter first, then calibre if it is installed.
    fn default() -> Self {
        let mut backends: Vec<Box<dyn Converter>> = vec![Box::new(NativeConverter)];
        if let Some(calibre) = CalibreConverter::detect() {
            backends.push(Box::new(calibre));
        }
        Self { backends }
    }
}

impl Converters {
    pub fn new(backends: Vec<Box<dyn Converter>>) -> Self {
        Self { backends }
    }

    pub fn find(&self, from: BookFormat, to: BookFormat) -> Option<&dyn Converter> {
        self.backends
            .iter()
            .find(|c| c.supports(from, to))
            .map(|c| c.as_ref())
    }

    /// Target formats reachable from any of `sources`.
    pub fn targets(&self, sources: &[BookFormat]) -> Vec<BookFormat> {
        BookFormat::ALL
            .into_iter()
            .filter(|to| !sources.contains(to))
            .filter(|to| sources.iter().any(|from| self.find(*from, *to).is_some()))
            .collect()
    }

    pub fn has_calibre(&self) -> bool {
        self.backends
            .iter()
            .any(|b| b.name() == "calibre ebook-convert")
    }

    /// Converts `input` into `output`, returning the backend's name.
    pub fn convert(
        &self,
        from: BookFormat,
        to: BookFormat,
        input: &Path,
        output: &Path,
        metadata: &Metadata,
    ) -> Result<&'static str> {
        let converter = self.find(from, to).ok_or(Error::Unsupported {
            from,
            to,
            hint: if self.has_calibre() {
                ""
            } else {
                " (install calibre to enable more conversions)"
            },
        })?;
        converter.convert(input, output, metadata)?;
        Ok(converter.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalanda_formats::{Block, Chapter};

    #[test]
    fn native_txt_to_epub_to_txt_preserves_text() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("in.txt");
        std::fs::write(&txt, "Chapter 1\n\nFirst para\nwrapped.\n\nSecond.\n").unwrap();
        let epub = dir.path().join("out.epub");
        let meta = Metadata {
            title: "T".into(),
            authors: vec!["A".into()],
            ..Default::default()
        };
        let converters = Converters::new(vec![Box::new(NativeConverter)]);
        converters
            .convert(BookFormat::Txt, BookFormat::Epub, &txt, &epub, &meta)
            .unwrap();

        let info = nalanda_formats::read_book_info(&epub).unwrap();
        assert_eq!(info.metadata.title, "T");
        assert_eq!(info.metadata.authors, ["A"]);
        let doc = nalanda_formats::read_document(&epub).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(
            doc.chapters[0].blocks,
            [
                Block::Heading {
                    level: 2,
                    text: "Chapter 1".into()
                },
                Block::Paragraph("First para wrapped.".into()),
                Block::Paragraph("Second.".into()),
            ]
        );

        let back = dir.path().join("back.txt");
        converters
            .convert(BookFormat::Epub, BookFormat::Txt, &epub, &back, &meta)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(back).unwrap(),
            "Chapter 1\n\nFirst para wrapped.\n\nSecond.\n"
        );
    }

    #[test]
    fn epub_to_epub_keeps_cover() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        std::fs::write(&src, "x").unwrap();
        let epub = dir.path().join("a.epub");
        let cover = nalanda_formats::Cover {
            data: vec![1, 2, 3],
            media_type: "image/png".into(),
        };
        let ch = Chapter {
            title: None,
            blocks: vec![Block::Paragraph("x".into())],
        };
        nalanda_formats::epub::write_epub(&epub, &Metadata::default(), &[ch], Some(&cover))
            .unwrap();
        let kepub = dir.path().join("a.kepub.epub");
        Converters::new(vec![Box::new(NativeConverter)])
            .convert(
                BookFormat::Epub,
                BookFormat::Kepub,
                &epub,
                &kepub,
                &Metadata::default(),
            )
            .unwrap();
        assert_eq!(
            nalanda_formats::read_book_info(&kepub).unwrap().cover,
            Some(cover)
        );
    }

    #[test]
    fn unsupported_pair_reports_hint_and_fallback_order() {
        let native_only = Converters::new(vec![Box::new(NativeConverter)]);
        let err = native_only
            .convert(
                BookFormat::Epub,
                BookFormat::Pdf,
                Path::new("a"),
                Path::new("b"),
                &Metadata::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("install calibre"), "{err}");
        assert_eq!(
            native_only.targets(&[BookFormat::Txt]),
            [BookFormat::Epub, BookFormat::Kepub]
        );

        let both = Converters::new(vec![
            Box::new(NativeConverter),
            Box::new(CalibreConverter::with_binary("/nonexistent")),
        ]);
        assert_eq!(
            both.find(BookFormat::Epub, BookFormat::Txt).unwrap().name(),
            "Nalanda (built-in)"
        );
        assert_eq!(
            both.find(BookFormat::Epub, BookFormat::Pdf).unwrap().name(),
            "calibre ebook-convert"
        );
    }
}

/// Source formats in order of conversion quality.
const SOURCE_PREFERENCE: [BookFormat; 10] = [
    BookFormat::Epub,
    BookFormat::Kepub,
    BookFormat::Azw3,
    BookFormat::Mobi,
    BookFormat::Docx,
    BookFormat::Fb2,
    BookFormat::Html,
    BookFormat::Rtf,
    BookFormat::Txt,
    BookFormat::Pdf,
];

#[derive(Debug, thiserror::Error)]
pub enum LibraryConvertError {
    #[error(transparent)]
    Library(#[from] nalanda_library::Error),
    #[error(transparent)]
    Convert(#[from] Error),
}

/// Converts a library book to `to` and stores the result as a new format.
/// Returns the name of the converter that ran.
pub fn convert_library_book(
    library: &nalanda_library::Library,
    converters: &Converters,
    id: nalanda_core::BookId,
    to: BookFormat,
    from: Option<BookFormat>,
) -> Result<&'static str, LibraryConvertError> {
    let book = library.get(id)?;
    let available: Vec<BookFormat> = book.formats.iter().map(|f| f.format).collect();
    let from = match from {
        Some(f) => f,
        None => SOURCE_PREFERENCE
            .into_iter()
            .filter(|f| available.contains(f))
            .find(|f| converters.find(*f, to).is_some())
            .or_else(|| available.first().copied())
            .ok_or(nalanda_library::Error::MissingFormat(id, BookFormat::Epub))?,
    };
    let input = library.format_path(id, from)?;
    let tmp = library
        .book_path(id)?
        .join(format!(".converting.{}", to.extension()));
    let result = converters.convert(from, to, &input, &tmp, &book.metadata);
    let outcome = result.map_err(LibraryConvertError::from).and_then(|name| {
        library.add_format(id, &tmp, to)?;
        Ok(name)
    });
    let _ = std::fs::remove_file(&tmp);
    outcome
}

#[cfg(test)]
mod library_tests {
    use super::*;

    #[test]
    fn converts_library_book_and_attaches_format() {
        let src = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let lib = nalanda_library::Library::open(root.path()).unwrap();
        let txt = src.path().join("Ann Author - Short Story.txt");
        std::fs::write(&txt, "Once upon a time.").unwrap();
        let book = lib.add_book(&txt).unwrap();
        assert_eq!(book.metadata.title, "Short Story");

        let converters = Converters::new(vec![Box::new(NativeConverter)]);
        let used =
            convert_library_book(&lib, &converters, book.id, BookFormat::Epub, None).unwrap();
        assert_eq!(used, "Nalanda (built-in)");

        let book = lib.get(book.id).unwrap();
        assert!(book.has_format(BookFormat::Epub));
        let epub = lib.format_path(book.id, BookFormat::Epub).unwrap();
        let info = nalanda_formats::read_book_info(&epub).unwrap();
        assert_eq!(info.metadata.authors, ["Ann Author"]);
        // No temp file left in the book directory.
        let leftovers: Vec<_> = std::fs::read_dir(lib.book_path(book.id).unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty());

        let err =
            convert_library_book(&lib, &converters, book.id, BookFormat::Pdf, None).unwrap_err();
        assert!(err.to_string().contains("install calibre"), "{err}");
    }
}

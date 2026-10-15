use std::path::{Path, PathBuf};

use nalanda_core::{BookFormat, Metadata};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{Error, Result};

/// Where the device sees its own storage; sideloaded ContentIDs are
/// `file:///mnt/onboard/<path relative to the USB mount>`.
const ONBOARD: &str = "file:///mnt/onboard/";

/// `ContentType` of top-level books in `KoboReader.sqlite` (chapters use other values).
const CONTENT_TYPE_BOOK: i64 = 6;

/// Parsed `.kobo/version`: `serial,?,firmware,?,?,model-id`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KoboInfo {
    pub serial: String,
    pub firmware: String,
    pub model_id: String,
}

impl KoboInfo {
    pub fn parse(version_file: &str) -> Option<Self> {
        let fields: Vec<&str> = version_file.trim().split(',').map(str::trim).collect();
        if fields.len() < 3 || fields[0].is_empty() {
            return None;
        }
        Some(Self {
            serial: fields[0].to_owned(),
            firmware: fields[2].to_owned(),
            model_id: fields.last().copied().unwrap_or_default().to_owned(),
        })
    }

    /// Short product code from the model id GUID, e.g. `…000000000388` → `388`.
    pub fn product_code(&self) -> &str {
        let tail = self.model_id.rsplit('-').next().unwrap_or_default();
        let trimmed = tail.trim_start_matches('0');
        if trimmed.is_empty() { tail } else { trimmed }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStatus {
    Unread,
    Reading,
    Finished,
}

/// A book on the device: indexed by Kobo (has `content_id`), present as a
/// file but not yet indexed (copied since the last eject), or both.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceBook {
    pub content_id: Option<String>,
    /// Absolute path on the mounted volume; `None` for store-bought books
    /// whose files are encrypted and managed by the device.
    pub path: Option<PathBuf>,
    pub title: String,
    pub authors: String,
    pub percent_read: f64,
    pub status: ReadStatus,
    pub last_read: Option<String>,
}

impl DeviceBook {
    pub fn is_sideloaded(&self) -> bool {
        self.path.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct KoboDevice {
    pub mount: PathBuf,
    pub info: KoboInfo,
}

impl KoboDevice {
    /// Opens a Kobo mounted at `mount`.
    pub fn open(mount: &Path) -> Result<Self> {
        let kobo_dir = mount.join(".kobo");
        if !kobo_dir.join("KoboReader.sqlite").is_file() {
            return Err(Error::NotAKobo(mount.to_owned()));
        }
        let info = std::fs::read_to_string(kobo_dir.join("version"))
            .ok()
            .and_then(|v| KoboInfo::parse(&v))
            .unwrap_or_default();
        Ok(Self {
            mount: mount.to_owned(),
            info,
        })
    }

    pub fn display_name(&self) -> String {
        match self.info.product_code() {
            "" => "Kobo eReader".to_owned(),
            code => format!("Kobo eReader ({code})"),
        }
    }

    fn db_path(&self) -> PathBuf {
        self.mount.join(".kobo").join("KoboReader.sqlite")
    }

    fn content_id_for(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.mount).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        Some(format!("{ONBOARD}{rel}"))
    }

    fn path_for(&self, content_id: &str) -> Option<PathBuf> {
        content_id
            .strip_prefix(ONBOARD)
            .map(|rel| self.mount.join(rel))
    }

    /// Lists books: everything Kobo has indexed plus sideloaded files it has
    /// not seen yet.
    pub fn books(&self) -> Result<Vec<DeviceBook>> {
        let conn = Connection::open_with_flags(
            self.db_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut stmt = conn.prepare(
            "SELECT ContentID, Title, Attribution, ___PercentRead, ReadStatus, DateLastRead
             FROM content WHERE ContentType = ?1 ORDER BY Title COLLATE NOCASE",
        )?;
        let mut books = stmt
            .query_map([CONTENT_TYPE_BOOK], |r| {
                let content_id: String = r.get(0)?;
                Ok(DeviceBook {
                    path: self.path_for(&content_id),
                    content_id: Some(content_id),
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    authors: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    percent_read: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    status: match r.get::<_, Option<i64>>(4)?.unwrap_or(0) {
                        1 => ReadStatus::Reading,
                        2 => ReadStatus::Finished,
                        _ => ReadStatus::Unread,
                    },
                    last_read: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let indexed: std::collections::HashSet<PathBuf> =
            books.iter().filter_map(|b| b.path.clone()).collect();
        let mut pending = Vec::new();
        collect_ebook_files(&self.mount, &mut pending)?;
        for path in pending.into_iter().filter(|p| !indexed.contains(p)) {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().trim_end_matches(".kepub").to_owned())
                .unwrap_or_default();
            // Files Nalanda (or calibre) sent are named "<Title> - <Authors>".
            let (title, authors) = match stem.rsplit_once(" - ") {
                Some((t, a)) => (t.to_owned(), a.to_owned()),
                None => (stem, String::new()),
            };
            books.push(DeviceBook {
                content_id: None,
                path: Some(path),
                title,
                authors,
                percent_read: 0.0,
                status: ReadStatus::Unread,
                last_read: None,
            });
        }
        Ok(books)
    }

    /// Copies a book onto the device as `<Author>/<Title> - <Author>.<ext>`
    /// (calibre's default layout). Kobo indexes it after the device is ejected.
    pub fn send_book(&self, source: &Path, metadata: &Metadata) -> Result<PathBuf> {
        let format = BookFormat::from_path(source).unwrap_or(BookFormat::Epub);
        let author = metadata
            .authors
            .first()
            .map(String::as_str)
            .unwrap_or("Unknown");
        let name = format!(
            "{} - {}.{}",
            sanitize(&metadata.title),
            sanitize(&metadata.authors_display()),
            format.extension()
        );
        let dir = self.mount.join(sanitize(author));
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(name);
        std::fs::copy(source, &dest)?;
        Ok(dest)
    }

    /// Deletes a sideloaded book: removes the file and its rows from the
    /// device database so it disappears from the home screen immediately.
    pub fn delete_book(&self, book: &DeviceBook) -> Result<()> {
        let Some(path) = &book.path else {
            return Err(Error::NotSideloaded(book.title.clone()));
        };
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        if let Some(parent) = path.parent()
            && parent != self.mount
        {
            let _ = std::fs::remove_dir(parent); // only succeeds if empty
        }
        let content_id = book
            .content_id
            .clone()
            .or_else(|| self.content_id_for(path));
        if let Some(id) = content_id {
            let mut conn = Connection::open(self.db_path())?;
            let tx = conn.transaction()?;
            // Chapters reference their book via BookID.
            tx.execute(
                "DELETE FROM content WHERE ContentID = ?1 OR BookID = ?1",
                [&id],
            )?;
            let has_shelves: Option<String> = tx
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'ShelfContent'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if has_shelves.is_some() {
                tx.execute("DELETE FROM ShelfContent WHERE ContentId = ?1", [&id])?;
            }
            tx.commit()?;
        }
        Ok(())
    }
}

fn collect_ebook_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue; // .kobo, .adobe-digital-editions, macOS metadata, ...
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_ebook_files(&path, out)?;
        } else if file_type.is_file() && BookFormat::from_path(&path).is_some() {
            out.push(path);
        }
    }
    Ok(())
}

fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.');
    if cleaned.is_empty() || cleaned.starts_with('.') {
        format!("_{cleaned}")
    } else {
        cleaned.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake mounted Kobo with the subset of the real schema Nalanda touches.
    fn fake_kobo() -> tempfile::TempDir {
        let mount = tempfile::tempdir().unwrap();
        let kobo = mount.path().join(".kobo");
        std::fs::create_dir_all(&kobo).unwrap();
        std::fs::write(
            kobo.join("version"),
            "N4181C1234567,4.1.15,4.38.21908,4.1.15,4.1.15,00000000-0000-0000-0000-000000000388\n",
        )
        .unwrap();
        let conn = Connection::open(kobo.join("KoboReader.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE content (ContentID TEXT, ContentType INTEGER, BookID TEXT,
                Title TEXT, Attribution TEXT, ___PercentRead REAL, ReadStatus INTEGER,
                DateLastRead TEXT);
             CREATE TABLE ShelfContent (ShelfName TEXT, ContentId TEXT);
             INSERT INTO content VALUES
               ('file:///mnt/onboard/Austen/Emma - Jane Austen.epub', 6, NULL, 'Emma', 'Jane Austen', 42.0, 1, '2026-01-01'),
               ('file:///mnt/onboard/Austen/Emma - Jane Austen.epub#(0)chapter1.xhtml', 9, 'file:///mnt/onboard/Austen/Emma - Jane Austen.epub', 'Ch1', NULL, NULL, NULL, NULL),
               ('3fa2b1c0-store-book', 6, NULL, 'Store Book', 'Someone', 100.0, 2, NULL);
             INSERT INTO ShelfContent VALUES ('Classics', 'file:///mnt/onboard/Austen/Emma - Jane Austen.epub');",
        )
        .unwrap();
        std::fs::create_dir_all(mount.path().join("Austen")).unwrap();
        std::fs::write(mount.path().join("Austen/Emma - Jane Austen.epub"), "epub").unwrap();
        mount
    }

    #[test]
    fn parses_version_file() {
        let info =
            KoboInfo::parse("N418,4.1,4.38.21908,4.1,4.1,00000000-0000-0000-0000-000000000388")
                .unwrap();
        assert_eq!(info.serial, "N418");
        assert_eq!(info.firmware, "4.38.21908");
        assert_eq!(info.product_code(), "388");
        assert_eq!(KoboInfo::parse(""), None);
    }

    #[test]
    fn rejects_non_kobo_volume() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            KoboDevice::open(dir.path()),
            Err(Error::NotAKobo(_))
        ));
    }

    #[test]
    fn lists_indexed_store_and_pending_books() {
        let mount = fake_kobo();
        let device = KoboDevice::open(mount.path()).unwrap();
        assert_eq!(device.info.firmware, "4.38.21908");

        let meta = Metadata {
            title: "Persuasion: A Novel".into(),
            authors: vec!["Jane Austen".into()],
            ..Default::default()
        };
        let src = mount.path().join("..").join("src-persuasion.epub");
        std::fs::write(&src, "data").unwrap();
        let sent = device.send_book(&src, &meta).unwrap();
        std::fs::remove_file(&src).unwrap();
        assert_eq!(
            sent,
            mount
                .path()
                .join("Jane Austen/Persuasion_ A Novel - Jane Austen.epub")
        );

        let books = device.books().unwrap();
        let summary: Vec<_> = books
            .iter()
            .map(|b| {
                (
                    b.title.as_str(),
                    b.content_id.is_some(),
                    b.is_sideloaded(),
                    b.status,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Emma", true, true, ReadStatus::Reading),
                ("Store Book", true, false, ReadStatus::Finished),
                ("Persuasion_ A Novel", false, true, ReadStatus::Unread),
            ]
        );
        assert_eq!(books[2].authors, "Jane Austen");
        assert_eq!(books[0].percent_read, 42.0);
        assert_eq!(
            books[0].path.as_deref(),
            Some(
                mount
                    .path()
                    .join("Austen/Emma - Jane Austen.epub")
                    .as_path()
            )
        );
    }

    #[test]
    fn delete_removes_file_rows_and_shelf_entries_only_for_sideloaded() {
        let mount = fake_kobo();
        let device = KoboDevice::open(mount.path()).unwrap();
        let books = device.books().unwrap();
        let emma = books.iter().find(|b| b.title == "Emma").unwrap();
        let store = books.iter().find(|b| b.title == "Store Book").unwrap();

        assert!(matches!(
            device.delete_book(store),
            Err(Error::NotSideloaded(_))
        ));
        device.delete_book(emma).unwrap();

        assert!(
            !mount.path().join("Austen").exists(),
            "empty author dir removed"
        );
        let conn = Connection::open(mount.path().join(".kobo/KoboReader.sqlite")).unwrap();
        let remaining: Vec<String> = conn
            .prepare("SELECT ContentID FROM content")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(remaining, ["3fa2b1c0-store-book"]);
        let shelf: i64 = conn
            .query_row("SELECT COUNT(*) FROM ShelfContent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shelf, 0);
    }
}

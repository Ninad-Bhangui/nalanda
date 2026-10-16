//! Nalanda 0.0.1's own catalog, `nalanda.db`. It is only read, to convert
//! such a library into a calibre library the first time it is opened.
//!
//! The book folders already follow calibre's layout and stay where they are.
//! KEPUB files are renamed from `.kepub.epub` to calibre's `.kepub`, covers
//! become `cover.jpg`, and `nalanda.db` is moved to the backup folder, where
//! calibre's Check Library won't flag it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nalanda_core::{Book, BookFormat, Metadata};
use rusqlite::{Connection, OpenFlags};

use crate::{Result, calibre};

pub(crate) const DB_FILE: &str = "nalanda.db";

/// A book as Nalanda 0.0.1 stored it.
struct OldBook {
    book: Book,
    dir: String,
    cover: Option<String>,
    /// Format, file name in `dir`, size.
    files: Vec<(BookFormat, String, u64)>,
}

/// Converts the Nalanda 0.0.1 library in `root` into a calibre library and
/// moves the old catalog into `backup_dir` (or keeps it in `root` as
/// `nalanda.db.converted` when there is none).
pub(crate) fn convert(root: &Path, backup_dir: Option<&Path>) -> Result<()> {
    let books = read(&root.join(DB_FILE))?;
    // Covers first: writing cover.jpg next to the old cover changes nothing
    // the old catalog depends on.
    let mut has_cover = Vec::new();
    for old in &books {
        has_cover.push(match &old.cover {
            Some(cover) => ensure_jpeg_cover(&root.join(&old.dir), cover),
            None => false,
        });
    }

    // Build the catalog under a temporary name, so a failure leaves the
    // library as it was.
    let building = root.join("metadata.db.converting");
    let _ = std::fs::remove_file(&building);
    let renames = (|| -> Result<Vec<(String, String)>> {
        calibre::create(&building)?;
        let mut conn = calibre::open(&building)?;
        let tx = conn.transaction()?;
        let mut renames = Vec::new();
        for (old, has_cover) in books.iter().zip(has_cover) {
            let mut book = old.book.clone();
            book.has_cover = has_cover;
            calibre::insert_existing_book(&tx, &book, &old.dir)?;
            for (format, file, size) in &old.files {
                let ext = format.extension();
                let name = file
                    .strip_suffix(&format!(".{ext}"))
                    .unwrap_or(file.as_str());
                calibre::record_format(&tx, book.id, *format, name, *size)?;
                let calibre_file = format!(
                    "{name}.{}",
                    calibre::format_name(*format).to_ascii_lowercase()
                );
                if calibre_file != *file {
                    renames.push((
                        format!("{}/{file}", old.dir),
                        format!("{}/{calibre_file}", old.dir),
                    ));
                }
            }
            calibre::set_times(&tx, book.id, book.added_at, book.modified_at)?;
        }
        tx.commit()?;
        Ok(renames)
    })();
    let renames = match renames {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&building);
            return Err(e);
        }
    };

    for (from, to) in renames {
        let (from, to) = (root.join(from), root.join(to));
        // A converting run that was interrupted may have renamed it already.
        if from.exists() || !to.exists() {
            std::fs::rename(from, to)?;
        }
    }
    let db = root.join(calibre::DB_FILE);
    std::fs::rename(&building, &db)?;
    let old_catalog = match backup_dir {
        Some(dir) => {
            let id: String =
                calibre::open(&db)?.query_row("SELECT uuid FROM library_id", [], |r| r.get(0))?;
            std::fs::create_dir_all(dir)?;
            dir.join(format!("{id}.nalanda-0.0.1.db"))
        }
        None => root.join("nalanda.db.converted"),
    };
    move_file(&root.join(DB_FILE), &old_catalog)?;
    for old in &books {
        if let Some(cover) = old.cover.as_deref().filter(|c| *c != "cover.jpg") {
            let _ = std::fs::remove_file(root.join(&old.dir).join(cover));
        }
    }
    Ok(())
}

/// Renames, or copies and deletes when `to` is on another file system.
fn move_file(from: &Path, to: &PathBuf) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
    }
    Ok(())
}

/// Makes sure the book folder has a JPEG `cover.jpg`; returns whether it does.
fn ensure_jpeg_cover(dir: &Path, cover: &str) -> bool {
    let Ok(data) = std::fs::read(dir.join(cover)) else {
        return false;
    };
    match calibre::cover_jpeg(&data) {
        Ok(jpeg) if cover == "cover.jpg" && *jpeg == *data => true,
        Ok(jpeg) => std::fs::write(dir.join("cover.jpg"), &jpeg).is_ok(),
        Err(_) => false,
    }
}

fn read(path: &Path) -> Result<Vec<OldBook>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    // 0.0.1 used WAL mode. Opening read-write lets SQLite fold a leftover -wal
    // file back in; leaving WAL mode removes -wal and -shm, so the catalog is
    // a single file that can be moved.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    let mut stmt = conn.prepare(
        "SELECT id, title, authors, series, series_index, publisher, language, published,
            description, tags, identifiers, path, cover, added_at, modified_at
         FROM books ORDER BY id",
    )?;
    let mut books = stmt
        .query_map([], |r| {
            let json = |i: usize| -> rusqlite::Result<String> { r.get(i) };
            let date = |i: usize| -> rusqlite::Result<DateTime<Utc>> {
                Ok(DateTime::parse_from_rfc3339(&r.get::<_, String>(i)?)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_default())
            };
            Ok(OldBook {
                book: Book {
                    id: r.get(0)?,
                    metadata: Metadata {
                        title: r.get(1)?,
                        authors: serde_json::from_str(&json(2)?).unwrap_or_default(),
                        series: r.get(3)?,
                        series_index: r.get(4)?,
                        publisher: r.get(5)?,
                        language: r.get(6)?,
                        published: r.get(7)?,
                        description: r.get(8)?,
                        tags: serde_json::from_str(&json(9)?).unwrap_or_default(),
                        identifiers: serde_json::from_str(&json(10)?).unwrap_or_default(),
                    },
                    formats: Vec::new(),
                    has_cover: false,
                    added_at: date(13)?,
                    modified_at: date(14)?,
                },
                dir: r.get(11)?,
                cover: r.get(12)?,
                files: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT format, file_name, size FROM formats WHERE book_id = ?1 ORDER BY format",
    )?;
    for old in &mut books {
        old.files = stmt
            .query_map([old.book.id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .filter_map(|row| match row {
                Ok((format, file, size)) => format
                    .parse()
                    .ok()
                    .map(|f| Ok((f, file, size.max(0) as u64))),
                Err(e) => Some(Err(e)),
            })
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(books)
}

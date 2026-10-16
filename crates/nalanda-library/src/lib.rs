//! A Nalanda library is a directory:
//!
//! ```text
//! <root>/nalanda.db                         catalog (SQLite)
//! <root>/<Author>/<Title> (<id>)/         one directory per book
//!     <Title> - <Author>.<ext>            one file per format
//!     cover.<ext>
//! ```
//!
//! The layout mirrors calibre's, so the files stay usable without Nalanda.
//! A calibre library (one with calibre's `metadata.db`) opens directly and is
//! edited in place; see [`calibre`].
//! [`Library`] is `Sync` and is shared by the desktop app and the server.

mod calibre;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use nalanda_core::{Book, BookFormat, BookId, FormatFile, Metadata};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Format(#[from] nalanda_formats::Error),
    #[error("book {0} not found")]
    NotFound(BookId),
    #[error("book {0} has no {1} file")]
    MissingFormat(BookId, BookFormat),
    #[error("cover image: {0}")]
    Cover(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS books (
    id           INTEGER PRIMARY KEY,
    title        TEXT NOT NULL,
    sort_title   TEXT NOT NULL,
    authors      TEXT NOT NULL,      -- JSON array
    author_sort  TEXT NOT NULL,
    series       TEXT,
    series_index REAL,
    publisher    TEXT,
    language     TEXT,
    published    TEXT,
    description  TEXT,
    tags         TEXT NOT NULL,      -- JSON array
    identifiers  TEXT NOT NULL,      -- JSON object
    path         TEXT NOT NULL,      -- directory relative to the library root
    cover        TEXT,               -- cover file name inside `path`
    added_at     TEXT NOT NULL,
    modified_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS formats (
    book_id   INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format    TEXT NOT NULL,
    file_name TEXT NOT NULL,
    size      INTEGER NOT NULL,
    PRIMARY KEY (book_id, format)
);
"#;

/// Sort orders offered by [`Library::list`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortBy {
    #[default]
    RecentlyAdded,
    Title,
    Author,
}

/// Catalog file inside a library folder.
const DB_FILE: &str = "nalanda.db";

/// Which catalog a library folder uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Catalog {
    Nalanda,
    Calibre,
}

pub struct Library {
    root: PathBuf,
    conn: Mutex<Connection>,
    catalog: Catalog,
    /// Where to back up a calibre catalog before the first write, if anywhere.
    backup_dir: Option<PathBuf>,
}

impl Library {
    /// Opens the library at `root`, creating it if needed. A calibre library
    /// is opened in place.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_backup_dir(root.into(), calibre::default_backup_dir())
    }

    fn open_with_backup_dir(root: PathBuf, backup_dir: Option<PathBuf>) -> Result<Self> {
        let calibre_db = root.join(calibre::DB_FILE);
        if calibre_db.is_file() {
            return Ok(Self {
                conn: Mutex::new(calibre::open(&calibre_db)?),
                root,
                catalog: Catalog::Calibre,
                backup_dir,
            });
        }
        std::fs::create_dir_all(&root)?;
        let conn = Connection::open(root.join(DB_FILE))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            root,
            conn: Mutex::new(conn),
            catalog: Catalog::Nalanda,
            backup_dir: None,
        })
    }

    /// Whether `root` already holds a Nalanda or calibre library.
    pub fn exists_at(root: &Path) -> bool {
        root.join(DB_FILE).is_file() || root.join(calibre::DB_FILE).is_file()
    }

    /// Runs `f` in a transaction on a calibre catalog, after making sure the
    /// catalog has been backed up once.
    fn calibre_write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        if let Some(dir) = &self.backup_dir {
            calibre::backup_once(&conn, &self.root, dir)?;
        }
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    fn add_calibre_book(&self, source: &Path) -> Result<Book> {
        let info = nalanda_formats::read_book_info(source)?;
        let (id, dir) = self.calibre_write(|tx| calibre::insert_book(tx, &info.metadata))?;
        let result = (|| {
            std::fs::create_dir_all(self.root.join(&dir))?;
            // An undecodable embedded cover shouldn't stop the import.
            if let Some(cover) = &info.cover
                && let Ok(jpeg) = calibre::cover_jpeg(&cover.data)
            {
                self.write_calibre_cover(id, &jpeg)?;
            }
            self.add_format(id, source, info.format)
        })();
        if let Err(e) = result {
            let _ = self.remove_book(id);
            return Err(e);
        }
        self.get(id)
    }

    fn add_calibre_format(&self, id: BookId, source: &Path, format: BookFormat) -> Result<()> {
        let book = self.get(id)?;
        let dir = self.book_path(id)?;
        std::fs::create_dir_all(&dir)?;
        let (file, name) = calibre::format_file_name(&book.metadata, format);
        let dest = dir.join(&file);
        if source != dest {
            std::fs::copy(source, &dest)?;
        }
        let size = std::fs::metadata(&dest)?.len();
        let previous =
            self.calibre_write(|tx| calibre::record_format(tx, id, format, &name, size))?;
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(dir.join(previous));
        }
        Ok(())
    }

    fn write_calibre_cover(&self, id: BookId, jpeg: &[u8]) -> Result<()> {
        let dir = self.book_path(id)?;
        std::fs::write(dir.join("cover.jpg"), jpeg)?;
        self.calibre_write(|tx| {
            tx.execute("UPDATE books SET has_cover = 1 WHERE id = ?1", [id])?;
            calibre::touch(tx, id)
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Imports an ebook file: reads its metadata and cover and copies it into
    /// the library. The source file is left untouched.
    pub fn add_book(&self, source: &Path) -> Result<Book> {
        if self.catalog == Catalog::Calibre {
            return self.add_calibre_book(source);
        }
        let info = nalanda_formats::read_book_info(source)?;
        let now = Utc::now();
        let id = {
            let conn = self.conn();
            let m = &info.metadata;
            conn.execute(
                "INSERT INTO books (title, sort_title, authors, author_sort, series, series_index,
                    publisher, language, published, description, tags, identifiers, path,
                    added_at, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, '', ?13, ?13)",
                params![
                    m.title,
                    sort_title(&m.title),
                    json(&m.authors),
                    m.author_sort(),
                    m.series,
                    m.series_index,
                    m.publisher,
                    m.language,
                    m.published,
                    m.description,
                    json(&m.tags),
                    json(&m.identifiers),
                    now.to_rfc3339(),
                ],
            )?;
            conn.last_insert_rowid()
        };

        let result = (|| {
            let dir = book_dir(&info.metadata, id);
            std::fs::create_dir_all(self.root.join(&dir))?;
            let cover_name = match &info.cover {
                Some(cover) => {
                    let name = format!("cover.{}", cover.extension());
                    std::fs::write(self.root.join(&dir).join(&name), &cover.data)?;
                    Some(name)
                }
                None => None,
            };
            self.conn().execute(
                "UPDATE books SET path = ?1, cover = ?2 WHERE id = ?3",
                params![dir.to_string_lossy(), cover_name, id],
            )?;
            self.add_format(id, source, info.format)
        })();
        if let Err(e) = result {
            // Leave no half-imported book behind.
            let _ = self.remove_book(id);
            return Err(e);
        }
        self.get(id)
    }

    /// Copies `source` into the book's directory as its `format` file,
    /// replacing any existing file of that format.
    pub fn add_format(&self, id: BookId, source: &Path, format: BookFormat) -> Result<()> {
        if self.catalog == Catalog::Calibre {
            return self.add_calibre_format(id, source, format);
        }
        let book = self.get(id)?;
        let dir = self.book_path(id)?;
        let file_name = format!(
            "{}.{}",
            sanitize(&format!(
                "{} - {}",
                book.metadata.title,
                book.metadata.authors_display()
            )),
            format.extension()
        );
        let dest = dir.join(&file_name);
        if source != dest {
            std::fs::copy(source, &dest)?;
        }
        let size = std::fs::metadata(&dest)?.len();
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO formats (book_id, format, file_name, size) VALUES (?1, ?2, ?3, ?4)",
            params![id, format.extension(), file_name, size as i64],
        )?;
        conn.execute(
            "UPDATE books SET modified_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn get(&self, id: BookId) -> Result<Book> {
        let conn = self.conn();
        if self.catalog == Catalog::Calibre {
            return calibre::get(&conn, id)?.ok_or(Error::NotFound(id));
        }
        let mut book = conn
            .query_row(&format!("{SELECT_BOOK} WHERE id = ?1"), [id], row_to_book)
            .optional()?
            .ok_or(Error::NotFound(id))?;
        book.formats = load_formats(&conn, id)?;
        Ok(book)
    }

    /// Lists books, optionally filtered by a case-insensitive search over
    /// title, authors, series and tags.
    pub fn list(&self, query: Option<&str>, sort: SortBy) -> Result<Vec<Book>> {
        let order = match sort {
            SortBy::RecentlyAdded => "added_at DESC, id DESC",
            SortBy::Title => "sort_title COLLATE NOCASE, id",
            SortBy::Author => "author_sort COLLATE NOCASE, series COLLATE NOCASE, series_index, id",
        };
        let terms: Vec<String> = query
            .unwrap_or_default()
            .split_whitespace()
            .map(|t| {
                format!(
                    "%{}%",
                    t.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                )
            })
            .collect();
        let conn = self.conn();
        if self.catalog == Catalog::Calibre {
            return calibre::list(&conn, &terms, sort);
        }
        let mut sql = SELECT_BOOK.to_owned();
        for (i, _) in terms.iter().enumerate() {
            sql.push_str(if i == 0 { " WHERE " } else { " AND " });
            sql.push_str(&format!(
                "(title LIKE ?{n} ESCAPE '\\' OR authors LIKE ?{n} ESCAPE '\\' \
                 OR series LIKE ?{n} ESCAPE '\\' OR tags LIKE ?{n} ESCAPE '\\')",
                n = i + 1
            ));
        }
        sql.push_str(&format!(" ORDER BY {order}"));
        let mut stmt = conn.prepare(&sql)?;
        let mut books = stmt
            .query_map(rusqlite::params_from_iter(terms.iter()), row_to_book)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for book in &mut books {
            book.formats = load_formats(&conn, book.id)?;
        }
        Ok(books)
    }

    /// Replaces a book's metadata. Files keep their names; only the catalog changes.
    pub fn update_metadata(&self, id: BookId, m: &Metadata) -> Result<Book> {
        if self.catalog == Catalog::Calibre {
            self.calibre_write(|tx| calibre::write_metadata(tx, id, m))?;
            return self.get(id);
        }
        let changed = self.conn().execute(
            "UPDATE books SET title = ?1, sort_title = ?2, authors = ?3, author_sort = ?4,
                series = ?5, series_index = ?6, publisher = ?7, language = ?8, published = ?9,
                description = ?10, tags = ?11, identifiers = ?12, modified_at = ?13
             WHERE id = ?14",
            params![
                m.title,
                sort_title(&m.title),
                json(&m.authors),
                m.author_sort(),
                m.series,
                m.series_index,
                m.publisher,
                m.language,
                m.published,
                m.description,
                json(&m.tags),
                json(&m.identifiers),
                Utc::now().to_rfc3339(),
                id,
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        self.get(id)
    }

    /// Deletes a book from the catalog and removes its directory.
    pub fn remove_book(&self, id: BookId) -> Result<()> {
        let dir: Option<String> = self
            .conn()
            .query_row("SELECT path FROM books WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(dir) = dir else {
            return Err(Error::NotFound(id));
        };
        if self.catalog == Catalog::Calibre {
            self.calibre_write(|tx| {
                // Calibre's delete trigger clears the link, data and comment rows.
                tx.execute("DELETE FROM books WHERE id = ?1", [id])?;
                tx.execute("DELETE FROM metadata_dirtied WHERE book = ?1", [id])?;
                calibre::remove_unused_items(tx)
            })?;
        } else {
            self.conn()
                .execute("DELETE FROM books WHERE id = ?1", [id])?;
        }
        if !dir.is_empty() {
            let path = self.root.join(&dir);
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
            // Drop the author directory once its last book is gone.
            if let Some(parent) = path.parent()
                && parent != self.root
            {
                let _ = std::fs::remove_dir(parent);
            }
        }
        Ok(())
    }

    /// Absolute path of the book's directory.
    pub fn book_path(&self, id: BookId) -> Result<PathBuf> {
        if self.catalog == Catalog::Calibre {
            let dir = calibre::book_dir(&self.conn(), id)?.ok_or(Error::NotFound(id))?;
            return Ok(self.root.join(dir));
        }
        let dir: String = self
            .conn()
            .query_row("SELECT path FROM books WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound(id))?;
        Ok(self.root.join(dir))
    }

    pub fn format_path(&self, id: BookId, format: BookFormat) -> Result<PathBuf> {
        if self.catalog == Catalog::Calibre {
            let file = calibre::format_file(&self.conn(), id, format)?
                .ok_or(Error::MissingFormat(id, format))?;
            return Ok(self.root.join(file));
        }
        let file: Option<String> = self
            .conn()
            .query_row(
                "SELECT file_name FROM formats WHERE book_id = ?1 AND format = ?2",
                params![id, format.extension()],
                |r| r.get(0),
            )
            .optional()?;
        let file = file.ok_or(Error::MissingFormat(id, format))?;
        Ok(self.book_path(id)?.join(file))
    }

    pub fn cover_path(&self, id: BookId) -> Result<Option<PathBuf>> {
        if self.catalog == Catalog::Calibre {
            let cover = calibre::cover_file(&self.conn(), id)?.ok_or(Error::NotFound(id))?;
            return Ok(cover.map(|c| self.root.join(c)));
        }
        let row: Option<(String, Option<String>)> = self
            .conn()
            .query_row("SELECT path, cover FROM books WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let (dir, cover) = row.ok_or(Error::NotFound(id))?;
        Ok(cover.map(|c| self.root.join(dir).join(c)))
    }

    /// Replaces the cover image with `data` (stored as `cover.<ext>`).
    pub fn set_cover(&self, id: BookId, data: &[u8], extension: &str) -> Result<()> {
        if self.catalog == Catalog::Calibre {
            return self.write_calibre_cover(id, &calibre::cover_jpeg(data)?);
        }
        let dir = self.book_path(id)?;
        if let Some(old) = self.cover_path(id)? {
            let _ = std::fs::remove_file(old);
        }
        let name = format!("cover.{extension}");
        std::fs::write(dir.join(&name), data)?;
        self.conn().execute(
            "UPDATE books SET cover = ?1, modified_at = ?2 WHERE id = ?3",
            params![name, Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }
}

const SELECT_BOOK: &str = "SELECT id, title, authors, series, series_index, publisher, language,
    published, description, tags, identifiers, cover, added_at, modified_at FROM books";

fn row_to_book(r: &rusqlite::Row<'_>) -> rusqlite::Result<Book> {
    let parse_json = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    let date = |i: usize| -> rusqlite::Result<DateTime<Utc>> {
        let s: String = r.get(i)?;
        Ok(DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_default())
    };
    Ok(Book {
        id: r.get(0)?,
        metadata: Metadata {
            title: r.get(1)?,
            authors: serde_json::from_str(&parse_json(2)?).unwrap_or_default(),
            series: r.get(3)?,
            series_index: r.get(4)?,
            publisher: r.get(5)?,
            language: r.get(6)?,
            published: r.get(7)?,
            description: r.get(8)?,
            tags: serde_json::from_str(&parse_json(9)?).unwrap_or_default(),
            identifiers: serde_json::from_str(&parse_json(10)?).unwrap_or_default(),
        },
        formats: Vec::new(),
        has_cover: r.get::<_, Option<String>>(11)?.is_some(),
        added_at: date(12)?,
        modified_at: date(13)?,
    })
}

fn load_formats(conn: &Connection, id: BookId) -> rusqlite::Result<Vec<FormatFile>> {
    let mut stmt =
        conn.prepare_cached("SELECT format, size FROM formats WHERE book_id = ?1 ORDER BY format")?;
    stmt.query_map([id], |r| {
        let ext: String = r.get(0)?;
        Ok((ext, r.get::<_, i64>(1)?))
    })?
    .filter_map(|row| match row {
        Ok((ext, size)) => ext.parse().ok().map(|format| {
            Ok(FormatFile {
                format,
                size: size as u64,
            })
        }),
        Err(e) => Some(Err(e)),
    })
    .collect()
}

fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
}

/// "The Hobbit" → "Hobbit, The", matching calibre's title sort.
fn sort_title(title: &str) -> String {
    for article in ["The ", "A ", "An "] {
        if let Some(rest) = title.strip_prefix(article)
            && !rest.is_empty()
        {
            return format!("{rest}, {}", article.trim_end());
        }
    }
    title.to_owned()
}

fn book_dir(m: &Metadata, id: BookId) -> PathBuf {
    let author = m.authors.first().map(String::as_str).unwrap_or("Unknown");
    let title = if m.title.trim().is_empty() {
        "Untitled"
    } else {
        &m.title
    };
    PathBuf::from(sanitize(author)).join(format!("{} ({id})", sanitize(title)))
}

/// Makes a string safe as a single path component on every OS.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim();
    let mut out: String = cleaned.chars().take(80).collect();
    if out.is_empty() || out.starts_with('.') {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests;

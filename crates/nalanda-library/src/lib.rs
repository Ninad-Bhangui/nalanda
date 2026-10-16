//! A Nalanda library is a calibre library: a folder holding calibre's
//! catalog, `metadata.db`, and one folder per book.
//!
//! ```text
//! <root>/metadata.db                      calibre's catalog (SQLite)
//! <root>/<Author>/<Title> (<id>)/         one folder per book
//!     <Title> - <Author>.<ext>            one file per format
//!     cover.jpg
//!     metadata.opf                        calibre's per-book backup
//! ```
//!
//! Existing calibre libraries open in place, and calibre can open libraries
//! Nalanda creates. Libraries from Nalanda 0.0.1 (with `nalanda.db`) are
//! converted on first open; see [`legacy`]. [`Library`] is `Sync` and is
//! shared by the desktop app and the server.

mod calibre;
mod legacy;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nalanda_core::{Book, BookFormat, BookId, Metadata};
use rusqlite::{Connection, Transaction};

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

/// Sort orders offered by [`Library::list`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortBy {
    #[default]
    RecentlyAdded,
    Title,
    Author,
}

pub struct Library {
    root: PathBuf,
    conn: Mutex<Connection>,
    /// Where to back up the catalog before Nalanda's first write, if anywhere.
    backup_dir: Option<PathBuf>,
}

impl Library {
    /// Opens the library at `root`. An existing calibre library is opened in
    /// place; a Nalanda 0.0.1 library is converted; otherwise a new, empty
    /// calibre library is created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_backup_dir(root.into(), calibre::default_backup_dir())
    }

    fn open_with_backup_dir(root: PathBuf, mut backup_dir: Option<PathBuf>) -> Result<Self> {
        let db = root.join(calibre::DB_FILE);
        if !db.is_file() {
            std::fs::create_dir_all(&root)?;
            if root.join(legacy::DB_FILE).is_file() {
                legacy::convert(&root, backup_dir.as_deref())?;
            } else {
                calibre::create(&db)?;
            }
            // Nothing in this catalog predates Nalanda, so there's nothing to protect.
            backup_dir = None;
        }
        Ok(Self {
            conn: Mutex::new(calibre::open(&db)?),
            root,
            backup_dir,
        })
    }

    /// Whether `root` already holds a library (calibre's or Nalanda 0.0.1's).
    pub fn exists_at(root: &Path) -> bool {
        root.join(calibre::DB_FILE).is_file() || root.join(legacy::DB_FILE).is_file()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` in a transaction, after backing up a pre-existing catalog once.
    fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        if let Some(dir) = &self.backup_dir {
            calibre::backup_once(&conn, &self.root, dir)?;
        }
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Imports an ebook file: reads its metadata and cover and copies it into
    /// the library. The source file is left untouched.
    pub fn add_book(&self, source: &Path) -> Result<Book> {
        let info = nalanda_formats::read_book_info(source)?;
        let (id, dir) = self.write(|tx| calibre::insert_book(tx, &info.metadata))?;
        let result = (|| {
            std::fs::create_dir_all(self.root.join(&dir))?;
            // An undecodable embedded cover shouldn't stop the import.
            if let Some(cover) = &info.cover
                && let Ok(jpeg) = calibre::cover_jpeg(&cover.data)
            {
                self.write_cover(id, &jpeg)?;
            }
            self.add_format(id, source, info.format)
        })();
        if let Err(e) = result {
            // Leave no half-imported book behind.
            let _ = self.remove_book(id);
            return Err(e);
        }
        self.get(id)
    }

    /// Copies `source` into the book's folder as its `format` file,
    /// replacing any existing file of that format.
    pub fn add_format(&self, id: BookId, source: &Path, format: BookFormat) -> Result<()> {
        let book = self.get(id)?;
        let dir = self.book_path(id)?;
        std::fs::create_dir_all(&dir)?;
        let (file, name) = calibre::format_file_name(&book.metadata, format);
        let dest = dir.join(&file);
        if source != dest {
            std::fs::copy(source, &dest)?;
        }
        let size = std::fs::metadata(&dest)?.len();
        let previous = self.write(|tx| calibre::record_format(tx, id, format, &name, size))?;
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(dir.join(previous));
        }
        Ok(())
    }

    pub fn get(&self, id: BookId) -> Result<Book> {
        calibre::get(&self.conn(), id)?.ok_or(Error::NotFound(id))
    }

    /// Lists books, optionally filtered by a case-insensitive search over
    /// title, authors, series and tags.
    pub fn list(&self, query: Option<&str>, sort: SortBy) -> Result<Vec<Book>> {
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
        calibre::list(&self.conn(), &terms, sort)
    }

    /// Replaces a book's metadata. Files keep their names; calibre renames
    /// them the next time it edits the book.
    pub fn update_metadata(&self, id: BookId, m: &Metadata) -> Result<Book> {
        self.write(|tx| calibre::write_metadata(tx, id, m))?;
        self.get(id)
    }

    /// Deletes a book from the catalog and removes its folder.
    pub fn remove_book(&self, id: BookId) -> Result<()> {
        let dir = self.book_path(id)?;
        self.write(|tx| {
            // Calibre's delete trigger clears the link, format and comment rows.
            tx.execute("DELETE FROM books WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM metadata_dirtied WHERE book = ?1", [id])?;
            calibre::remove_unused_items(tx)
        })?;
        if dir != self.root {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            // Drop the author folder once its last book is gone.
            if let Some(parent) = dir.parent()
                && parent != self.root
            {
                let _ = std::fs::remove_dir(parent);
            }
        }
        Ok(())
    }

    /// Absolute path of the book's folder.
    pub fn book_path(&self, id: BookId) -> Result<PathBuf> {
        let dir = calibre::book_dir(&self.conn(), id)?.ok_or(Error::NotFound(id))?;
        Ok(self.root.join(dir))
    }

    pub fn format_path(&self, id: BookId, format: BookFormat) -> Result<PathBuf> {
        let file = calibre::format_file(&self.conn(), id, format)?
            .ok_or(Error::MissingFormat(id, format))?;
        Ok(self.root.join(file))
    }

    pub fn cover_path(&self, id: BookId) -> Result<Option<PathBuf>> {
        let cover = calibre::cover_file(&self.conn(), id)?.ok_or(Error::NotFound(id))?;
        Ok(cover.map(|c| self.root.join(c)))
    }

    /// Replaces the cover image; any format the `image` crate reads is
    /// stored as `cover.jpg`, as calibre expects.
    pub fn set_cover(&self, id: BookId, data: &[u8]) -> Result<()> {
        self.write_cover(id, &calibre::cover_jpeg(data)?)
    }

    fn write_cover(&self, id: BookId, jpeg: &[u8]) -> Result<()> {
        let dir = self.book_path(id)?;
        std::fs::write(dir.join("cover.jpg"), jpeg)?;
        self.write(|tx| {
            tx.execute("UPDATE books SET has_cover = 1 WHERE id = ?1", [id])?;
            calibre::touch(tx, id)
        })
    }
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

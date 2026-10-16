//! Calibre's catalog, `metadata.db`.
//!
//! Calibre normalizes metadata into one table per field (`authors`, `tags`,
//! `series`, …) joined to `books` through `books_*_link` tables. Formats live in
//! `data`, where a file is `<books.path>/<data.name>.<lowercase data.format>`,
//! and a cover is always `<books.path>/cover.jpg` when `books.has_cover` is set.
//! Tables Nalanda doesn't model (custom columns, annotations, …) are left alone.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use nalanda_core::{Book, BookFormat, BookId, FormatFile, Metadata};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::{Error, Result, SortBy};

pub(crate) const DB_FILE: &str = "metadata.db";

/// Creates an empty catalog the way calibre 6.13 does. Newer calibre versions
/// upgrade it when they open it.
pub(crate) fn create(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(include_str!("calibre_schema.sql"))?;
    conn.execute(
        "INSERT INTO library_id (uuid) VALUES (?1)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    Ok(())
}

/// Opens an existing calibre catalog for reading and writing.
pub(crate) fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Calibre may be writing at the same moment; wait for it rather than fail.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Calibre's triggers call these when books and series are inserted or renamed.
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("title_sort", 1, flags, |ctx| {
        Ok(title_sort(&ctx.get::<String>(0)?))
    })?;
    conn.create_scalar_function("uuid4", 0, FunctionFlags::SQLITE_UTF8, |_| {
        Ok(uuid::Uuid::new_v4().to_string())
    })?;
    Ok(conn)
}

/// Calibre's name for a format, as stored in `data.format`.
pub(crate) fn format_name(format: BookFormat) -> String {
    match format {
        BookFormat::Kepub => "KEPUB".to_owned(),
        other => other.extension().to_ascii_uppercase(),
    }
}

/// Every book column Nalanda reads, with the linked fields folded in as JSON so
/// a whole listing is one query.
const SELECT_BOOK: &str = "SELECT b.id, b.title, b.series_index, b.pubdate, b.has_cover,
    b.timestamp, b.last_modified,
    (SELECT json_group_array(name) FROM (
        SELECT a.name FROM books_authors_link l JOIN authors a ON a.id = l.author
        WHERE l.book = b.id ORDER BY l.id)),
    (SELECT json_group_array(name) FROM (
        SELECT t.name FROM books_tags_link l JOIN tags t ON t.id = l.tag
        WHERE l.book = b.id ORDER BY t.name COLLATE NOCASE)),
    (SELECT s.name FROM books_series_link l JOIN series s ON s.id = l.series WHERE l.book = b.id),
    (SELECT p.name FROM books_publishers_link l JOIN publishers p ON p.id = l.publisher
        WHERE l.book = b.id),
    (SELECT g.lang_code FROM books_languages_link l JOIN languages g ON g.id = l.lang_code
        WHERE l.book = b.id ORDER BY l.item_order, l.id LIMIT 1),
    (SELECT text FROM comments WHERE book = b.id),
    (SELECT json_group_object(type, val) FROM identifiers WHERE book = b.id),
    (SELECT json_group_array(json_array(format, uncompressed_size)) FROM (
        SELECT format, uncompressed_size FROM data WHERE book = b.id ORDER BY format))
    FROM books b";

pub(crate) fn get(conn: &Connection, id: BookId) -> Result<Option<Book>> {
    Ok(conn
        .prepare_cached(&format!("{SELECT_BOOK} WHERE b.id = ?1"))?
        .query_row([id], row_to_book)
        .optional()?)
}

pub(crate) fn list(conn: &Connection, terms: &[String], sort: SortBy) -> Result<Vec<Book>> {
    let mut sql = SELECT_BOOK.to_owned();
    for (i, _) in terms.iter().enumerate() {
        sql.push_str(if i == 0 { " WHERE " } else { " AND " });
        sql.push_str(&format!(
            "(b.title LIKE ?{n} ESCAPE '\\'
              OR EXISTS (SELECT 1 FROM books_authors_link l JOIN authors a ON a.id = l.author
                         WHERE l.book = b.id AND a.name LIKE ?{n} ESCAPE '\\')
              OR EXISTS (SELECT 1 FROM books_series_link l JOIN series s ON s.id = l.series
                         WHERE l.book = b.id AND s.name LIKE ?{n} ESCAPE '\\')
              OR EXISTS (SELECT 1 FROM books_tags_link l JOIN tags t ON t.id = l.tag
                         WHERE l.book = b.id AND t.name LIKE ?{n} ESCAPE '\\'))",
            n = i + 1
        ));
    }
    sql.push_str(match sort {
        SortBy::RecentlyAdded => " ORDER BY b.timestamp DESC, b.id DESC",
        SortBy::Title => " ORDER BY b.sort COLLATE NOCASE, b.id",
        SortBy::Author => {
            " ORDER BY b.author_sort COLLATE NOCASE,
               (SELECT s.sort FROM books_series_link l JOIN series s ON s.id = l.series
                WHERE l.book = b.id) COLLATE NOCASE,
               b.series_index, b.id"
        }
    });
    let mut stmt = conn.prepare(&sql)?;
    let books = stmt
        .query_map(rusqlite::params_from_iter(terms), row_to_book)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(books)
}

/// The book's directory relative to the library root.
pub(crate) fn book_dir(conn: &Connection, id: BookId) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT path FROM books WHERE id = ?1", [id], |r| r.get(0))
        .optional()?)
}

/// The file of `format`, relative to the library root.
pub(crate) fn format_file(
    conn: &Connection,
    id: BookId,
    format: BookFormat,
) -> Result<Option<String>> {
    let format = format_name(format);
    Ok(conn
        .query_row(
            "SELECT b.path, d.name FROM data d JOIN books b ON b.id = d.book
             WHERE d.book = ?1 AND d.format = ?2",
            params![id, format],
            |r| {
                let dir: String = r.get(0)?;
                let name: String = r.get(1)?;
                Ok(format!("{dir}/{name}.{}", format.to_ascii_lowercase()))
            },
        )
        .optional()?)
}

/// `Some(cover file)` for an existing book, relative to the library root, or
/// `Some(None)` when it has no cover.
pub(crate) fn cover_file(conn: &Connection, id: BookId) -> Result<Option<Option<String>>> {
    Ok(conn
        .query_row(
            "SELECT path, has_cover FROM books WHERE id = ?1",
            [id],
            |r| {
                let dir: String = r.get(0)?;
                let has_cover: bool = r.get(1)?;
                Ok(has_cover.then(|| format!("{dir}/cover.jpg")))
            },
        )
        .optional()?)
}

fn row_to_book(r: &rusqlite::Row<'_>) -> rusqlite::Result<Book> {
    let json = |i: usize| -> rusqlite::Result<String> {
        Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
    };
    let series: Option<String> = r.get(9)?;
    let formats: Vec<(String, i64)> = serde_json::from_str(&json(14)?).unwrap_or_default();
    Ok(Book {
        id: r.get(0)?,
        metadata: Metadata {
            title: r.get(1)?,
            authors: serde_json::from_str(&json(7)?).unwrap_or_default(),
            series_index: series.as_ref().and(r.get::<_, Option<f64>>(2)?),
            series,
            publisher: r.get(10)?,
            language: r.get(11)?,
            published: r
                .get::<_, Option<String>>(3)?
                .as_deref()
                .and_then(published_date),
            description: r.get(12)?,
            tags: serde_json::from_str(&json(8)?).unwrap_or_default(),
            identifiers: serde_json::from_str(&json(13)?).unwrap_or_default(),
        },
        formats: formats
            .into_iter()
            .filter_map(|(format, size)| {
                // Formats Nalanda doesn't know (LIT, DJVU, ORIGINAL_EPUB, …) stay hidden.
                Some(FormatFile {
                    format: format.parse().ok()?,
                    size: size.max(0) as u64,
                })
            })
            .collect(),
        has_cover: r.get(4)?,
        added_at: r
            .get::<_, Option<String>>(5)?
            .as_deref()
            .and_then(parse_timestamp)
            .unwrap_or_default(),
        modified_at: r
            .get::<_, Option<String>>(6)?
            .as_deref()
            .and_then(parse_timestamp)
            .unwrap_or_default(),
    })
}

/// Parses calibre's timestamps, e.g. `2024-03-01 10:20:30.123456+00:00`.
pub(crate) fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z")
        .or_else(|_| DateTime::parse_from_rfc3339(s))
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|d| d.and_utc())
        })
}

/// The publication date as `YYYY-MM-DD`; calibre's "undefined" date (year 101) is `None`.
fn published_date(raw: &str) -> Option<String> {
    let date = parse_timestamp(raw)?.date_naive();
    (date.year() > 101).then(|| date.format("%Y-%m-%d").to_string())
}

/// Calibre's default title sort: drops a leading quote and moves an English
/// article to the end, so "The Hobbit" sorts as "Hobbit, The".
pub(crate) fn title_sort(title: &str) -> String {
    let is_quote = |c: char| {
        matches!(
            c,
            '\'' | '"' | '\u{2018}'..='\u{201d}' | '\u{2032}' | '\u{2033}'
        )
    };
    let strip_quote = |s: &str| -> String {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if is_quote(c) => chars.as_str().to_owned(),
            _ => s.to_owned(),
        }
    };
    let title = strip_quote(title.trim());
    for article in ["A", "The", "An"] {
        let Some(head) = title.get(..article.len()) else {
            continue;
        };
        let rest = &title[article.len()..];
        if head.eq_ignore_ascii_case(article) && rest.starts_with(char::is_whitespace) {
            return strip_quote(&format!("{}, {head}", rest.trim_start()))
                .trim()
                .to_owned();
        }
    }
    title.trim().to_owned()
}

// ----- writing -------------------------------------------------------------------

/// Calibre's maximum length of a path component (`PATH_LIMIT` in calibre's backend).
const PATH_LIMIT: usize = if cfg!(windows) { 40 } else { 100 };

/// Calibre's `ascii_filename`: transliterates to ASCII and replaces characters
/// that are invalid in file names on any OS.
fn ascii_filename(s: &str) -> String {
    let ascii = deunicode::deunicode(s);
    let replaced: String = ascii
        .chars()
        .map(|c| match c {
            '\\' | '|' | '?' | '*' | '<' | '"' | ':' | '>' | '+' | '/' => '_',
            c if (c as u32) < 32 => '_',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect();
    let mut name = replaced.trim().to_owned();
    if !name.is_empty() && name.chars().all(|c| c == '.') {
        name = "_".to_owned();
    }
    name = name.replace("..", "_");
    if name.ends_with(['.', ' ']) {
        name.pop();
        name.push('_');
    }
    if name.starts_with('.') {
        name.replace_range(..1, "_");
    }
    name
}

fn truncate(s: &str, max: usize) -> &str {
    // `ascii_filename` output is ASCII, so byte and char indices agree.
    &s[..s.len().min(max)]
}

/// Calibre's `construct_path_name`: `<Author>/<Title> (<id>)`.
pub(crate) fn book_path_name(id: BookId, m: &Metadata) -> String {
    let suffix = format!(" ({id})");
    let limit = PATH_LIMIT - suffix.len() / 2 - 2;
    let author = ascii_filename(first_author(m));
    let mut author = truncate(&author, limit)
        .trim_end_matches([' ', '.'])
        .to_owned();
    if author.is_empty() {
        author = "Unknown".to_owned();
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&author.to_ascii_uppercase().as_str()) {
        author.push('w');
    }
    format!("{author}/{}{suffix}", file_title(&m.title, limit))
}

/// Calibre's `construct_file_name`: `<Title> - <Author>`, without the extension.
fn file_name(m: &Metadata, extension: &str) -> String {
    let ext_len = (extension.len() + 1).max(14);
    let limit = if cfg!(windows) {
        PATH_LIMIT - ext_len / 2 - 2
    } else {
        (PATH_LIMIT - ext_len - 2) / 2
    };
    let author = ascii_filename(first_author(m));
    format!(
        "{} - {}",
        file_title(&m.title, limit),
        truncate(&author, limit)
    )
}

fn file_title(title: &str, limit: usize) -> String {
    let title = ascii_filename(title.trim_start());
    match truncate(&title, limit).trim_end() {
        "" => "Unknown".to_owned(),
        t => t.to_owned(),
    }
}

fn first_author(m: &Metadata) -> &str {
    m.authors.first().map(String::as_str).unwrap_or("Unknown")
}

/// A timestamp the way calibre writes it.
fn timestamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string()
}

/// Calibre's "no date" value.
const UNDEFINED_DATE: &str = "0101-01-01 00:00:00+00:00";

/// Stores a publication date (`YYYY`, `YYYY-MM`, `YYYY-MM-DD` or a full
/// timestamp) at noon UTC, so it shows as the same day in every time zone.
/// A missing month or day becomes the 15th, as in calibre.
fn pubdate(published: Option<&str>) -> String {
    let Some(raw) = published.map(str::trim).filter(|s| !s.is_empty()) else {
        return UNDEFINED_DATE.to_owned();
    };
    let mut parts = raw.get(..raw.len().min(10)).unwrap_or(raw).split('-');
    let year = parts.next().and_then(|y| y.parse::<i32>().ok());
    let month = parts.next().map_or(Some(1), |m| m.parse::<u32>().ok());
    let day = parts.next().map_or(Some(15), |d| d.parse::<u32>().ok());
    match (year, month, day) {
        (Some(y), Some(m), Some(d)) => chrono::NaiveDate::from_ymd_opt(y, m, d)
            .map(|date| format!("{} 12:00:00+00:00", date.format("%Y-%m-%d")))
            .unwrap_or_else(|| UNDEFINED_DATE.to_owned()),
        _ => UNDEFINED_DATE.to_owned(),
    }
}

/// Calibre stores ISO 639-3 codes ("eng"); ebooks usually carry BCP 47 ("en-US").
fn language_code(language: &str) -> Option<String> {
    let primary = language
        .trim()
        .split(['-', '_'])
        .next()?
        .to_ascii_lowercase();
    match primary.len() {
        2 => isolang::Language::from_639_1(&primary).map(|l| l.to_639_3().to_owned()),
        3 => Some(primary),
        _ => None,
    }
}

/// Writes every field Nalanda models: the `books` row and the linked tables.
/// The caller sets `path`, formats and the cover.
pub(crate) fn write_metadata(tx: &Transaction<'_>, id: BookId, m: &Metadata) -> Result<()> {
    // Authors: reuse existing rows (and their possibly hand-edited sort), keep order.
    tx.execute("DELETE FROM books_authors_link WHERE book = ?1", [id])?;
    let mut author_sorts = Vec::new();
    let authors: Vec<&str> = if m.authors.is_empty() {
        vec!["Unknown"]
    } else {
        m.authors.iter().map(String::as_str).collect()
    };
    for name in authors {
        let existing: Option<(i64, Option<String>)> = tx
            .query_row(
                "SELECT id, sort FROM authors WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (author_id, sort) = match existing {
            Some((author_id, Some(sort))) => (author_id, sort),
            Some((author_id, None)) => (author_id, nalanda_core::author_sort(name)),
            None => {
                let sort = nalanda_core::author_sort(name);
                tx.execute(
                    "INSERT INTO authors (name, sort) VALUES (?1, ?2)",
                    params![name, sort],
                )?;
                (tx.last_insert_rowid(), sort)
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO books_authors_link (book, author) VALUES (?1, ?2)",
            params![id, author_id],
        )?;
        author_sorts.push(sort);
    }

    let changed = tx.execute(
        "UPDATE books SET title = ?1, author_sort = ?2, series_index = ?3, pubdate = ?4,
            last_modified = ?5 WHERE id = ?6",
        params![
            if m.title.trim().is_empty() {
                "Unknown"
            } else {
                m.title.as_str()
            },
            author_sorts.join(" & "),
            m.series.as_ref().and(m.series_index).unwrap_or(1.0),
            pubdate(m.published.as_deref()),
            timestamp(Utc::now()),
            id,
        ],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(id));
    }

    tx.execute("DELETE FROM books_tags_link WHERE book = ?1", [id])?;
    for tag in &m.tags {
        let tag_id = item_id(tx, "tags", "name", tag)?;
        tx.execute(
            "INSERT OR IGNORE INTO books_tags_link (book, tag) VALUES (?1, ?2)",
            params![id, tag_id],
        )?;
    }

    tx.execute("DELETE FROM books_series_link WHERE book = ?1", [id])?;
    if let Some(series) = m.series.as_deref().filter(|s| !s.trim().is_empty()) {
        let series_id = item_id(tx, "series", "name", series)?;
        tx.execute(
            "INSERT INTO books_series_link (book, series) VALUES (?1, ?2)",
            params![id, series_id],
        )?;
    }

    tx.execute("DELETE FROM books_publishers_link WHERE book = ?1", [id])?;
    if let Some(publisher) = m.publisher.as_deref().filter(|s| !s.trim().is_empty()) {
        let publisher_id = item_id(tx, "publishers", "name", publisher)?;
        tx.execute(
            "INSERT INTO books_publishers_link (book, publisher) VALUES (?1, ?2)",
            params![id, publisher_id],
        )?;
    }

    // Nalanda models one language; keep any further ones calibre has.
    if let Some(code) = m.language.as_deref().and_then(language_code) {
        let lang_id = item_id(tx, "languages", "lang_code", &code)?;
        tx.execute(
            "DELETE FROM books_languages_link WHERE book = ?1 AND lang_code = ?2",
            params![id, lang_id],
        )?;
        tx.execute(
            "UPDATE books_languages_link SET item_order = item_order + 1 WHERE book = ?1",
            [id],
        )?;
        tx.execute(
            "INSERT INTO books_languages_link (book, lang_code, item_order) VALUES (?1, ?2, 0)",
            params![id, lang_id],
        )?;
    } else if m.language.is_none() {
        tx.execute("DELETE FROM books_languages_link WHERE book = ?1", [id])?;
    }

    match m.description.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(text) => tx.execute(
            "INSERT INTO comments (book, text) VALUES (?1, ?2)
             ON CONFLICT(book) DO UPDATE SET text = excluded.text",
            params![id, text],
        )?,
        None => tx.execute("DELETE FROM comments WHERE book = ?1", [id])?,
    };

    tx.execute("DELETE FROM identifiers WHERE book = ?1", [id])?;
    for (kind, value) in &m.identifiers {
        tx.execute(
            "INSERT INTO identifiers (book, type, val) VALUES (?1, ?2, ?3)",
            params![id, kind.to_lowercase(), value],
        )?;
    }

    remove_unused_items(tx)?;
    mark_dirty(tx, id)
}

/// Id of the row in `table` whose `column` is `value`, created if missing.
/// Calibre matches these case-insensitively (`COLLATE NOCASE`).
fn item_id(tx: &Transaction<'_>, table: &str, column: &str, value: &str) -> Result<i64> {
    let value = value.trim();
    if let Some(id) = tx
        .query_row(
            &format!("SELECT id FROM {table} WHERE {column} = ?1"),
            [value],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    tx.execute(
        &format!("INSERT INTO {table} ({column}) VALUES (?1)"),
        [value],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Deletes authors, tags, series, publishers and languages no book uses any
/// more, as calibre does after every edit.
pub(crate) fn remove_unused_items(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DELETE FROM authors WHERE id NOT IN (SELECT author FROM books_authors_link);
         DELETE FROM tags WHERE id NOT IN (SELECT tag FROM books_tags_link);
         DELETE FROM series WHERE id NOT IN (SELECT series FROM books_series_link);
         DELETE FROM publishers WHERE id NOT IN (SELECT publisher FROM books_publishers_link);
         DELETE FROM languages WHERE id NOT IN (SELECT lang_code FROM books_languages_link);",
    )?;
    Ok(())
}

/// Asks calibre to rewrite the book's `metadata.opf` backup the next time it runs.
pub(crate) fn mark_dirty(conn: &Connection, id: BookId) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO metadata_dirtied (book) VALUES (?1)",
        [id],
    )?;
    Ok(())
}

pub(crate) fn touch(conn: &Connection, id: BookId) -> Result<()> {
    conn.execute(
        "UPDATE books SET last_modified = ?1 WHERE id = ?2",
        params![timestamp(Utc::now()), id],
    )?;
    mark_dirty(conn, id)
}

/// Inserts a book row with its metadata and calibre's folder name; returns
/// the id and the folder relative to the library root.
pub(crate) fn insert_book(tx: &Transaction<'_>, m: &Metadata) -> Result<(BookId, String)> {
    let now = timestamp(Utc::now());
    tx.execute(
        "INSERT INTO books (title, timestamp, last_modified, path, has_cover)
         VALUES (?1, ?2, ?2, '', 0)",
        params![m.title, now],
    )?;
    let id = tx.last_insert_rowid();
    write_metadata(tx, id, m)?;
    let dir = book_path_name(id, m);
    tx.execute("UPDATE books SET path = ?1 WHERE id = ?2", params![dir, id])?;
    Ok((id, dir))
}

/// Inserts a book that already has an id and a folder (when converting a
/// Nalanda 0.0.1 library). Formats are recorded separately.
pub(crate) fn insert_existing_book(tx: &Transaction<'_>, book: &Book, dir: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO books (id, title, path, has_cover) VALUES (?1, ?2, ?3, ?4)",
        params![book.id, book.metadata.title, dir, book.has_cover],
    )?;
    write_metadata(tx, book.id, &book.metadata)
}

/// Sets when a book was added and last changed.
pub(crate) fn set_times(
    tx: &Transaction<'_>,
    id: BookId,
    added: DateTime<Utc>,
    modified: DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "UPDATE books SET timestamp = ?1, last_modified = ?2 WHERE id = ?3",
        params![timestamp(added), timestamp(modified), id],
    )?;
    Ok(())
}

/// The file a format of this book is stored as, relative to its folder.
pub(crate) fn format_file_name(m: &Metadata, format: BookFormat) -> (String, String) {
    let extension = format_name(format).to_ascii_lowercase();
    let name = file_name(m, &extension);
    (format!("{name}.{extension}"), name)
}

/// Records a format stored as `<name>.<extension>`; returns the previous file
/// name (with extension) if the format was already stored under another name.
pub(crate) fn record_format(
    tx: &Transaction<'_>,
    id: BookId,
    format: BookFormat,
    name: &str,
    size: u64,
) -> Result<Option<String>> {
    let format = format_name(format);
    let previous: Option<String> = tx
        .query_row(
            "SELECT name FROM data WHERE book = ?1 AND format = ?2",
            params![id, format],
            |r| r.get(0),
        )
        .optional()?;
    tx.execute(
        "INSERT INTO data (book, format, uncompressed_size, name) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(book, format) DO UPDATE SET
            uncompressed_size = excluded.uncompressed_size, name = excluded.name",
        params![id, format, size as i64, name],
    )?;
    touch(tx, id)?;
    Ok(previous
        .filter(|p| p != name)
        .map(|p| format!("{p}.{}", format.to_ascii_lowercase())))
}

/// Calibre always stores the cover as JPEG in `cover.jpg`.
pub(crate) fn cover_jpeg(data: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok(data.into());
    }
    let image = image::load_from_memory(data).map_err(|e| Error::Cover(e.to_string()))?;
    let mut jpeg = Vec::new();
    image
        .to_rgb8()
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| Error::Cover(e.to_string()))?;
    Ok(jpeg.into())
}

/// Where Nalanda keeps a copy of each calibre catalog as it was before
/// Nalanda first changed it: outside the library, where calibre won't see it.
pub(crate) fn default_backup_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("nalanda").join("calibre-backups"))
}

/// Snapshots the catalog into `dir` once, named after calibre's library id.
pub(crate) fn backup_once(conn: &Connection, root: &Path, dir: &Path) -> Result<()> {
    let id: Option<String> = conn
        .query_row("SELECT uuid FROM library_id", [], |r| r.get(0))
        .optional()?;
    let name = id.unwrap_or_else(|| crate::sanitize(&root.to_string_lossy()));
    let backup = dir.join(format!("{name}.db"));
    if !backup.exists() {
        std::fs::create_dir_all(dir)?;
        conn.execute("VACUUM INTO ?1", [backup.to_string_lossy()])?;
    }
    Ok(())
}

//! Calibre's catalog, `metadata.db`.
//!
//! Calibre normalizes metadata into one table per field (`authors`, `tags`,
//! `series`, …) joined to `books` through `books_*_link` tables. Formats live in
//! `data`, where a file is `<books.path>/<data.name>.<lowercase data.format>`,
//! and a cover is always `<books.path>/cover.jpg` when `books.has_cover` is set.
//! Tables Nalanda doesn't model (custom columns, annotations, …) are left alone.

use std::path::Path;

use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use nalanda_core::{Book, BookFormat, BookId, FormatFile, Metadata};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{Result, SortBy};

pub(crate) const DB_FILE: &str = "metadata.db";

/// Opens an existing calibre catalog without the ability to change it.
pub(crate) fn open_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Calibre may be writing at the same moment; wait for it rather than fail.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
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
#[cfg(test)]
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

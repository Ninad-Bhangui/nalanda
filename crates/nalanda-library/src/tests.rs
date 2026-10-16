use super::*;
use nalanda_formats::{Block, Chapter, Cover};

fn write_epub(dir: &Path, name: &str, title: &str, authors: &[&str], tags: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let metadata = Metadata {
        title: title.into(),
        authors: authors.iter().map(|s| s.to_string()).collect(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let chapter = Chapter {
        title: Some("One".into()),
        blocks: vec![Block::Paragraph("Hello".into())],
    };
    let cover = Cover {
        data: b"\xff\xd8fakejpeg".to_vec(),
        media_type: "image/jpeg".into(),
    };
    nalanda_formats::epub::write_epub(&path, &metadata, &[chapter], Some(&cover)).unwrap();
    path
}

#[test]
fn add_list_search_update_remove() {
    let src = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let lib = Library::open(root.path()).unwrap();

    let emma = write_epub(src.path(), "e.epub", "Emma", &["Jane Austen"], &["Classic"]);
    let hobbit = write_epub(
        src.path(),
        "h.epub",
        "The Hobbit",
        &["J.R.R. Tolkien"],
        &["Fantasy"],
    );
    let a = lib.add_book(&emma).unwrap();
    let b = lib.add_book(&hobbit).unwrap();

    // Stored with calibre-style layout and the extracted cover.
    assert_eq!(
        a.formats,
        [FormatFile {
            format: BookFormat::Epub,
            size: std::fs::metadata(&emma).unwrap().len()
        }]
    );
    assert!(a.has_cover);
    let epub_path = lib.format_path(a.id, BookFormat::Epub).unwrap();
    assert_eq!(
        epub_path.strip_prefix(root.path()).unwrap(),
        Path::new(&format!(
            "Jane Austen/Emma ({})/Emma - Jane Austen.epub",
            a.id
        ))
    );
    assert_eq!(
        std::fs::read(lib.cover_path(a.id).unwrap().unwrap()).unwrap(),
        b"\xff\xd8fakejpeg"
    );

    // Author sort uses "Last, First": "Austen" < "Tolkien" although "J.R.R." < "Jane".
    let titles = |books: Vec<Book>| {
        books
            .into_iter()
            .map(|b| b.metadata.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(lib.list(None, SortBy::Title).unwrap()),
        ["Emma", "The Hobbit"]
    );
    assert_eq!(
        titles(lib.list(None, SortBy::Author).unwrap()),
        ["Emma", "The Hobbit"]
    );
    assert_eq!(
        titles(lib.list(None, SortBy::RecentlyAdded).unwrap()),
        ["The Hobbit", "Emma"]
    );

    // Search: every term must match some field, case-insensitively; LIKE wildcards are literal.
    assert_eq!(
        titles(lib.list(Some("tolkien hob"), SortBy::Title).unwrap()),
        ["The Hobbit"]
    );
    assert_eq!(
        titles(lib.list(Some("CLASSIC"), SortBy::Title).unwrap()),
        ["Emma"]
    );
    assert!(
        lib.list(Some("austen hobbit"), SortBy::Title)
            .unwrap()
            .is_empty()
    );
    assert!(lib.list(Some("%"), SortBy::Title).unwrap().is_empty());

    let mut m = b.metadata.clone();
    m.series = Some("Middle-earth".into());
    m.series_index = Some(1.0);
    let updated = lib.update_metadata(b.id, &m).unwrap();
    assert_eq!(updated.metadata, m);
    assert_eq!(
        titles(lib.list(Some("middle"), SortBy::Title).unwrap()),
        ["The Hobbit"]
    );

    let dir = lib.book_path(a.id).unwrap();
    lib.remove_book(a.id).unwrap();
    assert!(!dir.exists());
    assert!(
        !root.path().join("Jane Austen").exists(),
        "empty author dir removed"
    );
    assert!(matches!(lib.get(a.id), Err(Error::NotFound(_))));
    assert_eq!(lib.list(None, SortBy::Title).unwrap().len(), 1);
}

#[test]
fn add_format_attaches_second_file() {
    let src = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let lib = Library::open(root.path()).unwrap();
    let book = lib
        .add_book(&write_epub(src.path(), "x.epub", "X", &["A B"], &[]))
        .unwrap();
    let txt = src.path().join("x.txt");
    std::fs::write(&txt, "hello").unwrap();
    lib.add_format(book.id, &txt, BookFormat::Txt).unwrap();
    let book = lib.get(book.id).unwrap();
    assert_eq!(
        book.formats.iter().map(|f| f.format).collect::<Vec<_>>(),
        [BookFormat::Epub, BookFormat::Txt]
    );
    assert_eq!(
        std::fs::read_to_string(lib.format_path(book.id, BookFormat::Txt).unwrap()).unwrap(),
        "hello"
    );
}

#[test]
fn failed_import_leaves_nothing_behind() {
    let src = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let lib = Library::open(root.path()).unwrap();
    let bad = src.path().join("broken.epub");
    std::fs::write(&bad, "not a zip").unwrap();
    assert!(lib.add_book(&bad).is_err());
    assert!(lib.list(None, SortBy::Title).unwrap().is_empty());
}

#[test]
fn sanitize_produces_safe_components() {
    assert_eq!(sanitize("AC/DC: Live?"), "AC_DC_ Live_");
    assert_eq!(sanitize("  ..hidden. "), "_..hidden");
    assert_eq!(sanitize(""), "_");
    assert_eq!(sort_title("The Hobbit"), "Hobbit, The");
    assert_eq!(sort_title("Theory"), "Theory");
}

/// A calibre library holding the rows calibre 6.13's `calibredb` wrote for four
/// books (see `tests_calibre_fixture.sql`). Files are not needed for reading.
fn calibre_fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("metadata.db");
    Connection::open(&db)
        .unwrap()
        .execute_batch(include_str!("calibre_schema.sql"))
        .unwrap();
    // Insert the rows with calibre's SQL functions available to its triggers.
    calibre::open(&db)
        .unwrap()
        .execute_batch(include_str!("tests_calibre_fixture.sql"))
        .unwrap();
    root
}

#[test]
fn reads_a_calibre_library() {
    let root = calibre_fixture();
    let lib = Library::open(root.path()).unwrap();

    let omens = lib.get(3).unwrap();
    let mut identifiers = std::collections::BTreeMap::new();
    identifiers.insert("goodreads".to_owned(), "12067".to_owned());
    identifiers.insert("isbn".to_owned(), "9780575048003".to_owned());
    assert_eq!(
        omens.metadata,
        Metadata {
            title: "Good Omens".into(),
            // Calibre keeps authors in link order, not alphabetical.
            authors: vec!["Terry Pratchett".into(), "Neil Gaiman".into()],
            series: Some("Discworld-ish".into()),
            series_index: Some(2.5),
            publisher: Some("Gollancz".into()),
            language: Some("eng".into()),
            // Calibre stores date-only values shifted off the 1st of the month.
            published: Some("1990-05-02".into()),
            description: Some("<p>Apocalypse <b>comedy</b></p>".into()),
            tags: vec!["Fantasy".into(), "Humour".into()],
            identifiers,
        }
    );
    assert!(omens.has_cover);
    assert_eq!(
        omens.added_at.to_rfc3339(),
        "2026-10-16T08:28:35.115527+00:00"
    );

    // Unknown formats (DJVU) are hidden; KEPUB maps to Nalanda's Kepub.
    let emma = lib.get(2).unwrap();
    assert_eq!(
        emma.formats,
        [
            FormatFile {
                format: BookFormat::Epub,
                size: 1140
            },
            FormatFile {
                format: BookFormat::Kepub,
                size: 1140
            },
        ]
    );
    assert_eq!(emma.metadata.published, None, "calibre's undefined date");
    assert_eq!(emma.metadata.series_index, None);
    assert_eq!(
        lib.format_path(2, BookFormat::Kepub).unwrap(),
        root.path()
            .join("Jane Austen/Emma (2)/Emma - Jane Austen.kepub")
    );
    assert_eq!(
        lib.cover_path(2).unwrap(),
        Some(root.path().join("Jane Austen/Emma (2)/cover.jpg"))
    );
    assert_eq!(lib.cover_path(4).unwrap(), None);
    assert!(matches!(
        lib.format_path(3, BookFormat::Pdf),
        Err(Error::MissingFormat(3, BookFormat::Pdf))
    ));
    assert!(matches!(lib.get(99), Err(Error::NotFound(99))));
    // The first language in calibre's order.
    assert_eq!(
        lib.get(1).unwrap().metadata.language.as_deref(),
        Some("deu")
    );

    let titles = |books: Vec<Book>| {
        books
            .into_iter()
            .map(|b| b.metadata.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(lib.list(None, SortBy::Title).unwrap()),
        ["Emma", "Good Omens", "The Hobbit", "The Masnavi"]
    );
    assert_eq!(
        titles(lib.list(None, SortBy::Author).unwrap()),
        ["Emma", "Good Omens", "The Masnavi", "The Hobbit"]
    );
    assert_eq!(
        titles(lib.list(None, SortBy::RecentlyAdded).unwrap()),
        ["The Masnavi", "Good Omens", "Emma", "The Hobbit"]
    );
    // Search covers every author, series and tag, not just the first.
    assert_eq!(
        titles(lib.list(Some("gaiman"), SortBy::Title).unwrap()),
        ["Good Omens"]
    );
    assert_eq!(
        titles(lib.list(Some("fantasy"), SortBy::Title).unwrap()),
        ["Good Omens", "The Hobbit"]
    );
    assert_eq!(
        titles(lib.list(Some("discworld humour"), SortBy::Title).unwrap()),
        ["Good Omens"]
    );
    assert!(lib.list(Some("%"), SortBy::Title).unwrap().is_empty());
}

#[test]
fn writes_a_calibre_library() {
    let root = calibre_fixture();
    let src = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let lib =
        Library::open_with_backup_dir(root.path().into(), Some(backups.path().into())).unwrap();
    let db = || Connection::open(root.path().join("metadata.db")).unwrap();
    let count = |sql: &str| -> i64 { db().query_row(sql, [], |r| r.get(0)).unwrap() };

    // Import: calibre's ASCII folder and file names, a PNG cover stored as cover.jpg.
    let mut png = Vec::new();
    image::RgbImage::from_pixel(4, 6, image::Rgb([200, 30, 30]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let solaris = src.path().join("solaris.epub");
    nalanda_formats::epub::write_epub(
        &solaris,
        &Metadata {
            title: "Solaris".into(),
            authors: vec!["Stanisław Lem".into()],
            language: Some("pl".into()),
            ..Default::default()
        },
        &[Chapter {
            title: None,
            blocks: vec![Block::Paragraph("Ocean".into())],
        }],
        Some(&Cover {
            data: png,
            media_type: "image/png".into(),
        }),
    )
    .unwrap();
    let book = lib.add_book(&solaris).unwrap();
    assert_eq!(book.id, 5);
    assert_eq!(book.metadata.language.as_deref(), Some("pol"));
    assert_eq!(
        lib.format_path(5, BookFormat::Epub).unwrap(),
        root.path()
            .join("Stanislaw Lem/Solaris (5)/Solaris - Stanislaw Lem.epub")
    );
    let cover = std::fs::read(lib.cover_path(5).unwrap().unwrap()).unwrap();
    assert!(
        cover.starts_with(&[0xff, 0xd8, 0xff]),
        "cover re-encoded as JPEG"
    );
    let (sort, uuid): (String, String) = db()
        .query_row("SELECT sort, uuid FROM books WHERE id = 5", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(sort, "Solaris");
    assert_eq!(uuid.len(), 36, "calibre's insert trigger ran");

    // The catalog as it was before Nalanda's first write is kept.
    let backup = backups
        .path()
        .join("d4e62f61-72e6-403a-8a93-6ff9e140597f.db");
    let backup =
        Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let before: i64 = backup
        .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 4);

    // Edit: drop an author, change tags, series and language, set a date.
    let mut m = lib.get(3).unwrap().metadata;
    m.authors = vec!["Terry Pratchett".into()];
    m.tags = vec!["Humour".into(), "Satire".into()];
    m.series = None;
    m.language = Some("en-GB".into());
    m.published = Some("1990-05-01".into());
    m.description = None;
    let updated = lib.update_metadata(3, &m).unwrap();
    m.series_index = None;
    m.language = Some("eng".into());
    assert_eq!(updated.metadata, m);
    // Unused authors, tags and series are removed, as calibre does.
    assert_eq!(
        count("SELECT count(*) FROM authors WHERE name = 'Neil Gaiman'"),
        0
    );
    assert_eq!(count("SELECT count(*) FROM tags WHERE name = 'Fantasy'"), 1);
    assert_eq!(count("SELECT count(*) FROM series"), 0);
    assert_eq!(count("SELECT count(*) FROM comments WHERE book = 3"), 0);
    // Calibre will rewrite metadata.opf for changed books.
    assert_eq!(
        count("SELECT count(*) FROM metadata_dirtied WHERE book IN (3, 5)"),
        2
    );
    // Setting the first language keeps calibre's other languages.
    let mut hobbit = lib.get(1).unwrap().metadata;
    hobbit.language = Some("eng".into());
    lib.update_metadata(1, &hobbit).unwrap();
    let langs: Vec<String> = db()
        .prepare(
            "SELECT g.lang_code FROM books_languages_link l JOIN languages g ON g.id = l.lang_code
             WHERE l.book = 1 ORDER BY l.item_order",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(langs, ["eng", "deu"]);

    // A second format, then replacing it after a title change moves it to the new name.
    let txt = src.path().join("s.txt");
    std::fs::write(&txt, "ocean").unwrap();
    lib.add_format(5, &txt, BookFormat::Txt).unwrap();
    let old_txt = lib.format_path(5, BookFormat::Txt).unwrap();
    let mut solaris_m = lib.get(5).unwrap().metadata;
    solaris_m.title = "Solaris: A Novel".into();
    lib.update_metadata(5, &solaris_m).unwrap();
    lib.add_format(5, &txt, BookFormat::Txt).unwrap();
    let new_txt = lib.format_path(5, BookFormat::Txt).unwrap();
    assert_eq!(
        new_txt.file_name().unwrap(),
        "Solaris_ A Novel - Stanislaw Lem.txt"
    );
    assert!(new_txt.exists() && !old_txt.exists());

    // Remove: rows, files and the now-empty author folder go.
    lib.remove_book(5).unwrap();
    assert!(!root.path().join("Stanislaw Lem").exists());
    assert_eq!(count("SELECT count(*) FROM data WHERE book = 5"), 0);
    assert_eq!(
        count("SELECT count(*) FROM authors WHERE name = 'Stanisław Lem'"),
        0
    );
    assert_eq!(
        count("SELECT count(*) FROM languages WHERE lang_code = 'pol'"),
        0
    );
    assert!(matches!(lib.get(5), Err(Error::NotFound(5))));
    assert_eq!(lib.list(None, SortBy::Title).unwrap().len(), 4);
}

#[test]
fn calibre_file_names() {
    let m = |title: &str, author: &str| Metadata {
        title: title.into(),
        authors: vec![author.into()],
        ..Default::default()
    };
    assert_eq!(
        calibre::book_path_name(12, &m("Jalāl's: Collected?", "Jalāl ad-Dīn Rūmī")),
        "Jalal ad-Din Rumi/Jalal's_ Collected_ (12)"
    );
    assert_eq!(
        calibre::book_path_name(1, &m("  ", "Con")),
        "Conw/Unknown (1)"
    );
    // Expected values are calibre 6.13's own construct_path_name output.
    assert_eq!(
        calibre::book_path_name(7, &m("...", "A. B. ")),
        "A. B_/_ (7)"
    );
    assert_eq!(
        calibre::book_path_name(8, &m("A/B+C*D", "x|y")),
        "x_y/A_B_C_D (8)"
    );
    let long = "x".repeat(150);
    let path = calibre::book_path_name(123, &m(&long, "A"));
    assert_eq!(path, format!("A/{} (123)", "x".repeat(95)));
    let (file, name) = calibre::format_file_name(&m(&long, "Jane Austen"), BookFormat::Kepub);
    assert_eq!(name, format!("{} - Jane Austen", "x".repeat(42)));
    assert_eq!(file, format!("{name}.kepub"));
}

#[test]
fn title_sort_matches_calibre() {
    use calibre::title_sort;
    assert_eq!(title_sort("The Hobbit"), "Hobbit, The");
    assert_eq!(
        title_sort("  a Tale of Two Cities"),
        "Tale of Two Cities, a"
    );
    assert_eq!(title_sort("“An Odd Book"), "Odd Book, An");
    assert_eq!(title_sort("Theory"), "Theory");
    assert_eq!(title_sort("Anathem"), "Anathem");
    assert_eq!(title_sort("The"), "The");
}

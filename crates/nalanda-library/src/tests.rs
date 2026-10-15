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

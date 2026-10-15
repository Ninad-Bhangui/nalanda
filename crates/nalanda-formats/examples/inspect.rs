//! Prints what Nalanda extracts from ebook files.
//! `cargo run -p nalanda-formats --example inspect -- book.epub [...]`

fn main() {
    for arg in std::env::args_os().skip(1) {
        let path = std::path::PathBuf::from(arg);
        println!("== {}", path.display());
        match nalanda_formats::read_book_info(&path) {
            Ok(info) => {
                println!("{:#?}", info.metadata);
                match &info.cover {
                    Some(c) => println!("cover: {} ({} bytes)", c.media_type, c.data.len()),
                    None => println!("cover: none"),
                }
            }
            Err(e) => println!("metadata error: {e}"),
        }
        match nalanda_formats::read_document(&path) {
            Ok(doc) => {
                let blocks: usize = doc.chapters.iter().map(|c| c.blocks.len()).sum();
                println!("{} chapters, {blocks} blocks", doc.chapters.len());
                for c in doc.chapters.iter().take(6) {
                    let first = c
                        .blocks
                        .iter()
                        .find(|b| matches!(b, nalanda_formats::Block::Paragraph(_)));
                    let preview: String = first
                        .map(|b| b.text().chars().take(70).collect())
                        .unwrap_or_default();
                    println!("  - {:?}: {preview}", c.display_title());
                }
            }
            Err(e) => println!("document error: {e}"),
        }
    }
}

//! Builds a synthetic library for benchmarking the desktop frontends:
//! `cargo run --release -p nalanda-desktop --example make_library -- DIR [COUNT]`.
//!
//! Every book is a small EPUB with a 1200×1800 JPEG cover (the size real covers come in) and
//! a few chapters of text, added through `Library::add_book` as an import would.

use std::path::PathBuf;

use image::{ImageBuffer, Rgb};
use nalanda_core::Metadata;
use nalanda_formats::{Block, Chapter, Cover};

const WORDS: &[&str] = &[
    "river", "lantern", "archive", "monsoon", "orchard", "copper", "harbour", "meridian",
    "quiet", "ember", "atlas", "garden", "winter", "tide", "letters", "salt", "mirror", "hollow",
];
const AUTHORS: &[&str] = &[
    "Asha Rao", "Tomas Lind", "Mei Chen", "Kofi Mensah", "Ines Duarte", "Ravi Iyer",
    "Hana Sato", "Olga Petrova", "Diego Ruiz", "Amara Obi",
];

fn word(seed: usize) -> &'static str {
    WORDS[seed % WORDS.len()]
}

fn cover(i: usize) -> Vec<u8> {
    let (w, h) = (1200u32, 1800u32);
    let hue = (i * 47 % 360) as f32;
    let base = |t: f32| -> [u8; 3] {
        let c = |shift: f32| (((hue + shift).to_radians().sin() * 0.5 + 0.5) * 200.0 * t + 30.0) as u8;
        [c(0.0), c(120.0), c(240.0)]
    };
    let img = ImageBuffer::from_fn(w, h, |x, y| {
        let t = 0.4 + 0.6 * (y as f32 / h as f32);
        let mut p = base(t);
        // A band and some texture, so the JPEG isn't trivially compressible.
        if (y / 90) % 7 == 3 {
            p = [255 - p[0], 255 - p[1], 255 - p[2]];
        }
        let n = ((x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) >> 13) as u8 & 15;
        Rgb([p[0].saturating_add(n), p[1].saturating_add(n), p[2].saturating_add(n)])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: make_library DIR [COUNT]"));
    let count: usize = args.next().map_or(Ok(500), |c| c.parse())?;
    let library = nalanda_library::Library::open(&dir)?;
    let scratch = tempfile::tempdir()?;
    for i in 0..count {
        let title = format!(
            "The {} of the {} {}",
            capitalize(word(i)),
            word(i / 3 + 7),
            word(i / 5 + 11)
        );
        let metadata = Metadata {
            title: title.clone(),
            authors: vec![AUTHORS[i % AUTHORS.len()].to_owned()],
            series: (i % 4 == 0).then(|| format!("{} Cycle", capitalize(word(i / 4)))),
            series_index: (i % 4 == 0).then_some((i / 4 % 6 + 1) as f64),
            publisher: Some("Synthetic Press".into()),
            language: Some("en".into()),
            published: Some(format!("{}-0{}-1{}", 1950 + i % 70, i % 9 + 1, i % 9)),
            description: Some(format!(
                "Book {i} of a generated benchmark library. A story of {} and {}.",
                word(i + 3),
                word(i + 5)
            )),
            tags: vec![word(i + 1).into(), word(i + 2).into()],
            ..Default::default()
        };
        let chapters: Vec<Chapter> = (0..6)
            .map(|c| Chapter {
                title: Some(format!("Chapter {}", c + 1)),
                blocks: std::iter::once(Block::Heading {
                    level: 1,
                    text: format!("Chapter {}", c + 1),
                })
                .chain((0..40).map(|p| {
                    Block::Paragraph(
                        (0..60)
                            .map(|w| word(i + c * 31 + p * 7 + w))
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                }))
                .collect(),
            })
            .collect();
        let path = scratch.path().join(format!("book-{i}.epub"));
        let cover = Cover {
            data: cover(i),
            media_type: "image/jpeg".into(),
        };
        nalanda_formats::epub::write_epub(&path, &metadata, &chapters, Some(&cover))?;
        library.add_book(&path)?;
        if (i + 1) % 100 == 0 {
            eprintln!("{} / {count}", i + 1);
        }
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

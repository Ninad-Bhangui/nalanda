//! The built-in reader: renders a [`Document`] chapter by chapter. Only the paragraphs near the
//! viewport are built (`virtual_rows`), so a long chapter costs what a screen of it costs.

use nalanda_formats::{Block, Document};
use rune::{Color, El, TextStyle, col, el, row, text};
use rune_kit::Kit;

use crate::app::{App, View};
use crate::settings::ReaderTheme;

pub struct Reader {
    pub title: String,
    pub doc: Document,
    pub chapter: usize,
}

impl Reader {
    pub fn new(doc: Document) -> Self {
        Self {
            title: doc.title.clone(),
            doc,
            chapter: 0,
        }
    }

    /// True if the chapter changed.
    pub fn go_to(&mut self, chapter: usize) -> bool {
        if chapter < self.doc.chapters.len() && chapter != self.chapter {
            self.chapter = chapter;
            true
        } else {
            false
        }
    }

    pub fn chapter_label(&self, ix: usize) -> String {
        self.doc.chapters[ix]
            .display_title()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Section {}", ix + 1))
    }

    pub fn block_count(&self) -> usize {
        self.doc
            .chapters
            .get(self.chapter)
            .map_or(0, |c| c.blocks.len())
    }
}

/// Colors of the reader page.
#[derive(Clone, Copy)]
pub struct PageColors {
    pub background: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub code: Color,
}

fn hex(s: &str) -> Color {
    rune::color::hex(s).expect("valid hex colour")
}

impl PageColors {
    pub fn for_theme(theme: ReaderTheme, k: &Kit) -> Self {
        let fixed = |bg, text, muted, border, code| Self {
            background: hex(bg),
            text: hex(text),
            muted: hex(muted),
            border: hex(border),
            code: hex(code),
        };
        match theme {
            ReaderTheme::App => Self {
                background: k.c.bg_base,
                text: k.c.text_base,
                muted: k.c.text_muted,
                border: k.c.border_base,
                code: k.c.layer_02,
            },
            ReaderTheme::Light => fixed("#ffffff", "#1f1f1f", "#6b6b6b", "#dddddd", "#f2f2f2"),
            ReaderTheme::Sepia => fixed("#f4ecd8", "#5b4636", "#8a7360", "#d9c9a8", "#ebe0c6"),
            ReaderTheme::Dark => fixed("#161616", "#d0ccc4", "#8f8a82", "#34322f", "#24231f"),
        }
    }
}

/// The page: one virtual row per block, centred at a readable measure.
pub fn page(reader: &Reader, size: f32, colors: PageColors) -> El {
    let count = reader.block_count();
    let rows = if count == 0 {
        col().child(
            text("This book has no readable text.")
                .font(TextStyle::new(size, 400.0).lh(size * 1.6))
                .color(colors.text),
        )
    } else {
        // Keys carry the chapter, so a new chapter mounts fresh rows at the top.
        col()
            .key("page-rows")
            .w_full()
            .growth(rune::GrowthSpec::default())
            .virtual_rows_version(reader.chapter)
            .virtual_rows(
                count,
                size * 1.6 * 3.0,
                |s: &App, i| match &s.view {
                    View::Reader(r) => format!("b{}-{i}", r.chapter).into(),
                    _ => format!("b-{i}").into(),
                },
                move |s: &App, i| {
                    let View::Reader(r) = &s.view else {
                        return el();
                    };
                    let Some(block) = r.doc.chapters.get(r.chapter).and_then(|c| c.blocks.get(i))
                    else {
                        return el();
                    };
                    // Rows space themselves with padding: virtual rows can't use stack gaps.
                    row()
                        .w_full()
                        .justify_center()
                        .px(32.0)
                        .pb(size * 0.8)
                        .pt(if i == 0 { 40.0 } else { 0.0 })
                        .child(col().w_full().max_w(size * 38.0).child(block_el(block, size, colors)))
                },
            )
    };
    col()
        .key("page-scroll")
        .grow(1.0)
        .h_full()
        .min_h(0.0)
        .fill(colors.background)
        .scroll(rune::ScrollSpec { stick_bottom: false })
        .child(rows.pb(40.0))
}

fn block_el(block: &Block, size: f32, colors: PageColors) -> El {
    let body = TextStyle::new(size, 400.0).lh(size * 1.6);
    match block {
        Block::Heading { level, text: t } => {
            let scale = match level {
                1 => 1.8,
                2 => 1.5,
                3 => 1.3,
                _ => 1.1,
            };
            text(t.clone())
                .font(TextStyle::new(size * scale, 700.0).lh(size * scale * 1.3))
                .color(colors.text)
                .wrap()
                .pt(size * 0.6)
        }
        Block::Paragraph(t) => text(t.clone()).font(body).color(colors.text).wrap(),
        Block::Quote(t) => col()
            .w_full()
            .pl(16.0)
            .border_sides([0.0, 0.0, 0.0, 2.0], colors.border)
            .child(text(t.clone()).font(body.italic()).color(colors.muted).wrap()),
        Block::Preformatted(t) => col()
            .w_full()
            .padding(12.0)
            .radius(6.0)
            .fill(colors.code)
            .scroll_x()
            .child(
                text(t.clone())
                    .font(TextStyle::new(size * 0.85, 400.0).lh(size * 0.85 * 1.5).mono())
                    .color(colors.text),
            ),
        Block::Separator => row()
            .w_full()
            .justify_center()
            .child(text("⁂").font(body).color(colors.muted)),
    }
}

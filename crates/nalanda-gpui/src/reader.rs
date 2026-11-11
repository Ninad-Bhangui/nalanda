//! The built-in reader: renders a [`Document`] chapter by chapter.

use gpui_kit::component::ActiveTheme as _;
use nalanda_formats::{Block, Document};

use crate::settings::ReaderTheme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub struct Reader {
    pub title: SharedString,
    pub doc: Document,
    pub chapter: usize,
    pub scroll: ScrollHandle,
}

impl Reader {
    pub fn new(doc: Document) -> Self {
        Self {
            title: doc.title.clone().into(),
            doc,
            chapter: 0,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn go_to(&mut self, chapter: usize) {
        if chapter < self.doc.chapters.len() {
            self.chapter = chapter;
            self.scroll.set_offset(Point::default());
        }
    }

    pub fn chapter_label(&self, ix: usize) -> String {
        self.doc.chapters[ix]
            .display_title()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Section {}", ix + 1))
    }

    /// Table of contents entries: (index, label, is_current).
    pub fn toc(&self) -> impl Iterator<Item = (usize, String, bool)> + '_ {
        (0..self.doc.chapters.len()).map(|ix| (ix, self.chapter_label(ix), ix == self.chapter))
    }

    pub fn render_page(&self, size: f32, colors: &PageColors) -> Div {
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(size * 0.8))
            .w_full()
            .max_w(px(size * 38.0))
            .text_size(px(size))
            .line_height(px(size * 1.6))
            .text_color(colors.text);
        let Some(chapter) = self.doc.chapters.get(self.chapter) else {
            return page.child("This book has no readable text.");
        };
        for block in &chapter.blocks {
            let el = match block {
                Block::Heading { level, text } => {
                    let scale = match level {
                        1 => 1.8,
                        2 => 1.5,
                        3 => 1.3,
                        _ => 1.1,
                    };
                    div()
                        .pt(px(size * 0.6))
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(size * scale))
                        .line_height(px(size * scale * 1.3))
                        .child(text.clone())
                }
                Block::Paragraph(text) => div().child(text.clone()),
                Block::Quote(text) => div()
                    .pl_4()
                    .border_l_2()
                    .border_color(colors.border)
                    .italic()
                    .text_color(colors.muted)
                    .child(text.clone()),
                Block::Preformatted(text) => div()
                    .p_3()
                    .rounded_md()
                    .bg(colors.code)
                    .font_family("monospace")
                    .text_size(px(size * 0.85))
                    .whitespace_nowrap()
                    .child(text.clone()),
                Block::Separator => div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_color(colors.muted)
                    .child("⁂"),
            };
            page = page.child(el);
        }
        page
    }
}

/// Colors of the reader page.
pub struct PageColors {
    pub background: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub border: Hsla,
    pub code: Hsla,
}

impl PageColors {
    pub fn for_theme(theme: ReaderTheme, cx: &App) -> Self {
        let fixed = |bg: u32, text: u32, muted: u32, border: u32, code: u32| Self {
            background: rgb(bg).into(),
            text: rgb(text).into(),
            muted: rgb(muted).into(),
            border: rgb(border).into(),
            code: rgb(code).into(),
        };
        match theme {
            ReaderTheme::App => {
                let t = cx.theme();
                Self {
                    background: t.background,
                    text: t.foreground,
                    muted: t.muted_foreground,
                    border: t.border,
                    code: t.muted,
                }
            }
            ReaderTheme::Light => fixed(0xffffff, 0x1f1f1f, 0x6b6b6b, 0xdddddd, 0xf2f2f2),
            ReaderTheme::Sepia => fixed(0xf4ecd8, 0x5b4636, 0x8a7360, 0xd9c9a8, 0xebe0c6),
            ReaderTheme::Dark => fixed(0x161616, 0xd0ccc4, 0x8f8a82, 0x34322f, 0x24231f),
        }
    }
}

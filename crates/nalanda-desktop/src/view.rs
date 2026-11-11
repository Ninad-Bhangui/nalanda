//! The window: library grid, book details, metadata editor, devices and reader.
//! `view` reads the state and queues handlers; nothing here changes state directly.

use std::path::PathBuf;

use nalanda_core::{Book, BookFormat};
use nalanda_device::ReadStatus;
use nalanda_library::SortBy;
use rune::{
    Color, Cursor, Cx, El, ScrollSpec, Spring, SurfaceFit, TextStyle, Transition, Tx, col,
    color::hex, el, row, text,
};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Dialog, Kit, PaletteFocus, PaletteSeed, SegmentedControl,
    TextArea, TextInput,
};

use crate::app::{App, Confirm, View, book_key, human_size};
use crate::reader::{self, PageColors};
use crate::settings::{Appearance, ReaderTheme};

const SIDEBAR_WIDTH: f32 = 220.0;
const DETAILS_WIDTH: f32 = 340.0;
const CARD_WIDTH: f32 = 152.0;
const CARD_PADDING: f32 = 6.0;
const COVER: (f32, f32) = (140.0, 210.0);
const GRID_GAP: f32 = 16.0;
const GRID_PADDING: f32 = 16.0;
/// Cover, title (two 18 pt lines), author line, gaps and the row's bottom gap.
const GRID_ROW_HEIGHT: f32 = COVER.1 + 2.0 * CARD_PADDING + 36.0 + 16.0 + 8.0 + GRID_GAP;
const SELECT_MOTION: Spring = Spring::new(0.22, 0.0);

fn library_kit(dark: bool) -> Kit {
    // Paper and ink in light mode; a quiet blue-black reading room after dark.
    let (bg, fg, accent) = if dark {
        ("#1c1c1b", "#f1eee6", "#dca779")
    } else {
        ("#f9f6ef", "#302e2a", "#a75435")
    };
    Kit::from_palette_focus(
        PaletteSeed {
            bg: hex(bg).expect("theme color"),
            fg: hex(fg).expect("theme color"),
            accent: hex(accent).expect("theme color"),
        },
        PaletteFocus::Accent,
    )
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = library_kit(s.dark(cx.system_dark()));
    let scale = cx.env().scale;
    let mut layer = Vec::new();
    let body = match &s.view {
        View::Reader(_) => col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(reader_view(s, &k)),
        View::Device(mount) => col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(toolbar(s, &k))
            .child(
                row()
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .child(sidebar(s, &k))
                    .child(device_view(s, mount, &k)),
            ),
        View::Library => {
            let grid_width = cx
                .layout_size("grid-viewport")
                .map_or(1280.0 - SIDEBAR_WIDTH - DETAILS_WIDTH, |[w, _]| w);
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(toolbar(s, &k))
                .child(
                    row()
                        .w_full()
                        .grow(1.0)
                        .min_h(0.0)
                        .child(sidebar(s, &k))
                        .child(grid(s, &k, grid_width, scale))
                        .child(details(s, &k, scale)),
                )
        }
    };
    confirm_dialog(s, &k, &mut layer);
    let base = col()
        .key("app")
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(body)
        .child(status_bar(s, &k))
        .on_drop_files(|s: &mut App, files, _| s.import(files.to_vec()));
    rune_kit::root(base, layer)
}

// ----- small pieces ---------------------------------------------------------------------

fn label(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.small).color(k.c.text_muted)
}

fn heading(k: &Kit, t: impl Into<String>) -> El {
    text(t)
        .font(TextStyle::new(17.0, 700.0).lh(22.0))
        .color(k.c.text_base)
        .wrap()
}

fn small_button(t: &str, variant: ButtonVariant) -> Button {
    Button::new(t).variant(variant).control_size(ControlSize::Regular)
}

/// A row of mutually exclusive choices, e.g. Auto / Light / Dark.
fn segmented<T: Copy + PartialEq + Send + 'static>(
    k: &Kit,
    name: &str,
    options: &'static [(T, &'static str)],
    current: T,
    on_select: fn(&mut App, T),
) -> El {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let selected = options.iter().position(|(v, _)| *v == current);
    SegmentedControl::new(&labels, selected)
        .label(name)
        .fit()
        .on_pick(move |s: &mut App, i, _: &mut Tx| on_select(s, options[i].0))
        .build(k)
}

// ----- toolbar & sidebar ----------------------------------------------------------------

const SORTS: &[(SortBy, &str)] = &[
    (SortBy::RecentlyAdded, "Recent"),
    (SortBy::Title, "Title"),
    (SortBy::Author, "Author"),
];
const APPEARANCES: &[(Appearance, &str)] = &[
    (Appearance::System, "Auto"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
];
const READER_THEMES: &[(ReaderTheme, &str)] = &[
    (ReaderTheme::App, "Auto"),
    (ReaderTheme::Light, "Light"),
    (ReaderTheme::Sepia, "Sepia"),
    (ReaderTheme::Dark, "Dark"),
];

fn toolbar(s: &App, k: &Kit) -> El {
    let search = TextInput::new(&s.search)
        .placeholder("Search title, author, series, tags…")
        .label("Search")
        .clear_button()
        .width(Some(320.0))
        .on_edit(|s: &mut App| &mut s.search)
        .on_change(|s: &mut App, _: &mut Tx| s.reload())
        .build(k);
    let sort = segmented(k, "Sort", SORTS, s.sort, |s, sort| {
        s.sort = sort;
        s.reload();
    });
    let appearance = segmented(k, "Theme", APPEARANCES, s.settings.appearance, |s, a| {
        s.update_settings(|st| st.appearance = a)
    });
    let server = match &s.server {
        Some(server) => row()
            .gap(6.0)
            .center_y()
            .child(
                text(format!("● Serving http://{}", server.addr()))
                    .font(k.ty.small)
                    .color(k.c.success.fg),
            )
            .child(
                small_button("Stop", ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.toggle_server())
                    .build(k)
                    .key("tb-stop-server"),
            ),
        None => small_button("Start server", ButtonVariant::Ghost)
            .help("Share this library over HTTP and OPDS for phones and other clients")
            .on_press(|s: &mut App| s.toggle_server())
            .build(k)
            .key("tb-serve"),
    };
    row()
        .key("toolbar")
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(8.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_base)
        .child(
            Button::new("Add books…")
                .variant(ButtonVariant::Contrast)
                .disabled(s.busy.is_some())
                .on_press_tx(|s: &mut App, tx: &mut Tx| s.prompt_add_file(tx))
                .build(k)
                .key("tb-add"),
        )
        .child(
            small_button("Add folder…", ButtonVariant::Ghost)
                .disabled(s.busy.is_some())
                .on_press_tx(|s: &mut App, tx: &mut Tx| s.prompt_add_folder(tx))
                .build(k)
                .key("tb-add-folder"),
        )
        .child(search)
        .child(row().gap(6.0).center_y().child(label(k, "Sort")).child(sort))
        .child(el().grow(1.0))
        .child(row().gap(6.0).center_y().child(label(k, "Theme")).child(appearance))
        .child(server)
}

fn sidebar_item(k: &Kit, key: String, title: String, sub: Option<String>, active: bool) -> El {
    let mut item = col()
        .key(key)
        .w_full()
        .px(12.0)
        .py(8.0)
        .radius(6.0)
        .border_sides(
            [0.0, 0.0, 0.0, 3.0],
            if active { k.c.text_accent } else { k.c.bg_deep },
        )
        .cursor(Cursor::Pointer)
        .hover(k.c.hover)
        .press(k.c.pressed)
        .press_scale(0.98)
        .motion(SELECT_MOTION)
        .child(
            text(title)
                .font(k.ty.ui_medium)
                .color(if active { k.c.text_accent } else { k.c.text_base })
                .truncate(),
        );
    if active {
        item = item.fill(k.c.layer_02);
    }
    if let Some(sub) = sub {
        item = item.child(label(k, sub).truncate());
    }
    item
}

fn section(k: &Kit, t: &str) -> El {
    text(t)
        .font(TextStyle::new(11.0, 600.0))
        .color(k.c.text_muted)
        .px(12.0)
        .pt(12.0)
        .pb(4.0)
}

fn sidebar(s: &App, k: &Kit) -> El {
    let mut devices = col().w_full().gap(4.0);
    if s.devices.is_empty() {
        devices = devices.child(
            label(k, "No device connected. Plug in a Kobo over USB.")
                .wrap()
                .px(12.0),
        );
    }
    for device in &s.devices {
        let mount = device.mount.clone();
        let active = matches!(&s.view, View::Device(m) if *m == mount);
        let count = s
            .device_books
            .iter()
            .find(|(m, _)| *m == mount)
            .map_or(0, |(_, b)| b.len());
        let sub = format!("{count} books · fw {}", device.info.firmware);
        devices = devices.child(
            sidebar_item(
                k,
                format!("side-device-{}", mount.display()),
                device.display_name(),
                Some(sub),
                active,
            )
            .on_press(move |s: &mut App| s.view = View::Device(mount.clone())),
        );
    }
    col()
        .key("sidebar")
        .w(SIDEBAR_WIDTH)
        .h_full()
        .padding(8.0)
        .gap(4.0)
        .fill(k.c.bg_deep)
        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_base)
        .child(
            col()
                .px(12.0)
                .pt(14.0)
                .pb(20.0)
                .gap(2.0)
                .child(text("NALANDA").font(TextStyle::new(19.0, 700.0)).color(k.c.text_base))
                .child(label(k, "A home for your books")),
        )
        .child(section(k, "LIBRARY"))
        .child(
            sidebar_item(
                k,
                "side-library".into(),
                "All books".into(),
                Some(format!("{} books", s.books.len())),
                matches!(s.view, View::Library),
            )
            .on_press(|s: &mut App| s.view = View::Library),
        )
        .child(section(k, "DEVICES"))
        .child(devices)
        .child(el().grow(1.0))
        .child(
            col()
                .px(12.0)
                .gap(4.0)
                .items_start()
                .child(label(k, format!("Library: {}", s.library.root().display())).wrap())
                .child(
                    small_button("Change library…", ButtonVariant::Ghost)
                        .help("Open another library folder, or an empty folder for a new one")
                        .disabled(s.busy.is_some())
                        .on_press_tx(|s: &mut App, tx: &mut Tx| s.prompt_switch_library(tx))
                        .build(k)
                        .key("side-switch-library"),
                ),
        )
        .child(
            label(
                k,
                if s.converters.has_calibre() {
                    "Converters: built-in + calibre"
                } else {
                    "Converters: built-in (install calibre for more)"
                },
            )
            .wrap()
            .px(12.0)
            .pt(4.0),
        )
}

// ----- library grid ---------------------------------------------------------------------

fn grid(s: &App, k: &Kit, width: f32, scale: f32) -> El {
    let viewport = col()
        .key("grid-viewport")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0);
    if s.books.is_empty() {
        let (title, message) = if s.search.text.is_empty() {
            (
                "A shelf waiting for stories",
                "Add books above, or drop ebook files into this window.",
            )
        } else {
            ("No books found", "Try another title, author, series, or tag.")
        };
        return viewport
            .center()
            .child(
                col()
                    .gap(8.0)
                    .items_center()
                    .child(text("✦").font(TextStyle::new(30.0, 400.0)).color(k.c.text_accent))
                    .child(heading(k, title))
                    .child(text(message).font(k.ty.ui).color(k.c.text_muted).wrap()),
            );
    }
    // Only the rows near the viewport are built, so scrolling costs the same for ten books or
    // ten thousand.
    let columns =
        (((width - GRID_PADDING * 2.0 + GRID_GAP) / (CARD_WIDTH + GRID_GAP)) as usize).max(1);
    let rows = s.books.len().div_ceil(columns);
    let k = k.clone();
    viewport
        .child(
            row()
                .w_full()
                .px(GRID_PADDING + CARD_PADDING)
                .pt(18.0)
                .pb(12.0)
                .center_y()
                .child(heading(&k, "Your library"))
                .child(el().grow(1.0))
                .child(label(&k, format!("{} titles", s.books.len()))),
        )
        .child(
            col()
                .key("grid-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(
                    col()
                        .key("grid-rows")
                        .w_full()
                        .growth(rune::GrowthSpec::default())
                        .virtual_rows_version((s.books_version, columns, s.selected, k.dark))
                        .virtual_rows(
                            rows,
                            GRID_ROW_HEIGHT,
                            move |s: &App, r| {
                                let id = s.books.get(r * columns).map_or(0, |b| b.id);
                                format!("grid-{columns}-{id}").into()
                            },
                            move |s: &App, r| {
                                let start = r * columns;
                                let end = (start + columns).min(s.books.len());
                                let mut line = row().gap(GRID_GAP).px(GRID_PADDING).pb(GRID_GAP);
                                for book in &s.books[start..end] {
                                    line = line.child(card(s, book, &k, scale));
                                }
                                line
                            },
                        ),
                ),
        )
}

fn card(s: &App, book: &Book, k: &Kit, scale: f32) -> El {
    let id = book.id;
    let selected = s.selected == Some(id);
    let synced = s.device_keys.contains(&book_key(
        &book.metadata.title,
        &book.metadata.authors_display(),
    ));
    let mut byline = row().w_full().gap(4.0).center_y().child(
        label(k, book.metadata.authors_display())
            .truncate()
            .grow(1.0),
    );
    if synced {
        byline = byline.child(text("● device").font(k.ty.small).color(k.c.success.fg));
    }
    let card = col()
        .key(format!("book-{id}"))
        .w(CARD_WIDTH)
        .padding(CARD_PADDING)
        .gap(4.0)
        .radius(8.0)
        .cursor(Cursor::Pointer)
        .hover(k.c.hover)
        .press(k.c.pressed)
        .press_scale(0.97)
        .motion(SELECT_MOTION)
        .on_press(move |s: &mut App| s.select(id))
        .on_double_press(move |s: &mut App| s.open_reader(id))
        .child(cover(s, book, k, COVER, scale))
        // Fixed at two lines so every row is the same height.
        .child(
            text(book.metadata.title.clone())
                .font(TextStyle::new(13.0, 500.0).lh(18.0))
                .color(k.c.text_base)
                .max_lines(2)
                .h(36.0),
        )
        .child(byline);
    if selected {
        card.fill(k.c.layer_02).border(2.0, k.c.border_focus)
    } else {
        card
    }
}

fn cover(s: &App, book: &Book, k: &Kit, (w, h): (f32, f32), scale: f32) -> El {
    let frame = el()
        .size(w, h)
        .radius(6.0)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base);
    if book.has_cover {
        // Decode at the box's device-pixel size: sharp, and far smaller than the full cover.
        let pixels = s.covers.get(
            &s.library,
            book.id,
            (w * scale).ceil() as u32,
            (h * scale).ceil() as u32,
        );
        return frame.pixels_fit(&pixels, SurfaceFit::Fill);
    }
    col()
        .size(w, h)
        .radius(6.0)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .padding(12.0)
        .gap(8.0)
        .justify_center()
        .child(
            text(book.metadata.title.clone())
                .font(TextStyle::new(13.0, 700.0))
                .color(k.c.text_base)
                .align(rune::Align::Center)
                .wrap()
                .w_full(),
        )
        .child(
            label(k, book.metadata.authors_display())
                .align(rune::Align::Center)
                .wrap()
                .w_full(),
        )
}

// ----- details & editor -----------------------------------------------------------------

fn details(s: &App, k: &Kit, scale: f32) -> El {
    let panel = |content: El| {
        col()
            .key("details")
            .w(DETAILS_WIDTH)
            .h_full()
            .min_h(0.0)
            .fill(k.c.bg_base)
            .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_base)
            .scroll(ScrollSpec { stick_bottom: false })
            .child(content)
    };
    let Some(book) = s.selected.and_then(|id| s.books.iter().find(|b| b.id == id)) else {
        return panel(
            col()
                .padding(16.0)
                .child(text("Select a book to see its details.").font(k.ty.ui).color(k.c.text_muted)),
        );
    };
    if s.editor.book == Some(book.id) {
        return panel(editor(s, k));
    }
    let id = book.id;
    let m = &book.metadata;
    let info_row = |name: &'static str, value: String| {
        row()
            .w_full()
            .gap(8.0)
            .items_start()
            .child(label(k, name).w(84.0))
            .child(
                text(value)
                    .font(k.ty.ui)
                    .color(k.c.text_base)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
    };
    let mut info = col().w_full().gap(6.0);
    if let Some(series) = &m.series {
        let idx = m.series_index.map(|i| format!(" #{i}")).unwrap_or_default();
        info = info.child(info_row("Series", format!("{series}{idx}")));
    }
    let optional = [
        ("Publisher", m.publisher.clone()),
        ("Published", m.published.clone()),
        ("Language", m.language.clone()),
        ("ISBN", m.isbn().map(str::to_owned)),
    ];
    for (name, value) in optional {
        if let Some(v) = value {
            info = info.child(info_row(name, v));
        }
    }
    if !m.tags.is_empty() {
        info = info.child(info_row("Tags", m.tags.join(", ")));
    }
    let formats = book
        .formats
        .iter()
        .map(|f| format!("{} ({})", f.format, human_size(f.size)))
        .collect::<Vec<_>>()
        .join(", ");
    info = info
        .child(info_row("Formats", formats))
        .child(info_row("Added", book.added_at.format("%Y-%m-%d").to_string()));

    let sources: Vec<BookFormat> = book.formats.iter().map(|f| f.format).collect();
    let mut convert = row().w_full().flex_wrap().gap(4.0);
    for to in s.converters.targets(&sources) {
        convert = convert.child(
            small_button(&to.to_string(), ButtonVariant::Outline)
                .disabled(s.busy.is_some())
                .on_press(move |s: &mut App| s.convert(id, to))
                .build(k)
                .key(format!("convert-{}", to.extension())),
        );
    }
    let mut send = col().gap(4.0).items_start();
    for device in &s.devices {
        let mount: PathBuf = device.mount.clone();
        send = send.child(
            small_button(&format!("Send to {}", device.display_name()), ButtonVariant::Neutral)
                .disabled(s.busy.is_some())
                .on_press(move |s: &mut App| s.send_to_device(id, mount.clone()))
                .build(k)
                .key(format!("send-{}", device.mount.display())),
        );
    }
    let title = m.title.clone();
    let mut content = col()
        .key(format!("details-book-{id}"))
        .w_full()
        .padding(16.0)
        .gap(12.0)
        .transition(Transition::offset(0.0, 8.0).combined(Transition::opacity()))
        .child(row().w_full().justify_center().child(cover(s, book, k, (200.0, 300.0), scale)))
        .child(
            col()
                .w_full()
                .gap(2.0)
                .child(heading(k, m.title.clone()))
                .child(text(m.authors_display()).font(k.ty.ui).color(k.c.text_muted).wrap()),
        )
        .child(
            row()
                .gap(8.0)
                .child(
                    small_button("Read", ButtonVariant::Contrast)
                        .disabled(book.preferred_reading_format().is_none())
                        .on_press(move |s: &mut App| s.open_reader(id))
                        .build(k)
                        .key("d-read"),
                )
                .child(
                    small_button("Edit metadata", ButtonVariant::Neutral)
                        .on_press(move |s: &mut App| s.start_editing(id))
                        .build(k)
                        .key("d-edit"),
                )
                .child(
                    small_button("Remove", ButtonVariant::Danger)
                        .on_press(move |s: &mut App| {
                            s.confirm = Some(Confirm::RemoveBook {
                                id,
                                title: title.clone(),
                            })
                        })
                        .build(k)
                        .key("d-remove"),
                ),
        )
        .child(send)
        .child(info)
        .child(col().w_full().gap(4.0).child(label(k, "Convert to")).child(convert));
    if let Some(desc) = &m.description {
        content = content.child(text(desc.clone()).font(k.ty.ui).color(k.c.text_muted).wrap());
    }
    panel(content)
}

fn editor(s: &App, k: &Kit) -> El {
    type Field = for<'a> fn(&'a mut App) -> &'a mut rune::TextEdit;
    let field = |name: &'static str, edit: &rune::TextEdit, access: Field, placeholder: &str| {
        col()
            .w_full()
            .gap(4.0)
            .child(label(k, name))
            .child(
                TextInput::new(edit)
                    .label(name)
                    .placeholder(placeholder)
                    .width(None)
                    .on_edit(access)
                    .on_submit(|s: &mut App, _: &mut Tx| s.save_editor())
                    .build(k),
            )
    };
    let e = &s.editor;
    col()
        .key("editor")
        .w_full()
        .padding(16.0)
        .gap(8.0)
        .child(heading(k, "Edit metadata"))
        .child(field("Title", &e.title, |s| &mut s.editor.title, "Title"))
        .child(field(
            "Authors (separate with &)",
            &e.authors,
            |s| &mut s.editor.authors,
            "Author & Another Author",
        ))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(field("Series", &e.series, |s| &mut s.editor.series, "Series").grow(1.0))
                .child(
                    field("Number", &e.series_index, |s| &mut s.editor.series_index, "1").w(70.0),
                ),
        )
        .child(field(
            "Tags (comma separated)",
            &e.tags,
            |s| &mut s.editor.tags,
            "tag, another tag",
        ))
        .child(field("Publisher", &e.publisher, |s| &mut s.editor.publisher, "Publisher"))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    field("Published", &e.published, |s| &mut s.editor.published, "YYYY-MM-DD")
                        .grow(1.0),
                )
                .child(field("Language", &e.language, |s| &mut s.editor.language, "en").w(90.0)),
        )
        .child(field("ISBN", &e.isbn, |s| &mut s.editor.isbn, "ISBN"))
        .child(
            col()
                .w_full()
                .gap(4.0)
                .child(label(k, "Description"))
                .child(
                    TextArea::new(&e.description)
                        .label("Description")
                        .rows(6)
                        .width(None)
                        .on_edit(|s: &mut App| &mut s.editor.description)
                        .build(k),
                ),
        )
        .child(
            row()
                .gap(8.0)
                .child(
                    small_button("Save", ButtonVariant::Contrast)
                        .on_press(|s: &mut App| s.save_editor())
                        .build(k)
                        .key("ed-save"),
                )
                .child(
                    small_button("Cancel", ButtonVariant::Ghost)
                        .on_press(|s: &mut App| s.editor.book = None)
                        .build(k)
                        .key("ed-cancel"),
                ),
        )
}

fn confirm_dialog(s: &App, k: &Kit, layer: &mut Vec<El>) {
    let Some(confirm) = &s.confirm else {
        return;
    };
    let (title, detail) = match confirm {
        Confirm::RemoveBook { title, .. } => (
            format!("Remove “{title}” from the library?"),
            "This deletes its files from the library.",
        ),
        Confirm::DeleteFromDevice { book, .. } => (
            format!("Delete “{}” from the device?", book.title),
            "This removes the file from the e-reader.",
        ),
    };
    Dialog::new("confirm", title)
        .description(detail)
        .open(true)
        .fit()
        .width(420.0)
        .on_close(|s: &mut App| s.confirm = None)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .padding(16.0)
                .justify_end()
                .child(
                    small_button("Cancel", ButtonVariant::Ghost)
                        .on_press(|s: &mut App| s.confirm = None)
                        .build(k)
                        .key("confirm-cancel"),
                )
                .child(
                    small_button("Delete", ButtonVariant::Danger)
                        .on_press(|s: &mut App| s.confirm_delete())
                        .build(k)
                        .key("confirm-delete"),
                ),
        )
        .build(k, layer);
}

// ----- devices --------------------------------------------------------------------------

fn device_view(s: &App, mount: &PathBuf, k: &Kit) -> El {
    let Some(device) = s.devices.iter().find(|d| d.mount == *mount) else {
        return el().grow(1.0);
    };
    let books: &[nalanda_device::DeviceBook] = s
        .device_books
        .iter()
        .find(|(m, _)| m == mount)
        .map_or(&[], |(_, b)| b.as_slice());
    let library_keys: std::collections::HashSet<String> = s
        .books
        .iter()
        .map(|b| book_key(&b.metadata.title, &b.metadata.authors_display()))
        .collect();
    let refresh_mount = mount.clone();
    let header = row()
        .w_full()
        .gap(16.0)
        .padding(16.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_base)
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .child(heading(k, device.display_name()))
                .child(label(
                    k,
                    format!(
                        "Serial {} · Firmware {} · {}",
                        device.info.serial,
                        device.info.firmware,
                        device.mount.display()
                    ),
                )),
        )
        .child(
            small_button("Refresh", ButtonVariant::Neutral)
                .on_press(move |s: &mut App| s.refresh_device(refresh_mount.clone()))
                .build(k)
                .key("dev-refresh"),
        );
    let mut list = col().w_full();
    for (ix, book) in books.iter().enumerate() {
        let in_library = library_keys.contains(&book_key(&book.title, &book.authors));
        let kind = match (&book.content_id, book.is_sideloaded()) {
            (_, false) => "Store",
            (None, true) => "Pending (eject to index)",
            (Some(_), true) => "Sideloaded",
        };
        let progress = match book.status {
            ReadStatus::Finished => "Finished".to_owned(),
            ReadStatus::Reading => format!("{:.0}% read", book.percent_read),
            ReadStatus::Unread => "Unread".to_owned(),
        };
        let deletable = book.is_sideloaded();
        let (mount, target) = (mount.clone(), book.clone());
        list = list.child(
            row()
                .key(format!("dev-book-{ix}"))
                .w_full()
                .gap(12.0)
                .px(16.0)
                .py(8.0)
                .center_y()
                .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_base)
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .child(
                            text(book.title.clone())
                                .font(k.ty.ui_medium)
                                .color(k.c.text_base)
                                .truncate(),
                        )
                        .child(label(k, book.authors.clone()).truncate()),
                )
                .child(text(progress).font(k.ty.small).color(k.c.text_base).w(90.0))
                .child(label(k, kind).w(150.0))
                .child(
                    text(if in_library { "In library" } else { "" })
                        .font(k.ty.small)
                        .color(if in_library { k.c.success.fg } else { k.c.text_muted })
                        .w(80.0),
                )
                .child(
                    small_button("Delete", ButtonVariant::Ghost)
                        .disabled(!deletable || s.busy.is_some())
                        .on_press(move |s: &mut App| {
                            s.confirm = Some(Confirm::DeleteFromDevice {
                                mount: mount.clone(),
                                book: target.clone(),
                            })
                        })
                        .build(k)
                        .key(format!("dev-delete-{ix}")),
                ),
        );
    }
    col()
        .key("device")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .child(header)
        .child(
            col()
                .key("device-list")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(list),
        )
}

// ----- reader ---------------------------------------------------------------------------

fn step_chapter(s: &mut App, delta: isize, tx: &mut Tx) {
    if let View::Reader(r) = &mut s.view {
        let next = r.chapter as isize + delta;
        if next >= 0 && r.go_to(next as usize) {
            tx.scroll_to_instant("page-scroll", 0.0, 0.0);
        }
    }
}

fn change_font(s: &mut App, delta: f32) {
    s.update_settings(|st| st.reader_font_size = (st.reader_font_size + delta).clamp(12.0, 32.0));
}

fn reader_view(s: &App, k: &Kit) -> El {
    let View::Reader(reader) = &s.view else {
        return el();
    };
    let chapter = reader.chapter;
    let total = reader.doc.chapters.len();
    let mut toc = col().w_full().gap(2.0).padding(8.0);
    for ix in 0..total {
        let current = ix == chapter;
        let mut item = text(reader.chapter_label(ix))
            .key(format!("toc-{ix}"))
            .font(if current { k.ty.ui_medium } else { k.ty.ui })
            .color(k.c.text_base)
            .truncate()
            .w_full()
            .px(8.0)
            .py(4.0)
            .radius(6.0)
            .cursor(Cursor::Pointer)
            .hover(k.c.hover)
            .on_press_tx(move |s: &mut App, tx: &mut Tx| {
                if let View::Reader(r) = &mut s.view
                    && r.go_to(ix)
                {
                    tx.scroll_to_instant("page-scroll", 0.0, 0.0);
                }
            });
        if current {
            item = item.fill(k.c.layer_03);
        }
        toc = toc.child(item);
    }
    let page_theme = segmented(k, "Page", READER_THEMES, s.settings.reader_theme, |s, t| {
        s.update_settings(|st| st.reader_theme = t)
    });
    let toolbar = row()
        .key("reader-toolbar")
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(8.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_base)
        .child(
            small_button("← Library", ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.view = View::Library)
                .build(k)
                .key("r-back"),
        )
        .child(
            text(reader.title.clone())
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0),
        )
        .child(page_theme)
        .child(
            small_button("A−", ButtonVariant::Ghost)
                .on_press(|s: &mut App| change_font(s, -1.0))
                .build(k)
                .key("r-font-down"),
        )
        .child(
            small_button("A+", ButtonVariant::Ghost)
                .on_press(|s: &mut App| change_font(s, 1.0))
                .build(k)
                .key("r-font-up"),
        )
        .child(
            small_button("‹ Prev", ButtonVariant::Neutral)
                .disabled(chapter == 0)
                .on_press_tx(|s: &mut App, tx: &mut Tx| step_chapter(s, -1, tx))
                .build(k)
                .key("r-prev"),
        )
        .child(label(k, format!("{} / {}", (chapter + 1).min(total), total)))
        .child(
            small_button("Next ›", ButtonVariant::Neutral)
                .disabled(chapter + 1 >= total)
                .on_press_tx(|s: &mut App, tx: &mut Tx| step_chapter(s, 1, tx))
                .build(k)
                .key("r-next"),
        );
    let colors = PageColors::for_theme(s.settings.reader_theme, k);
    col()
        .key("reader")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(toolbar)
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(
                    col()
                        .key("toc-scroll")
                        .w(240.0)
                        .h_full()
                        .min_h(0.0)
                        .fill(k.c.bg_deep)
                        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_base)
                        .scroll(ScrollSpec { stick_bottom: false })
                        .child(toc),
                )
                .child(reader::page(reader, s.settings.reader_font_size, colors)),
        )
}

// ----- status ---------------------------------------------------------------------------

fn status_bar(s: &App, k: &Kit) -> El {
    let (message, color): (String, Color) = match (&s.busy, &s.status) {
        (Some(busy), _) => (busy.clone(), k.c.text_muted),
        (None, Some(st)) => (
            st.text.clone(),
            if st.error { k.c.danger.fg } else { k.c.text_muted },
        ),
        (None, None) => ("Ready".into(), k.c.text_muted),
    };
    row()
        .key("status")
        .w_full()
        .px(12.0)
        .py(4.0)
        .fill(k.c.layer_01)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_base)
        .child(text(message).key("status-text").font(k.ty.small).color(color).truncate())
}

//! Main window: library grid, book details, metadata editor, devices and reader.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, Theme, ThemeMode, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use nalanda_convert::Converters;
use nalanda_core::{Book, BookFormat, BookId, Metadata};
use nalanda_device::{DeviceBook, KoboDevice, ReadStatus};
use nalanda_library::{Library, SortBy};

use crate::reader::{PageColors, Reader};
use crate::settings::{Appearance, ReaderTheme, Settings};

/// Formats Kobo devices read, in the order Nalanda prefers to send them.
const KOBO_FORMATS: [BookFormat; 5] = [
    BookFormat::Kepub,
    BookFormat::Epub,
    BookFormat::Pdf,
    BookFormat::Cbz,
    BookFormat::Txt,
];

enum View {
    Library,
    Device(PathBuf),
    Reader(Box<Reader>),
}

struct Status {
    text: SharedString,
    error: bool,
}

/// Inputs of the metadata editor for one book.
struct Editor {
    book_id: BookId,
    title: Entity<InputState>,
    authors: Entity<InputState>,
    series: Entity<InputState>,
    series_index: Entity<InputState>,
    tags: Entity<InputState>,
    publisher: Entity<InputState>,
    language: Entity<InputState>,
    published: Entity<InputState>,
    isbn: Entity<InputState>,
    description: Entity<TextareaState>,
}

pub struct NalandaApp {
    library: Arc<Library>,
    converters: Arc<Converters>,
    books: Vec<Book>,
    sort: SortBy,
    selected: Option<BookId>,
    search: Entity<InputState>,
    view: View,
    devices: Vec<KoboDevice>,
    /// Books on each connected device, keyed by mount point.
    device_books: Vec<(PathBuf, Vec<DeviceBook>)>,
    editor: Option<Editor>,
    status: Option<Status>,
    busy: Option<SharedString>,
    server: Option<crate::server::Server>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl NalandaApp {
    pub fn new(
        library: Arc<Library>,
        converters: Arc<Converters>,
        settings: Settings,
        imports: Vec<PathBuf>,
        warning: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search title, author, series, tags…")
        });
        let subscription = cx.subscribe_in(&search, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.reload(cx);
            }
        });
        // Re-sync when the OS switches between light and dark, if following it.
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.settings.appearance == Appearance::System {
                this.apply_appearance(window, cx);
            }
        });
        let mut app = Self {
            library,
            converters,
            books: Vec::new(),
            sort: SortBy::RecentlyAdded,
            selected: None,
            search,
            view: View::Library,
            devices: Vec::new(),
            device_books: Vec::new(),
            editor: None,
            status: warning.map(|text| Status {
                text: text.into(),
                error: true,
            }),
            busy: None,
            server: None,
            settings,
            settings_path: Settings::default_path(),
            _subscriptions: vec![subscription, appearance],
        };
        app.apply_appearance(window, cx);
        app.reload(cx);
        app.watch_devices(cx);
        if !imports.is_empty() {
            app.import(imports, cx);
        }
        app
    }

    // ----- settings ---------------------------------------------------------------

    fn apply_appearance(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.settings.appearance {
            Appearance::System => Theme::sync_system_appearance(Some(window), cx),
            Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
    }

    /// Applies a settings change and persists it.
    fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.set_error(format!("Could not save settings: {e}"));
        }
        cx.notify();
    }

    /// A row of mutually exclusive small buttons, e.g. Auto / Light / Dark.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: &[(T, &'static str)],
        current: T,
        on_select: fn(&mut Self, T, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Div {
        let mut row = h_flex().gap_0p5();
        for (ix, (value, label)) in options.iter().copied().enumerate() {
            let button = Button::new((id, ix)).small().label(label);
            let button = if value == current {
                button.primary()
            } else {
                button.ghost()
            };
            row = row.child(button.on_click(
                cx.listener(move |this, _, window, cx| on_select(this, value, window, cx)),
            ));
        }
        row
    }

    // ----- state & background work ------------------------------------------------

    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        match self.library.list(Some(&query), self.sort) {
            Ok(books) => self.books = books,
            Err(e) => self.set_error(format!("Could not read library: {e}")),
        }
        if self
            .selected
            .is_some_and(|id| !self.books.iter().any(|b| b.id == id))
        {
            self.selected = None;
        }
        cx.notify();
    }

    fn set_info(&mut self, text: impl Into<SharedString>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn set_error(&mut self, text: impl Into<SharedString>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// Runs `work` on a background thread, then `done` on the UI thread.
    fn run<T: Send + 'static>(
        &mut self,
        label: impl Into<SharedString>,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(label.into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { work() }).await;
            this.update(cx, |this, cx| {
                this.busy = None;
                done(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Polls for connected Kobo devices every few seconds.
    fn watch_devices(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let found = cx
                    .background_spawn(async move {
                        nalanda_device::detect_kobo_devices()
                            .into_iter()
                            .map(|d| {
                                let books = d.books().unwrap_or_default();
                                (d, books)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let alive = this.update(cx, |this, cx| this.set_devices(found, cx));
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        })
        .detach();
    }

    fn set_devices(&mut self, found: Vec<(KoboDevice, Vec<DeviceBook>)>, cx: &mut Context<Self>) {
        let mounts: Vec<PathBuf> = found.iter().map(|(d, _)| d.mount.clone()).collect();
        let before: Vec<PathBuf> = self.devices.iter().map(|d| d.mount.clone()).collect();
        if mounts != before {
            for (d, _) in found.iter().filter(|(d, _)| !before.contains(&d.mount)) {
                self.set_info(format!(
                    "Connected: {} at {}",
                    d.display_name(),
                    d.mount.display()
                ));
            }
            if let View::Device(mount) = &self.view
                && !mounts.contains(mount)
            {
                self.view = View::Library;
                self.set_info("Device disconnected");
            }
        }
        self.device_books = found
            .iter()
            .map(|(d, books)| (d.mount.clone(), books.clone()))
            .collect();
        self.devices = found.into_iter().map(|(d, _)| d).collect();
        cx.notify();
    }

    fn refresh_device(&mut self, mount: PathBuf, cx: &mut Context<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        self.run(
            "Reading device…",
            move || device.books(),
            move |this, result, _| match result {
                Ok(books) => {
                    if let Some(entry) = this.device_books.iter_mut().find(|(m, _)| *m == mount) {
                        entry.1 = books;
                    }
                }
                Err(e) => this.set_error(format!("Could not read device: {e}")),
            },
            cx,
        );
    }

    /// Normalized "title|author" keys of everything on connected devices.
    fn on_device_keys(&self) -> HashSet<String> {
        self.device_books
            .iter()
            .flat_map(|(_, books)| books.iter())
            .map(|b| book_key(&b.title, &b.authors))
            .collect()
    }

    fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let library = self.library.clone();
        let count = paths.len();
        self.run(
            format!("Adding {count} book(s)…"),
            move || {
                let mut added = Vec::new();
                let mut failed = Vec::new();
                for path in collect_ebooks(paths) {
                    match library.add_book(&path) {
                        Ok(book) => added.push(book.id),
                        Err(e) => failed.push(format!("{}: {e}", path.display())),
                    }
                }
                (added, failed)
            },
            |this, (added, failed), cx| {
                this.reload(cx);
                if let Some(&last) = added.last() {
                    this.selected = Some(last);
                }
                if failed.is_empty() {
                    this.set_info(format!("Added {} book(s)", added.len()));
                } else {
                    this.set_error(format!(
                        "Added {}, failed {}: {}",
                        added.len(),
                        failed.len(),
                        failed.join("; ")
                    ));
                }
            },
            cx,
        );
    }

    fn prompt_add(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Add to library".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = paths.await;
            this.update(cx, |this, cx| match picked {
                Ok(Ok(Some(paths))) => this.import(paths, cx),
                Ok(Err(e)) => {
                    this.set_error(format!(
                        "File picker unavailable: {e}. Drop files on the window instead."
                    ));
                    cx.notify();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn convert(&mut self, id: BookId, to: BookFormat, cx: &mut Context<Self>) {
        let library = self.library.clone();
        let converters = self.converters.clone();
        self.run(
            format!("Converting to {to}…"),
            move || nalanda_convert::convert_library_book(&library, &converters, id, to, None),
            move |this, result, cx| {
                match result {
                    Ok(backend) => this.set_info(format!("Converted to {to} with {backend}")),
                    Err(e) => this.set_error(format!("Conversion failed: {e}")),
                }
                this.reload(cx);
            },
            cx,
        );
    }

    fn send_to_device(&mut self, id: BookId, mount: PathBuf, cx: &mut Context<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        let Some(book) = self.books.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let library = self.library.clone();
        let converters = self.converters.clone();
        self.run(
            format!("Sending to {}…", device.display_name()),
            move || -> Result<(PathBuf, Option<&'static str>), String> {
                let mut converted = None;
                let format = match KOBO_FORMATS.into_iter().find(|f| book.has_format(*f)) {
                    Some(f) => f,
                    None => {
                        // Nothing Kobo can read: make an EPUB first.
                        let backend = nalanda_convert::convert_library_book(
                            &library,
                            &converters,
                            id,
                            BookFormat::Epub,
                            None,
                        )
                        .map_err(|e| format!("needs conversion to EPUB first: {e}"))?;
                        converted = Some(backend);
                        BookFormat::Epub
                    }
                };
                let path = library.format_path(id, format).map_err(|e| e.to_string())?;
                let dest = device
                    .send_book(&path, &book.metadata)
                    .map_err(|e| e.to_string())?;
                Ok((dest, converted))
            },
            move |this, result, cx| {
                match result {
                    Ok((dest, converted)) => {
                        let note = if converted.is_some() {
                            " (converted to EPUB)"
                        } else {
                            ""
                        };
                        this.set_info(format!(
                            "Sent to device{note}: {}. Eject the device to let it index the book.",
                            dest.display()
                        ));
                        this.reload(cx);
                    }
                    Err(e) => this.set_error(format!("Send failed: {e}")),
                }
                this.refresh_device(mount, cx);
            },
            cx,
        );
    }

    fn delete_from_device(&mut self, mount: PathBuf, book: DeviceBook, cx: &mut Context<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        let title = book.title.clone();
        self.run(
            "Deleting from device…",
            move || device.delete_book(&book),
            move |this, result, cx| {
                match result {
                    Ok(()) => this.set_info(format!("Deleted “{title}” from device")),
                    Err(e) => this.set_error(format!("Delete failed: {e}")),
                }
                this.refresh_device(mount, cx);
            },
            cx,
        );
    }

    /// Asks before a destructive action and runs `action` only on "Delete".
    fn confirm_delete(
        &mut self,
        message: String,
        detail: &str,
        action: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(detail),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| action(this, cx)).ok();
            }
        })
        .detach();
    }

    fn remove_book(&mut self, id: BookId, cx: &mut Context<Self>) {
        match self.library.remove_book(id) {
            Ok(()) => self.set_info("Removed book from library"),
            Err(e) => self.set_error(format!("Remove failed: {e}")),
        }
        self.editor = None;
        self.reload(cx);
    }

    fn open_reader(&mut self, id: BookId, cx: &mut Context<Self>) {
        let Some(book) = self.books.iter().find(|b| b.id == id) else {
            return;
        };
        let Some(format) = book.preferred_reading_format() else {
            self.set_error("No readable format. Convert to EPUB first.");
            cx.notify();
            return;
        };
        let path = match self.library.format_path(id, format) {
            Ok(p) => p,
            Err(e) => {
                self.set_error(e.to_string());
                cx.notify();
                return;
            }
        };
        let title = book.metadata.title.clone();
        self.run(
            "Opening book…",
            move || nalanda_formats::read_document(&path),
            move |this, result, _| match result {
                Ok(mut doc) => {
                    if doc.title.is_empty() {
                        doc.title = title;
                    }
                    this.view = View::Reader(Box::new(Reader::new(doc)));
                }
                Err(e) => this.set_error(format!("Could not open book: {e}")),
            },
            cx,
        );
    }

    fn toggle_server(&mut self, cx: &mut Context<Self>) {
        if let Some(server) = self.server.take() {
            server.stop();
            self.set_info("Content server stopped");
        } else {
            match self.start_server() {
                Ok(url) => self.set_info(format!(
                    "Content server running at {url} (OPDS at {url}/opds)"
                )),
                Err(e) => self.set_error(e),
            }
        }
        cx.notify();
    }

    /// Starts the content server for the current library and returns its URL.
    fn start_server(&mut self) -> Result<String, String> {
        let server = crate::server::start(self.library.clone(), self.converters.clone())
            .map_err(|e| format!("Could not start server: {e}"))?;
        let url = format!("http://{}", server.addr());
        self.server = Some(server);
        Ok(url)
    }

    fn prompt_switch_library(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use as library".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = paths.await;
            this.update(cx, |this, cx| match picked {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => {
                    this.switch_library(paths.swap_remove(0), cx)
                }
                Ok(Err(e)) => {
                    this.set_error(format!("Folder picker unavailable: {e}"));
                    cx.notify();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    /// Opens the library in `dir` (creating it if the folder is empty) and
    /// remembers it for the next launch. A running content server moves along.
    fn switch_library(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if self.busy.is_some() || dir == self.library.root() {
            return;
        }
        if let Err(e) = crate::settings::check_library_folder(&dir) {
            self.set_error(e);
            cx.notify();
            return;
        }
        let library = match Library::open(&dir) {
            Ok(library) => Arc::new(library),
            Err(e) => {
                self.set_error(format!("Could not open library {}: {e}", dir.display()));
                cx.notify();
                return;
            }
        };
        self.library = library;
        self.selected = None;
        self.editor = None;
        if matches!(self.view, View::Reader(_)) {
            self.view = View::Library;
        }
        self.set_info(format!("Switched to library {}", dir.display()));
        self.reload(cx);
        if let Some(server) = self.server.take() {
            server.stop();
            if let Err(e) = self.start_server() {
                self.set_error(e);
            }
        }
        self.update_settings(cx, |s| s.library_dir = Some(dir));
    }

    // ----- metadata editor -----------------------------------------------------------

    fn start_editing(&mut self, id: BookId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(book) = self.books.iter().find(|b| b.id == id) else {
            return;
        };
        let m = &book.metadata;
        let mut field = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let editor = Editor {
            book_id: id,
            title: field(m.title.clone(), "Title"),
            authors: field(m.authors.join(" & "), "Author & Another Author"),
            series: field(m.series.clone().unwrap_or_default(), "Series"),
            series_index: field(
                m.series_index.map(|i| i.to_string()).unwrap_or_default(),
                "1",
            ),
            tags: field(m.tags.join(", "), "tag, another tag"),
            publisher: field(m.publisher.clone().unwrap_or_default(), "Publisher"),
            language: field(m.language.clone().unwrap_or_default(), "en"),
            published: field(m.published.clone().unwrap_or_default(), "YYYY-MM-DD"),
            isbn: field(m.isbn().unwrap_or_default().to_owned(), "ISBN"),
            description: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(6)
                    .default_value(m.description.clone().unwrap_or_default())
            }),
        };
        self.editor = Some(editor);
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        let Some(book) = self.books.iter().find(|b| b.id == editor.book_id) else {
            return;
        };
        let text = |e: &Entity<InputState>| e.read(cx).value().trim().to_owned();
        let opt = |s: String| (!s.is_empty()).then_some(s);
        let split = |s: String, sep: char| -> Vec<String> {
            s.split(sep)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut metadata = Metadata {
            title: text(&editor.title),
            authors: split(text(&editor.authors), '&'),
            series: opt(text(&editor.series)),
            series_index: text(&editor.series_index).parse().ok(),
            tags: split(text(&editor.tags), ','),
            publisher: opt(text(&editor.publisher)),
            language: opt(text(&editor.language)),
            published: opt(text(&editor.published)),
            description: opt(editor.description.read(cx).value().trim().to_owned()),
            identifiers: book.metadata.identifiers.clone(),
        };
        match opt(text(&editor.isbn)) {
            Some(isbn) => metadata.identifiers.insert("isbn".into(), isbn),
            None => metadata.identifiers.remove("isbn"),
        };
        if metadata.title.is_empty() {
            self.set_error("Title cannot be empty");
            self.editor = Some(editor);
            cx.notify();
            return;
        }
        match self.library.update_metadata(editor.book_id, &metadata) {
            Ok(_) => self.set_info("Metadata saved"),
            Err(e) => self.set_error(format!("Save failed: {e}")),
        }
        self.reload(cx);
    }

    // ----- rendering ------------------------------------------------------------------

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let sort = self.segmented(
            "sort",
            &[
                (SortBy::RecentlyAdded, "Recent"),
                (SortBy::Title, "Title"),
                (SortBy::Author, "Author"),
            ],
            self.sort,
            |this, sort, _, cx| {
                this.sort = sort;
                this.reload(cx);
            },
            cx,
        );
        let appearance = self.segmented(
            "appearance",
            &[
                (Appearance::System, "Auto"),
                (Appearance::Light, "Light"),
                (Appearance::Dark, "Dark"),
            ],
            self.settings.appearance,
            |this, appearance, window, cx| {
                this.update_settings(cx, |s| s.appearance = appearance);
                this.apply_appearance(window, cx);
            },
            cx,
        );
        let theme = cx.theme();
        let label = |text: &'static str| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                Button::new("add")
                    .primary()
                    .label("Add books…")
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_add(cx))),
            )
            .child(
                div()
                    .w(px(320.))
                    .child(Input::new(&self.search).cleanable(true)),
            )
            .child(h_flex().gap_1().child(label("Sort")).child(sort))
            .child(div().flex_1())
            .child(h_flex().gap_1().child(label("Theme")).child(appearance))
            .child(match &self.server {
                Some(server) => h_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.success)
                            .child(format!("● Serving http://{}", server.addr())),
                    )
                    .child(
                        Button::new("stop-serve")
                            .ghost()
                            .small()
                            .label("Stop")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_server(cx))),
                    )
                    .into_any_element(),
                None => Button::new("serve")
                    .ghost()
                    .small()
                    .label("Start server")
                    .tooltip("Share this library over HTTP and OPDS for phones and other clients")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_server(cx)))
                    .into_any_element(),
            })
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let in_library = matches!(self.view, View::Library);
        let item = |id: SharedString, label: String, sub: Option<String>, active: bool| {
            let theme = cx.theme();
            div()
                .id(ElementId::Name(id))
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .hover(|d| d.bg(theme.sidebar_accent))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                .when_some(sub, |d, sub| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(sub),
                    )
                })
        };

        let mut devices = v_flex().gap_1();
        if self.devices.is_empty() {
            devices = devices.child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("No device connected. Plug in a Kobo over USB."),
            );
        }
        for device in &self.devices {
            let mount = device.mount.clone();
            let active = matches!(&self.view, View::Device(m) if *m == mount);
            let count = self
                .device_books
                .iter()
                .find(|(m, _)| *m == mount)
                .map_or(0, |(_, b)| b.len());
            let sub = format!("{count} books · fw {}", device.info.firmware);
            devices = devices.child(
                item(
                    format!("device-{}", mount.display()).into(),
                    device.display_name(),
                    Some(sub),
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.view = View::Device(mount.clone());
                    cx.notify();
                })),
            );
        }

        let section = |label: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(label)
        };
        v_flex()
            .w(px(220.))
            .h_full()
            .p_2()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(section("LIBRARY"))
            .child(
                item(
                    "library".into(),
                    "All books".into(),
                    Some(format!("{} books", self.books.len())),
                    in_library,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.view = View::Library;
                    cx.notify();
                })),
            )
            .child(section("DEVICES"))
            .child(devices)
            .child(div().flex_1())
            .child(
                v_flex()
                    .px_3()
                    .items_start()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Library: {}", self.library.root().display()))
                    .child(
                        Button::new("switch-library")
                            .ghost()
                            .xsmall()
                            .label("Change library…")
                            .tooltip(
                                "Open another library folder, or an empty folder for a new one",
                            )
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.prompt_switch_library(cx))),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if self.converters.has_calibre() {
                        "Converters: built-in + calibre"
                    } else {
                        "Converters: built-in (install calibre for more)"
                    }),
            )
    }

    fn render_grid(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let on_device = self.on_device_keys();
        if self.books.is_empty() {
            let query_empty = self.search.read(cx).value().is_empty();
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(if query_empty {
                    "Your library is empty. Click “Add books…” or drop ebook files here."
                } else {
                    "No books match your search."
                })
                .into_any_element();
        }
        let mut grid = div().flex().flex_wrap().gap_4().p_4();
        for book in &self.books {
            let id = book.id;
            let selected = self.selected == Some(id);
            let synced = on_device.contains(&book_key(
                &book.metadata.title,
                &book.metadata.authors_display(),
            ));
            let cover = self.cover_element(book, px(140.), px(210.), cx);
            grid = grid.child(
                v_flex()
                    .id(("book", id as u64))
                    .w(px(152.))
                    .p_1p5()
                    .gap_1()
                    .rounded_lg()
                    .cursor_pointer()
                    .border_2()
                    .border_color(if selected {
                        theme.ring
                    } else {
                        transparent_black()
                    })
                    .hover(|d| d.bg(theme.secondary))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 {
                            this.open_reader(id, cx);
                        } else {
                            if this.selected != Some(id) {
                                this.editor = None;
                            }
                            this.selected = Some(id);
                            cx.notify();
                        }
                    }))
                    .child(cover)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .line_clamp(2)
                            .child(book.metadata.title.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(book.metadata.authors_display()),
                            )
                            .when(synced, |d| {
                                d.child(div().text_xs().text_color(theme.success).child("● device"))
                            }),
                    ),
            );
        }
        div()
            .id("grid-scroll")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(grid)
            .into_any_element()
    }

    fn cover_element(&self, book: &Book, w: Pixels, h: Pixels, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let path = book
            .has_cover
            .then(|| self.library.cover_path(book.id).ok().flatten())
            .flatten();
        let frame = div()
            .w(w)
            .h(h)
            .rounded_md()
            .overflow_hidden()
            .bg(theme.muted)
            .border_1()
            .border_color(theme.border);
        match path {
            Some(path) => frame
                .child(img(path).size_full().object_fit(ObjectFit::Cover))
                .into_any_element(),
            None => frame
                .flex()
                .flex_col()
                .justify_center()
                .p_3()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .text_center()
                        .child(book.metadata.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(book.metadata.authors_display()),
                )
                .into_any_element(),
        }
    }

    fn render_details(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let panel = v_flex()
            .id("details")
            .w(px(340.))
            .h_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background);
        let Some(book) = self
            .selected
            .and_then(|id| self.books.iter().find(|b| b.id == id))
            .cloned()
        else {
            return panel
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Select a book to see its details."),
                )
                .into_any_element();
        };
        if let Some(editor) = &self.editor
            && editor.book_id == book.id
        {
            return panel.child(self.render_editor(cx)).into_any_element();
        }
        let id = book.id;
        let m = &book.metadata;
        let title = m.title.clone();
        let row = |label: &'static str, value: String| {
            h_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().text_sm().child(value))
        };
        let mut info = v_flex().gap_1p5();
        if let Some(series) = &m.series {
            let idx = m.series_index.map(|i| format!(" #{i}")).unwrap_or_default();
            info = info.child(row("Series", format!("{series}{idx}")));
        }
        let optional = [
            ("Publisher", m.publisher.clone()),
            ("Published", m.published.clone()),
            ("Language", m.language.clone()),
            ("ISBN", m.isbn().map(str::to_owned)),
        ];
        for (label, value) in optional {
            if let Some(v) = value {
                info = info.child(row(label, v));
            }
        }
        if !m.tags.is_empty() {
            info = info.child(row("Tags", m.tags.join(", ")));
        }
        let formats = book
            .formats
            .iter()
            .map(|f| format!("{} ({})", f.format, human_size(f.size)))
            .collect::<Vec<_>>()
            .join(", ");
        info = info.child(row("Formats", formats));
        info = info.child(row("Added", book.added_at.format("%Y-%m-%d").to_string()));

        let sources: Vec<BookFormat> = book.formats.iter().map(|f| f.format).collect();
        let mut convert = h_flex().flex_wrap().gap_1();
        for to in self.converters.targets(&sources) {
            convert = convert.child(
                Button::new(SharedString::from(format!("convert-{}", to.extension())))
                    .small()
                    .outline()
                    .label(to.to_string())
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| this.convert(id, to, cx))),
            );
        }

        let mut send = v_flex().gap_1();
        for device in &self.devices {
            let mount = device.mount.clone();
            send = send.child(
                Button::new(SharedString::from(format!("send-{}", mount.display())))
                    .small()
                    .label(format!("Send to {}", device.display_name()))
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_to_device(id, mount.clone(), cx)
                    })),
            );
        }

        panel
            .child(div().flex().justify_center().child(self.cover_element(
                &book,
                px(200.),
                px(300.),
                cx,
            )))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child(m.title.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(m.authors_display()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("read")
                            .primary()
                            .small()
                            .label("Read")
                            .disabled(book.preferred_reading_format().is_none())
                            .on_click(cx.listener(move |this, _, _, cx| this.open_reader(id, cx))),
                    )
                    .child(Button::new("edit").small().label("Edit metadata").on_click(
                        cx.listener(move |this, _, window, cx| this.start_editing(id, window, cx)),
                    ))
                    .child(
                        Button::new("remove")
                            .small()
                            .danger()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete(
                                    format!("Remove “{title}” from the library?"),
                                    "This deletes its files from the library.",
                                    move |this, cx| this.remove_book(id, cx),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(send)
            .child(info)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Convert to"),
                    )
                    .child(convert),
            )
            .when_some(m.description.clone(), |d, desc| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(desc),
                )
            })
            .into_any_element()
    }

    fn render_editor(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.as_ref().expect("editor open");
        let field = |label: &'static str, state: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(Input::new(state).small())
        };
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("Edit metadata"),
            )
            .child(field("Title", &editor.title))
            .child(field("Authors (separate with &)", &editor.authors))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(field("Series", &editor.series)))
                    .child(
                        div()
                            .w(px(70.))
                            .child(field("Number", &editor.series_index)),
                    ),
            )
            .child(field("Tags (comma separated)", &editor.tags))
            .child(field("Publisher", &editor.publisher))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(field("Published", &editor.published)))
                    .child(div().w(px(90.)).child(field("Language", &editor.language))),
            )
            .child(field("ISBN", &editor.isbn))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Description"),
                    )
                    .child(Textarea::new(&editor.description)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save")
                            .primary()
                            .small()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))),
                    )
                    .child(
                        Button::new("cancel")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editor = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_device(&mut self, mount: PathBuf, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(device) = self.devices.iter().find(|d| d.mount == mount) else {
            return div().flex_1().into_any_element();
        };
        let books = self
            .device_books
            .iter()
            .find(|(m, _)| *m == mount)
            .map(|(_, b)| b.clone())
            .unwrap_or_default();
        let library_keys: HashSet<String> = self
            .books
            .iter()
            .map(|b| book_key(&b.metadata.title, &b.metadata.authors_display()))
            .collect();

        let header = h_flex()
            .gap_4()
            .p_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child(device.display_name()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "Serial {} · Firmware {} · {}",
                                device.info.serial,
                                device.info.firmware,
                                device.mount.display()
                            )),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("refresh-device")
                    .small()
                    .label("Refresh")
                    .on_click(cx.listener({
                        let mount = mount.clone();
                        move |this, _, _, cx| this.refresh_device(mount.clone(), cx)
                    })),
            );

        let mut list = v_flex();
        for (ix, book) in books.into_iter().enumerate() {
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
            let mount = mount.clone();
            list = list.child(
                h_flex()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(book.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(book.authors.clone()),
                            ),
                    )
                    .child(div().w(px(90.)).text_xs().child(progress))
                    .child(
                        div()
                            .w(px(150.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(kind),
                    )
                    .child(
                        div()
                            .w(px(80.))
                            .text_xs()
                            .text_color(if in_library {
                                theme.success
                            } else {
                                theme.muted_foreground
                            })
                            .child(if in_library { "In library" } else { "" }),
                    )
                    .child(
                        Button::new(("delete-device-book", ix))
                            .small()
                            .ghost()
                            .label("Delete")
                            .disabled(!deletable || self.busy.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let (mount, book) = (mount.clone(), book.clone());
                                this.confirm_delete(
                                    format!("Delete “{}” from the device?", book.title),
                                    "This removes the file from the e-reader.",
                                    move |this, cx| this.delete_from_device(mount, book, cx),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            );
        }
        v_flex()
            .flex_1()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("device-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_reader(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let page_theme = self.segmented(
            "page-theme",
            &[
                (ReaderTheme::App, "Auto"),
                (ReaderTheme::Light, "Light"),
                (ReaderTheme::Sepia, "Sepia"),
                (ReaderTheme::Dark, "Dark"),
            ],
            self.settings.reader_theme,
            |this, theme, _, cx| this.update_settings(cx, |s| s.reader_theme = theme),
            cx,
        );
        let theme = cx.theme();
        let View::Reader(reader) = &self.view else {
            unreachable!()
        };
        let chapter = reader.chapter;
        let total = reader.doc.chapters.len();
        let mut toc = v_flex().gap_0p5().p_2();
        for (ix, label, current) in reader.toc() {
            toc = toc.child(
                div()
                    .id(("toc", ix))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .truncate()
                    .when(current, |d| {
                        d.bg(theme.sidebar_accent).font_weight(FontWeight::MEDIUM)
                    })
                    .hover(|d| d.bg(theme.sidebar_accent))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let View::Reader(r) = &mut this.view {
                            r.go_to(ix);
                        }
                        cx.notify();
                    })),
            );
        }
        let step = |delta: isize| {
            cx.listener(
                move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    if let View::Reader(r) = &mut this.view {
                        let next = r.chapter as isize + delta;
                        if next >= 0 {
                            r.go_to(next as usize);
                        }
                    }
                    cx.notify();
                },
            )
        };
        let font = |delta: f32| {
            cx.listener(
                move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    this.update_settings(cx, |s| {
                        s.reader_font_size = (s.reader_font_size + delta).clamp(12.0, 32.0)
                    });
                },
            )
        };
        let toolbar = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                Button::new("back")
                    .small()
                    .ghost()
                    .label("← Library")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.view = View::Library;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(reader.title.clone()),
            )
            .child(page_theme)
            .child(
                Button::new("font-down")
                    .small()
                    .ghost()
                    .label("A−")
                    .on_click(font(-1.0)),
            )
            .child(
                Button::new("font-up")
                    .small()
                    .ghost()
                    .label("A+")
                    .on_click(font(1.0)),
            )
            .child(
                Button::new("prev")
                    .small()
                    .label("‹ Prev")
                    .disabled(chapter == 0)
                    .on_click(step(-1)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} / {}", (chapter + 1).min(total), total)),
            )
            .child(
                Button::new("next")
                    .small()
                    .label("Next ›")
                    .disabled(chapter + 1 >= total)
                    .on_click(step(1)),
            );
        let colors = PageColors::for_theme(self.settings.reader_theme, cx);
        let page = reader.render_page(self.settings.reader_font_size, &colors);
        v_flex().size_full().child(toolbar).child(
            h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("toc-scroll")
                        .w(px(240.))
                        .h_full()
                        .overflow_y_scroll()
                        .bg(theme.sidebar)
                        .border_r_1()
                        .border_color(theme.sidebar_border)
                        .child(toc),
                )
                .child(
                    div()
                        .id("page-scroll")
                        .flex_1()
                        .h_full()
                        .overflow_y_scroll()
                        .track_scroll(&reader.scroll)
                        .bg(colors.background)
                        .child(div().flex().justify_center().px_8().py_10().child(page)),
                ),
        )
    }

    fn render_status(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (text, color) = match (&self.busy, &self.status) {
            (Some(busy), _) => (busy.clone(), theme.muted_foreground),
            (None, Some(s)) => (
                s.text.clone(),
                if s.error {
                    theme.danger
                } else {
                    theme.muted_foreground
                },
            ),
            (None, None) => ("Ready".into(), theme.muted_foreground),
        };
        h_flex()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(theme.status_bar_border)
            .bg(theme.status_bar)
            .text_xs()
            .text_color(color)
            .child(div().truncate().child(text))
    }
}

impl Render for NalandaApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let root = v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import(paths.paths().to_vec(), cx);
            }));
        if matches!(self.view, View::Reader(_)) {
            return root
                .child(div().flex_1().min_h_0().child(self.render_reader(cx)))
                .child(self.render_status(cx));
        }
        let main = match &self.view {
            View::Device(mount) => {
                let mount = mount.clone();
                self.render_device(mount, cx).into_any_element()
            }
            _ => h_flex()
                .flex_1()
                .h_full()
                .child(self.render_grid(cx))
                .child(self.render_details(cx))
                .into_any_element(),
        };
        root.child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(main),
            )
            .child(self.render_status(cx))
    }
}

/// Expands directories and keeps only files with an ebook extension.
fn collect_ebooks(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = paths;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if BookFormat::from_path(&path).is_some() {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Loose identity used to match library books with device books.
fn book_key(title: &str, authors: &str) -> String {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    format!("{}|{}", norm(title), norm(authors))
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

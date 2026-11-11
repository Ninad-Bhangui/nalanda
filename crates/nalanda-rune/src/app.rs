//! App state and everything that changes it: the library, devices, the editor and the reader.
//! The view (`view.rs`) only reads this and queues handlers.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nalanda_convert::Converters;
use nalanda_core::{Book, BookFormat, BookId, Metadata};
use nalanda_device::{DeviceBook, KoboDevice};
use nalanda_library::{Library, SortBy};
use rune::{FileDialogOptions, TextEdit, Tx, Waker};

use crate::covers::Covers;
use crate::jobs::Jobs;
use crate::reader::Reader;
use crate::settings::{Appearance, Settings};

/// Formats Kobo devices read, in the order Nalanda prefers to send them.
const KOBO_FORMATS: [BookFormat; 5] = [
    BookFormat::Kepub,
    BookFormat::Epub,
    BookFormat::Pdf,
    BookFormat::Cbz,
    BookFormat::Txt,
];

/// Sort order of a freshly opened window, so the first listing can be read before the window
/// exists.
pub const INITIAL_SORT: SortBy = SortBy::RecentlyAdded;

/// Named states for headless snapshots (`--scene`).
pub const SCENES: &[(&str, fn(&mut App))] = &[
    ("selected", |s| s.selected = s.books.first().map(|b| b.id)),
    ("dark", |s| s.settings.appearance = Appearance::Dark),
    ("light", |s| s.settings.appearance = Appearance::Light),
    ("editing", |s| {
        if let Some(id) = s.books.first().map(|b| b.id) {
            s.selected = Some(id);
            s.start_editing(id);
        }
    }),
    ("reading", |s| {
        if let Some(id) = s.books.first().map(|b| b.id) {
            s.open_reader_now(id);
        }
    }),
];

pub enum View {
    Library,
    Device(PathBuf),
    Reader(Box<Reader>),
}

pub struct Status {
    pub text: String,
    pub error: bool,
}

/// What a confirmation dialog deletes when the user says so.
pub enum Confirm {
    RemoveBook { id: BookId, title: String },
    DeleteFromDevice { mount: PathBuf, book: DeviceBook },
}

/// The metadata editor's fields. Always present so field accessors never need an `unwrap`;
/// `book` says whether it is open and for which book.
#[derive(Default)]
pub struct Editor {
    pub book: Option<BookId>,
    pub title: TextEdit,
    pub authors: TextEdit,
    pub series: TextEdit,
    pub series_index: TextEdit,
    pub tags: TextEdit,
    pub publisher: TextEdit,
    pub language: TextEdit,
    pub published: TextEdit,
    pub isbn: TextEdit,
    pub description: TextEdit,
}

pub struct App {
    pub library: Arc<Library>,
    pub converters: Arc<Converters>,
    pub books: Vec<Book>,
    /// Bumped whenever `books` is replaced, so the virtual grid re-keys its rows.
    pub books_version: u64,
    pub covers: Covers,
    pub sort: SortBy,
    pub selected: Option<BookId>,
    pub search: TextEdit,
    pub view: View,
    pub devices: Vec<KoboDevice>,
    /// Books on each connected device, keyed by mount point.
    pub device_books: Vec<(PathBuf, Vec<DeviceBook>)>,
    /// Normalized "title|author" keys of everything on connected devices.
    pub device_keys: HashSet<String>,
    pub editor: Editor,
    pub confirm: Option<Confirm>,
    pub status: Option<Status>,
    pub busy: Option<String>,
    pub server: Option<crate::server::Server>,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    pending_imports: Vec<PathBuf>,
    jobs: Jobs<App>,
}

impl App {
    /// `AppSpec` wants an `init` function; the real first state comes from `App::init`'s closure.
    pub fn placeholder() -> App {
        unreachable!("the first state is built by App::init")
    }

    pub fn new(
        library: Arc<Library>,
        books: Option<Vec<Book>>,
        converters: Arc<Converters>,
        settings: Settings,
        imports: Vec<PathBuf>,
        warning: Option<String>,
    ) -> Self {
        let mut app = Self {
            library,
            converters,
            books: Vec::new(),
            books_version: 0,
            covers: Covers::new(),
            sort: INITIAL_SORT,
            selected: None,
            search: TextEdit::new(""),
            view: View::Library,
            devices: Vec::new(),
            device_books: Vec::new(),
            device_keys: HashSet::new(),
            editor: Editor::default(),
            confirm: None,
            status: warning.map(|text| Status { text, error: true }),
            busy: None,
            server: None,
            settings,
            settings_path: Settings::default_path(),
            pending_imports: imports,
            jobs: Jobs::new(),
        };
        // Headless runs (`--snap`, `--script`) must never write the user's settings file.
        if rune::tooling::headless_run().is_some() {
            app.settings_path = None;
        }
        match books {
            Some(books) => {
                app.books = books;
                app.books_version = 1;
            }
            None => app.reload(),
        }
        app
    }

    /// Runs once with the window's waker: start background work that reports back.
    pub fn setup(s: &mut App, waker: Waker) {
        s.jobs.set_waker(waker);
        if rune::tooling::headless_run().is_none() {
            s.watch_devices();
        }
        let imports = std::mem::take(&mut s.pending_imports);
        if !imports.is_empty() {
            s.import(imports);
        }
    }

    /// Before every frame: apply finished background work.
    pub fn poll(s: &mut App, _clock: f64, _tx: &mut Tx) -> bool {
        let finished = s.jobs.finished();
        let any = !finished.is_empty();
        for done in finished {
            done(s);
        }
        any
    }

    pub fn dark(&self, system_dark: bool) -> bool {
        match self.settings.appearance {
            Appearance::System => system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        }
    }

    /// Applies a settings change and persists it.
    pub fn update_settings(&mut self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.set_error(format!("Could not save settings: {e}"));
        }
    }

    // ----- state & background work ------------------------------------------------

    pub fn reload(&mut self) {
        let query = self.search.text.clone();
        match self.library.list(Some(&query), self.sort) {
            Ok(books) => self.books = books,
            Err(e) => self.set_error(format!("Could not read library: {e}")),
        }
        self.books_version += 1;
        if self
            .selected
            .is_some_and(|id| !self.books.iter().any(|b| b.id == id))
        {
            self.selected = None;
        }
    }

    pub fn set_info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    pub fn set_error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// Runs `work` on a background thread, then `done` on the UI thread.
    fn run<T: Send + 'static>(
        &mut self,
        label: impl Into<String>,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T) + Send + 'static,
    ) {
        self.busy = Some(label.into());
        self.jobs.spawn(work, move |s: &mut App, result| {
            s.busy = None;
            done(s, result);
        });
    }

    /// Polls for connected Kobo devices every few seconds.
    fn watch_devices(&mut self) {
        let sender = self.jobs.sender();
        std::thread::Builder::new()
            .name("device-watch".into())
            .spawn(move || {
                let mut last: Option<Vec<(PathBuf, usize)>> = None;
                loop {
                    let found: Vec<(KoboDevice, Vec<DeviceBook>)> =
                        nalanda_device::detect_kobo_devices()
                            .into_iter()
                            .map(|d| {
                                let books = d.books().unwrap_or_default();
                                (d, books)
                            })
                            .collect();
                    // Wake the window only when something changed, so an idle app draws nothing.
                    let summary: Vec<(PathBuf, usize)> =
                        found.iter().map(|(d, b)| (d.mount.clone(), b.len())).collect();
                    if last.as_ref() != Some(&summary) {
                        last = Some(summary);
                        if !sender.send(move |s: &mut App| s.set_devices(found)) {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            })
            .expect("spawn device watcher");
    }

    fn set_devices(&mut self, found: Vec<(KoboDevice, Vec<DeviceBook>)>) {
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
        self.refresh_device_keys();
    }

    fn refresh_device_keys(&mut self) {
        self.device_keys = self
            .device_books
            .iter()
            .flat_map(|(_, books)| books.iter())
            .map(|b| book_key(&b.title, &b.authors))
            .collect();
        // The grid's "● device" marks depend on these.
        self.books_version += 1;
    }

    pub fn refresh_device(&mut self, mount: PathBuf) {
        let Some(device) = self.devices.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        self.run(
            "Reading device…",
            move || device.books(),
            move |this, result| match result {
                Ok(books) => {
                    if let Some(entry) = this.device_books.iter_mut().find(|(m, _)| *m == mount) {
                        entry.1 = books;
                    }
                    this.refresh_device_keys();
                }
                Err(e) => this.set_error(format!("Could not read device: {e}")),
            },
        );
    }

    pub fn import(&mut self, paths: Vec<PathBuf>) {
        let library = self.library.clone();
        let count = paths.len();
        self.run(
            format!("Adding {count} item(s)…"),
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
            |this, (added, failed)| {
                this.covers.clear();
                this.reload();
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
        );
    }

    /// Rune's portal dialog picks one file or one folder at a time, so "Add" offers both.
    pub fn prompt_add_file(&mut self, tx: &mut Tx) {
        let mut options = FileDialogOptions {
            title: "Add to library".into(),
            ..Default::default()
        };
        let extensions: Vec<&str> = BookFormat::ALL.iter().map(|f| f.extension()).collect();
        options = options.filter("Ebooks", &extensions);
        tx.open_file(options, |s: &mut App, result, _| s.picked_import(result));
    }

    pub fn prompt_add_folder(&mut self, tx: &mut Tx) {
        let options = FileDialogOptions {
            title: "Add a folder of books".into(),
            ..Default::default()
        };
        tx.open_folder(options, |s: &mut App, result, _| s.picked_import(result));
    }

    fn picked_import(&mut self, result: rune::FileDialogResult) {
        match result {
            Ok(Some(path)) => self.import(vec![path]),
            Ok(None) => {}
            Err(e) => self.set_error(format!(
                "File picker unavailable: {e}. Drop files on the window instead."
            )),
        }
    }

    pub fn convert(&mut self, id: BookId, to: BookFormat) {
        let library = self.library.clone();
        let converters = self.converters.clone();
        self.run(
            format!("Converting to {to}…"),
            move || nalanda_convert::convert_library_book(&library, &converters, id, to, None),
            move |this, result| {
                match result {
                    Ok(backend) => this.set_info(format!("Converted to {to} with {backend}")),
                    Err(e) => this.set_error(format!("Conversion failed: {e}")),
                }
                this.covers.clear();
                this.reload();
            },
        );
    }

    pub fn send_to_device(&mut self, id: BookId, mount: PathBuf) {
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
            move |this, result| {
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
                        this.reload();
                    }
                    Err(e) => this.set_error(format!("Send failed: {e}")),
                }
                this.refresh_device(mount);
            },
        );
    }

    fn delete_from_device(&mut self, mount: PathBuf, book: DeviceBook) {
        let Some(device) = self.devices.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        let title = book.title.clone();
        self.run(
            "Deleting from device…",
            move || device.delete_book(&book),
            move |this, result| {
                match result {
                    Ok(()) => this.set_info(format!("Deleted “{title}” from device")),
                    Err(e) => this.set_error(format!("Delete failed: {e}")),
                }
                this.refresh_device(mount);
            },
        );
    }

    /// The confirmation dialog's "Delete".
    pub fn confirm_delete(&mut self) {
        match self.confirm.take() {
            Some(Confirm::RemoveBook { id, .. }) => self.remove_book(id),
            Some(Confirm::DeleteFromDevice { mount, book }) => self.delete_from_device(mount, book),
            None => {}
        }
    }

    fn remove_book(&mut self, id: BookId) {
        match self.library.remove_book(id) {
            Ok(()) => self.set_info("Removed book from library"),
            Err(e) => self.set_error(format!("Remove failed: {e}")),
        }
        self.editor.book = None;
        self.reload();
    }

    fn reading_path(&mut self, id: BookId) -> Option<(PathBuf, String)> {
        let book = self.books.iter().find(|b| b.id == id)?;
        let Some(format) = book.preferred_reading_format() else {
            self.set_error("No readable format. Convert to EPUB first.");
            return None;
        };
        let title = book.metadata.title.clone();
        match self.library.format_path(id, format) {
            Ok(p) => Some((p, title)),
            Err(e) => {
                self.set_error(e.to_string());
                None
            }
        }
    }

    pub fn open_reader(&mut self, id: BookId) {
        let Some((path, title)) = self.reading_path(id) else {
            return;
        };
        self.run(
            "Opening book…",
            move || nalanda_formats::read_document(&path),
            move |this, result| match result {
                Ok(mut doc) => {
                    if doc.title.is_empty() {
                        doc.title = title;
                    }
                    this.view = View::Reader(Box::new(Reader::new(doc)));
                }
                Err(e) => this.set_error(format!("Could not open book: {e}")),
            },
        );
    }

    /// Opens the reader on the UI thread, for headless scenes.
    fn open_reader_now(&mut self, id: BookId) {
        let Some((path, title)) = self.reading_path(id) else {
            return;
        };
        match nalanda_formats::read_document(&path) {
            Ok(mut doc) => {
                if doc.title.is_empty() {
                    doc.title = title;
                }
                self.view = View::Reader(Box::new(Reader::new(doc)));
            }
            Err(e) => self.set_error(format!("Could not open book: {e}")),
        }
    }

    pub fn toggle_server(&mut self) {
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
    }

    /// Starts the content server for the current library and returns its URL.
    fn start_server(&mut self) -> Result<String, String> {
        let server = crate::server::start(self.library.clone(), self.converters.clone())
            .map_err(|e| format!("Could not start server: {e}"))?;
        let url = format!("http://{}", server.addr());
        self.server = Some(server);
        Ok(url)
    }

    pub fn prompt_switch_library(&mut self, tx: &mut Tx) {
        let options = FileDialogOptions {
            title: "Use as library".into(),
            ..Default::default()
        };
        tx.open_folder(options, |s: &mut App, result, _| match result {
            Ok(Some(dir)) => s.switch_library(dir),
            Ok(None) => {}
            Err(e) => s.set_error(format!("Folder picker unavailable: {e}")),
        });
    }

    /// Opens the library in `dir` (creating it if the folder is empty) and remembers it for the
    /// next launch. A running content server moves along.
    fn switch_library(&mut self, dir: PathBuf) {
        if self.busy.is_some() || dir == self.library.root() {
            return;
        }
        if let Err(e) = crate::settings::check_library_folder(&dir) {
            self.set_error(e);
            return;
        }
        let library = match Library::open(&dir) {
            Ok(library) => Arc::new(library),
            Err(e) => {
                self.set_error(format!("Could not open library {}: {e}", dir.display()));
                return;
            }
        };
        self.library = library;
        self.selected = None;
        self.editor.book = None;
        if matches!(self.view, View::Reader(_)) {
            self.view = View::Library;
        }
        self.set_info(format!("Switched to library {}", dir.display()));
        self.covers.clear();
        self.reload();
        if let Some(server) = self.server.take() {
            server.stop();
            if let Err(e) = self.start_server() {
                self.set_error(e);
            }
        }
        self.update_settings(|s| s.library_dir = Some(dir));
    }

    pub fn select(&mut self, id: BookId) {
        if self.selected != Some(id) {
            self.editor.book = None;
        }
        self.selected = Some(id);
    }

    // ----- metadata editor -----------------------------------------------------------

    pub fn start_editing(&mut self, id: BookId) {
        let Some(book) = self.books.iter().find(|b| b.id == id) else {
            return;
        };
        let m = &book.metadata;
        let e = &mut self.editor;
        e.book = Some(id);
        e.title = TextEdit::new(m.title.clone());
        e.authors = TextEdit::new(m.authors.join(" & "));
        e.series = TextEdit::new(m.series.clone().unwrap_or_default());
        e.series_index = TextEdit::new(m.series_index.map(|i| i.to_string()).unwrap_or_default());
        e.tags = TextEdit::new(m.tags.join(", "));
        e.publisher = TextEdit::new(m.publisher.clone().unwrap_or_default());
        e.language = TextEdit::new(m.language.clone().unwrap_or_default());
        e.published = TextEdit::new(m.published.clone().unwrap_or_default());
        e.isbn = TextEdit::new(m.isbn().unwrap_or_default().to_owned());
        e.description = TextEdit::new(m.description.clone().unwrap_or_default());
    }

    pub fn save_editor(&mut self) {
        let Some(id) = self.editor.book else {
            return;
        };
        let Some(book) = self.books.iter().find(|b| b.id == id) else {
            return;
        };
        let e = &self.editor;
        let text = |t: &TextEdit| t.text.trim().to_owned();
        let opt = |s: String| (!s.is_empty()).then_some(s);
        let split = |s: String, sep: char| -> Vec<String> {
            s.split(sep)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut metadata = Metadata {
            title: text(&e.title),
            authors: split(text(&e.authors), '&'),
            series: opt(text(&e.series)),
            series_index: text(&e.series_index).parse().ok(),
            tags: split(text(&e.tags), ','),
            publisher: opt(text(&e.publisher)),
            language: opt(text(&e.language)),
            published: opt(text(&e.published)),
            description: opt(text(&e.description)),
            identifiers: book.metadata.identifiers.clone(),
        };
        match opt(text(&e.isbn)) {
            Some(isbn) => metadata.identifiers.insert("isbn".into(), isbn),
            None => metadata.identifiers.remove("isbn"),
        };
        if metadata.title.is_empty() {
            self.set_error("Title cannot be empty");
            return;
        }
        self.editor.book = None;
        match self.library.update_metadata(id, &metadata) {
            Ok(_) => self.set_info("Metadata saved"),
            Err(e) => self.set_error(format!("Save failed: {e}")),
        }
        self.covers.clear();
        self.reload();
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
pub fn book_key(title: &str, authors: &str) -> String {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    format!("{}|{}", norm(title), norm(authors))
}

pub fn human_size(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_keys_ignore_case_and_punctuation() {
        assert_eq!(
            book_key("The Hobbit!", "J. R. R. Tolkien"),
            book_key("the hobbit", "JRR Tolkien")
        );
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}

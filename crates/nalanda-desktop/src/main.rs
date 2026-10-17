//! Nalanda desktop: a calibre-style ebook manager built on GPUI.
//!
//! Usage: `nalanda [FILES...]`. Files given on the command line are imported.
//! The library folder is the one chosen in the app (default `~/Nalanda Library`);
//! `NALANDA_LIBRARY` overrides it.

mod app;
mod cover;
mod reader;
mod server;
mod settings;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::*;
use settings::Settings;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let settings = Settings::default_path()
        .as_deref()
        .map(Settings::load)
        .unwrap_or_default();
    let default_dir = || {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Nalanda Library"))
            .context("set NALANDA_LIBRARY")
    };
    let mut warning = None;
    let library_dir = match (std::env::var_os("NALANDA_LIBRARY"), &settings.library_dir) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(dir)) if dir.is_dir() => dir.clone(),
        // Don't recreate a chosen folder that is gone, e.g. on an unplugged drive.
        (None, Some(dir)) => {
            warning = Some(format!(
                "Library folder {} not found; opened the default library instead",
                dir.display()
            ));
            default_dir()?
        }
        (None, None) => default_dir()?,
    };
    // Open the catalog and read the first listing while the platform and GPU
    // start up. On a cold disk this is the slowest part of startup.
    let loading = std::thread::Builder::new()
        .name("library-open".into())
        .spawn(move || {
            let library = nalanda_library::Library::open(&library_dir)?;
            // A failed listing is reported in the window, as on any reload.
            let books = library.list(None, app::INITIAL_SORT).ok();
            anyhow::Ok((Arc::new(library), books))
        })?;
    let converters = Arc::new(nalanda_convert::Converters::default());
    let imports: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        prewarm_text(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Nalanda".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let (library, books) = match loading.join().expect("library thread panicked") {
                Ok(loaded) => loaded,
                Err(e) => {
                    eprintln!("Error: {e:?}");
                    std::process::exit(1);
                }
            };
            cx.new(|cx| {
                app::NalandaApp::new(
                    library, books, converters, settings, imports, warning, window, cx,
                )
            })
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}

/// Loads the UI font on a background thread while the window opens.
///
/// The first time cosmic-text matches a set of font attributes it opens every
/// installed font, about 30 ms each. The first layout needs two such matches:
/// the UI font, and the default sans-serif that an empty line (like the empty
/// search box) is shaped with. Shaping a sample line does both here.
fn prewarm_text(cx: &App) {
    use gpui_kit::component::ActiveTheme as _;
    let font = font(cx.theme().font_family.clone());
    let text_system = cx.text_system().clone();
    cx.background_executor()
        .spawn(async move {
            let text_system = WindowTextSystem::new(text_system);
            for text in ["", "Nalanda"] {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                text_system.layout_line(text, px(14.), &[run], None);
            }
        })
        .detach();
}

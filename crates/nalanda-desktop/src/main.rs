//! Nalanda desktop: a calibre-style ebook manager built on GPUI.
//!
//! Usage: `nalanda [FILES...]`. Files given on the command line are imported.
//! `NALANDA_LIBRARY` overrides the library directory (default `~/Nalanda Library`).

mod app;
mod reader;
mod server;
mod settings;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::*;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let library_dir = match std::env::var_os("NALANDA_LIBRARY") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Nalanda Library"))
            .context("set NALANDA_LIBRARY")?,
    };
    let library = Arc::new(nalanda_library::Library::open(&library_dir)?);
    let converters = Arc::new(nalanda_convert::Converters::default());
    let imports: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
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
            cx.new(|cx| app::NalandaApp::new(library, converters, imports, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}

//! Nalanda desktop on Rune: the same app as `nalanda-desktop`, drawn with Rune instead of GPUI.
//!
//! Usage: `nalanda-rune [FILES...]`. Files given on the command line are imported.
//! The library folder is the one chosen in the app (default `~/Nalanda Library`);
//! `NALANDA_LIBRARY` overrides it. Rune's own flags (`--snap`, `--tree`, `--script`, …) render
//! the window headlessly.

mod app;
mod covers;
mod jobs;
mod reader;
#[path = "../../nalanda-desktop/src/server.rs"]
mod server;
#[path = "../../nalanda-desktop/src/settings.rs"]
mod settings;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use rune::AppSpec;
use settings::Settings;

fn main() -> anyhow::Result<()> {
    rune::prewarm_gpu();
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
    // Open the catalog and read the first listing while the platform and GPU start up.
    let loading = std::thread::Builder::new()
        .name("library-open".into())
        .spawn(move || {
            let library = nalanda_library::Library::open(&library_dir)?;
            // A failed listing is reported in the window, as on any reload.
            let books = library.list(None, app::INITIAL_SORT).ok();
            anyhow::Ok((Arc::new(library), books))
        })?;
    let converters = Arc::new(nalanda_convert::Converters::default());
    // Rune takes its own flags (`--snap`, `--tree`, …); what's left are files to import.
    let imports: Vec<PathBuf> = rune::tooling::extra_args()
        .iter()
        .map(PathBuf::from)
        .collect();

    let spec = AppSpec {
        min_size: Some((900.0, 560.0)),
        setup: Some(app::App::setup),
        poll: Some(app::App::poll),
        scenes: app::SCENES,
        ..AppSpec::new("Nalanda", (1280.0, 820.0), app::App::placeholder, view::view)
    };
    rune::App::new(spec)
        .init(move || {
            let (library, books) = match loading.join().expect("library thread panicked") {
                Ok(loaded) => loaded,
                Err(e) => {
                    eprintln!("Error: {e:?}");
                    std::process::exit(1);
                }
            };
            app::App::new(library, books, converters, settings, imports, warning)
        })
        .run();
    Ok(())
}

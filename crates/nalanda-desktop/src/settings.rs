//! User preferences, stored as JSON in the platform config directory
//! (e.g. `~/.config/nalanda/settings.json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// App-wide light/dark mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the operating system, including live changes.
    #[default]
    System,
    Light,
    Dark,
}

/// Colors of the reader page, independent of the app theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReaderTheme {
    /// Use the app theme's colors.
    #[default]
    App,
    Light,
    Sepia,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub reader_theme: ReaderTheme,
    pub reader_font_size: f32,
    /// Library folder chosen in the app; `None` means the default.
    pub library_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            reader_theme: ReaderTheme::App,
            reader_font_size: 18.0,
            library_dir: None,
        }
    }
}

impl Settings {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("nalanda").join("settings.json"))
    }

    /// Loads settings; a missing or unreadable file yields the defaults, and
    /// unknown or missing fields fall back individually.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).expect("settings serialize");
        std::fs::write(path, json)
    }
}

/// Checks that `dir` can be used as the library folder: an existing Nalanda
/// or calibre library, or an empty folder for a new one. This keeps Nalanda
/// from writing its catalog into, say, the home directory.
pub fn check_library_folder(dir: &Path) -> Result<(), String> {
    if nalanda_library::Library::exists_at(dir) {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if entries.next().is_none() {
        return Ok(());
    }
    Err(format!(
        "{} is not empty. Choose an empty folder or an existing Nalanda or calibre library.",
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_partial_or_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(Settings::load(&path), Settings::default());

        let custom = Settings {
            appearance: Appearance::Dark,
            reader_theme: ReaderTheme::Sepia,
            reader_font_size: 22.0,
            library_dir: Some("/books/Library".into()),
        };
        custom.save(&path).unwrap();
        assert_eq!(Settings::load(&path), custom);

        // A file from an older version with only some keys keeps the rest default.
        std::fs::write(&path, r#"{"reader_theme":"dark"}"#).unwrap();
        let partial = Settings::load(&path);
        assert_eq!(partial.reader_theme, ReaderTheme::Dark);
        assert_eq!(partial.appearance, Appearance::System);
        assert_eq!(partial.reader_font_size, 18.0);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn library_folder_must_be_empty_or_an_existing_library() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(check_library_folder(&empty).is_ok());

        let existing = dir.path().join("existing");
        nalanda_library::Library::open(&existing).unwrap();
        assert!(check_library_folder(&existing).is_ok());

        let calibre = dir.path().join("calibre");
        std::fs::create_dir(&calibre).unwrap();
        std::fs::write(calibre.join("metadata.db"), "").unwrap();
        assert!(check_library_folder(&calibre).is_ok());

        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("notes.txt"), "").unwrap();
        assert!(
            check_library_folder(&home)
                .unwrap_err()
                .contains("not empty")
        );
        assert!(!home.join("nalanda.db").exists());

        assert!(check_library_folder(&dir.path().join("missing")).is_err());
    }
}

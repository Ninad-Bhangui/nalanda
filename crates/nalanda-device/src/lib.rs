//! E-reader device support. Currently Kobo, which mounts as USB mass storage
//! and keeps its catalog in `.kobo/KoboReader.sqlite`.
//!
//! Devices are local hardware, so this crate is used by the desktop app
//! directly and is not exposed through the server.

mod kobo;
mod mounts;

pub use kobo::{DeviceBook, KoboDevice, KoboInfo, ReadStatus};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("device database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0} is not a Kobo device")]
    NotAKobo(std::path::PathBuf),
    #[error("{0} is not a sideloaded book and cannot be deleted from here")]
    NotSideloaded(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Scans mounted volumes for connected Kobo e-readers.
pub fn detect_kobo_devices() -> Vec<KoboDevice> {
    mounts::candidate_mount_points()
        .into_iter()
        .filter_map(|mount| KoboDevice::open(&mount).ok())
        .collect()
}

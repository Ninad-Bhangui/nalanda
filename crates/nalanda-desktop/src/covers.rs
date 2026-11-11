//! Downscaled cover thumbnails for the library grid and the details panel.
//!
//! Covers are often 1500×2400 or more, so a screen of them at full size would mean hundreds of
//! megabytes of textures. A worker resolves the cover file, decodes it and keeps only a
//! thumbnail sized to the box it is shown in, then hands it to a `rune::Pixels`, which wakes
//! the window by itself. The view never touches the disk.
//!
//! The cache is bounded in decoded bytes and evicts the least recently drawn thumbnails.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use image::imageops::FilterType;
use nalanda_core::BookId;
use nalanda_library::Library;
use rune::Pixels;

/// Decoded thumbnails kept at most, in bytes of RGBA.
const BUDGET_BYTES: usize = 96 << 20;
const WORKERS: usize = 3;

struct Request {
    library: Arc<Library>,
    id: BookId,
    width: u32,
    height: u32,
    pixels: Pixels,
}

struct Entry {
    pixels: Pixels,
    bytes: usize,
    used: u64,
}

pub struct Covers {
    entries: RefCell<HashMap<(BookId, u32, u32), Entry>>,
    clock: RefCell<u64>,
    bytes: RefCell<usize>,
    queue: Sender<Request>,
}

impl Covers {
    pub fn new() -> Self {
        let (tx, rx) = channel::<Request>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("cover-{i}"))
                .spawn(move || {
                    loop {
                        let next = rx.lock().unwrap().recv();
                        let Ok(request) = next else { break };
                        if let Some((w, h, rgba)) = decode(&request) {
                            request.pixels.set(w, h, rgba);
                        }
                    }
                })
                .expect("spawn cover worker");
        }
        Self {
            entries: RefCell::default(),
            clock: RefCell::new(0),
            bytes: RefCell::new(0),
            queue: tx,
        }
    }

    /// Forgets every thumbnail, e.g. after a reload that may have changed covers.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        *self.bytes.borrow_mut() = 0;
    }

    /// The thumbnail of book `id` at `w`×`h` device pixels. Empty until the worker fills it.
    pub fn get(&self, library: &Arc<Library>, id: BookId, w: u32, h: u32) -> Pixels {
        let mut clock = self.clock.borrow_mut();
        *clock += 1;
        let now = *clock;
        let mut entries = self.entries.borrow_mut();
        if let Some(entry) = entries.get_mut(&(id, w, h)) {
            entry.used = now;
            return entry.pixels.clone();
        }
        let pixels = Pixels::new();
        let bytes = (w * h * 4) as usize;
        let _ = self.queue.send(Request {
            library: library.clone(),
            id,
            width: w,
            height: h,
            pixels: pixels.clone(),
        });
        entries.insert(
            (id, w, h),
            Entry {
                pixels: pixels.clone(),
                bytes,
                used: now,
            },
        );
        let mut total = self.bytes.borrow_mut();
        *total += bytes;
        while *total > BUDGET_BYTES && entries.len() > 1 {
            let oldest = entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
                .expect("non-empty");
            if let Some(e) = entries.remove(&oldest) {
                *total -= e.bytes;
            }
        }
        pixels
    }
}

fn decode(r: &Request) -> Option<(u32, u32, Vec<u8>)> {
    let path = r.library.cover_path(r.id).ok().flatten()?;
    let image = image::ImageReader::open(&path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    // Fill the box (the view crops with `SurfaceFit::Fill`), but never upscale.
    let image = if image.width() > r.width && image.height() > r.height {
        image.resize_to_fill(r.width, r.height, FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

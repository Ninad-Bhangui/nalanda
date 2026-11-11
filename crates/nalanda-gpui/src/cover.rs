//! Downscaled cover thumbnails for the library grid.
//!
//! `img(path)` decodes covers at full size (often 1500×2400 or more), so a
//! screen of them means hundreds of megabytes of textures to upload and draw.
//! This loader decodes off the main thread and keeps only a thumbnail sized
//! to the box it is shown in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::*;
use image::{Frame, imageops::FilterType};
use smallvec::smallvec;

/// Thumbnails are decoded at this multiple of their layout size so they stay
/// sharp on HiDPI screens.
const SCALE: f32 = 2.;

pub enum CoverThumb {}

/// The cover file, its mtime (so a replaced cover is decoded afresh) and the
/// pixel size to fit it in.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct CoverSource {
    path: PathBuf,
    modified: SystemTime,
    width: u32,
    height: u32,
}

impl CoverThumb {
    pub fn source(path: PathBuf, modified: SystemTime, w: Pixels, h: Pixels) -> CoverSource {
        CoverSource {
            path,
            modified,
            width: (f32::from(w) * SCALE).ceil() as u32,
            height: (f32::from(h) * SCALE).ceil() as u32,
        }
    }
}

impl Asset for CoverThumb {
    type Source = CoverSource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let task = cx.background_executor().spawn(async move { decode(&source) });
        async move { task.await }
    }
}

fn decode(source: &CoverSource) -> Result<Arc<RenderImage>, ImageCacheError> {
    let image = image::ImageReader::open(&source.path)
        .map_err(|e| ImageCacheError::Io(Arc::new(e)))?
        .with_guessed_format()
        .map_err(|e| ImageCacheError::Io(Arc::new(e)))?
        .decode()
        .map_err(|e| ImageCacheError::Image(Arc::new(e)))?;
    // Fill the box (the grid crops with `ObjectFit::Cover`), but never upscale.
    let image = if image.width() > source.width && image.height() > source.height {
        image.resize_to_fill(source.width, source.height, FilterType::Triangle)
    } else {
        image
    };
    let mut buffer = image.into_rgba8();
    // GPUI expects BGRA.
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])))
}

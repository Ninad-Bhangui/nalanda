use std::path::Path;

use nalanda_core::Metadata;

use crate::Result;

/// Reads the PDF document information dictionary.
///
/// Cover thumbnails need a PDF rasterizer (e.g. pdfium) and are not extracted yet.
pub(crate) fn read_metadata(path: &Path) -> Result<Metadata> {
    let doc = lopdf::Document::load(path)?;
    let mut metadata = Metadata::default();
    let Ok(info) = doc.trailer.get(b"Info") else {
        return Ok(metadata);
    };
    let info = match info {
        lopdf::Object::Reference(id) => doc.get_dictionary(*id)?,
        lopdf::Object::Dictionary(d) => d,
        _ => return Ok(metadata),
    };
    let text = |key: &[u8]| -> Option<String> {
        let obj = info.get(key).ok()?;
        let obj = match obj {
            lopdf::Object::Reference(id) => doc.get_object(*id).ok()?,
            o => o,
        };
        let s = decode_pdf_string(obj.as_str().ok()?);
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_owned())
    };
    metadata.title = text(b"Title").unwrap_or_default();
    if let Some(author) = text(b"Author") {
        metadata.authors = split_authors(&author);
    }
    if let Some(subject) = text(b"Subject") {
        metadata.description = Some(subject);
    }
    if let Some(keywords) = text(b"Keywords") {
        metadata.tags = keywords
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    Ok(metadata)
}

/// PDF text strings are UTF-16BE with a BOM, or PDFDocEncoding (≈ Latin-1).
fn decode_pdf_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn split_authors(s: &str) -> Vec<String> {
    s.split(['&', ';'])
        .flat_map(|part| part.split(" and "))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16_and_latin1() {
        let utf16 = [0xFE, 0xFF, 0x00, 0x45, 0x00, 0xE9];
        assert_eq!(decode_pdf_string(&utf16), "Eé");
        assert_eq!(decode_pdf_string(b"Caf\xe9"), "Café");
    }

    #[test]
    fn splits_author_lists() {
        assert_eq!(
            split_authors("A B & C D; E and F"),
            ["A B", "C D", "E", "F"]
        );
    }
}

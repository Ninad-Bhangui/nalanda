//! EPUB 2/3 reading (OPF metadata, cover, spine) and a minimal EPUB 3 writer.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use nalanda_core::{BookFormat, Metadata};

use crate::{Chapter, Cover, Document, Error, Result};

#[derive(Debug, Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

/// An opened EPUB. The archive is read eagerly into memory-light indexes; file
/// contents are fetched on demand.
pub struct Epub {
    archive: zip::ZipArchive<File>,
    /// Directory of the OPF inside the archive, used to resolve hrefs.
    base: String,
    metadata: Metadata,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    cover_id: Option<String>,
    /// Table of contents in reading order: (file, fragment, title).
    toc: Vec<(String, Option<String>, String)>,
}

fn malformed(reason: impl Into<String>) -> Error {
    Error::Malformed {
        format: BookFormat::Epub,
        reason: reason.into(),
    }
}

impl Epub {
    pub fn open(path: &Path) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(File::open(path)?)?;
        let container = read_string(&mut archive, "META-INF/container.xml")?;
        let opf_path = {
            let doc = roxmltree::Document::parse(&container)?;
            doc.descendants()
                .find(|n| n.has_tag_name("rootfile"))
                .and_then(|n| n.attribute("full-path"))
                .ok_or_else(|| malformed("container.xml has no rootfile"))?
                .to_owned()
        };
        let base = match opf_path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/"),
            None => String::new(),
        };
        let opf = read_string(&mut archive, &opf_path)?;
        let mut epub = Epub {
            archive,
            base,
            metadata: Metadata::default(),
            manifest: HashMap::new(),
            spine: Vec::new(),
            cover_id: None,
            toc: Vec::new(),
        };
        let ncx_id = epub.parse_opf(&opf)?;
        epub.load_toc(ncx_id);
        Ok(epub)
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Returns the NCX id from the spine `toc` attribute, if any.
    fn parse_opf(&mut self, opf: &str) -> Result<Option<String>> {
        let doc = roxmltree::Document::parse(opf)?;
        let meta = &mut self.metadata;
        let mut epub2_cover = None;
        // EPUB 3 refinements: `<meta refines="#id" property="...">`.
        let mut refines: HashMap<String, Vec<(String, String)>> = HashMap::new();
        if let Some(md) = doc.descendants().find(|n| n.has_tag_name("metadata")) {
            for n in md.children().filter(|n| n.is_element()) {
                if n.tag_name().name() == "meta"
                    && let (Some(r), Some(p)) = (n.attribute("refines"), n.attribute("property"))
                {
                    refines
                        .entry(r.trim_start_matches('#').to_owned())
                        .or_default()
                        .push((p.to_owned(), text_of(&n)));
                }
            }
            for n in md.children().filter(|n| n.is_element()) {
                let text = text_of(&n);
                let attr = |name: &str| {
                    n.attributes()
                        .find(|a| a.name() == name)
                        .map(|a| a.value().to_owned())
                };
                match n.tag_name().name() {
                    "title" if meta.title.is_empty() => meta.title = text,
                    "creator" => {
                        let role = attr("role").or_else(|| {
                            let id = attr("id")?;
                            refines
                                .get(&id)?
                                .iter()
                                .find_map(|(p, v)| (p == "role").then(|| v.clone()))
                        });
                        if role.as_deref().is_none_or(|r| r == "aut") && !text.is_empty() {
                            meta.authors.push(text);
                        }
                    }
                    "publisher" => meta.publisher = Some(text).filter(|s| !s.is_empty()),
                    "language" if meta.language.is_none() => meta.language = Some(text),
                    "date" if meta.published.is_none() => meta.published = Some(text),
                    "description" => meta.description = Some(text).filter(|s| !s.is_empty()),
                    "subject" if !text.is_empty() => meta.tags.push(text),
                    "identifier" => {
                        if let Some((scheme, value)) = parse_identifier(attr("scheme"), &text) {
                            meta.identifiers.entry(scheme).or_insert(value);
                        }
                    }
                    "meta" => match (attr("name").as_deref(), attr("content")) {
                        (Some("cover"), Some(id)) => epub2_cover = Some(id),
                        (Some("calibre:series"), Some(s)) => meta.series = Some(s),
                        (Some("calibre:series_index"), Some(i)) => {
                            meta.series_index = i.parse().ok()
                        }
                        _ => match attr("property").as_deref() {
                            Some("belongs-to-collection") if meta.series.is_none() => {
                                meta.series = Some(text.clone());
                                if let Some(id) = attr("id") {
                                    meta.series_index = refines.get(&id).and_then(|r| {
                                        r.iter().find_map(|(p, v)| {
                                            (p == "group-position").then(|| v.parse().ok())?
                                        })
                                    });
                                }
                            }
                            _ => {}
                        },
                    },
                    _ => {}
                }
            }
        }

        for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
            let (Some(id), Some(href)) = (item.attribute("id"), item.attribute("href")) else {
                continue;
            };
            self.manifest.insert(
                id.to_owned(),
                ManifestItem {
                    href: percent_decode(href),
                    media_type: item.attribute("media-type").unwrap_or_default().to_owned(),
                    properties: item.attribute("properties").unwrap_or_default().to_owned(),
                },
            );
        }
        let spine_node = doc.descendants().find(|n| n.has_tag_name("spine"));
        self.spine = spine_node
            .iter()
            .flat_map(|s| s.children())
            .filter(|n| n.has_tag_name("itemref") && n.attribute("linear") != Some("no"))
            .filter_map(|n| n.attribute("idref").map(str::to_owned))
            .collect();

        self.cover_id = self
            .manifest
            .iter()
            .find(|(_, i)| i.properties.split_whitespace().any(|p| p == "cover-image"))
            .map(|(id, _)| id.clone())
            .or(epub2_cover.filter(|id| self.manifest.contains_key(id)))
            .or_else(|| {
                // Last resort: an image whose id or file name says "cover".
                self.manifest
                    .iter()
                    .filter(|(_, i)| i.media_type.starts_with("image/"))
                    .find(|(id, i)| {
                        id.to_lowercase().contains("cover")
                            || i.href.to_lowercase().contains("cover")
                    })
                    .map(|(id, _)| id.clone())
            });
        Ok(spine_node
            .and_then(|s| s.attribute("toc"))
            .map(str::to_owned))
    }

    /// Chapter titles from the EPUB 3 nav document or the EPUB 2 NCX.
    fn load_toc(&mut self, ncx_id: Option<String>) {
        let nav = self
            .manifest
            .values()
            .find(|i| i.properties.split_whitespace().any(|p| p == "nav"))
            .cloned();
        let ncx = ncx_id
            .and_then(|id| self.manifest.get(&id).cloned())
            .or_else(|| {
                self.manifest
                    .values()
                    .find(|i| i.media_type == "application/x-dtbncx+xml")
                    .cloned()
            });
        let mut entries: Vec<(String, String)> = Vec::new();
        if let Some(item) = nav
            && let Ok(html) = self.read_item_string(&item.href)
        {
            let doc = scraper::Html::parse_document(&html);
            let navs = scraper::Selector::parse("nav").expect("valid selector");
            let links = scraper::Selector::parse("a[href]").expect("valid selector");
            // Only the `epub:type="toc"` nav; skip page lists and landmarks.
            let is_toc = |nav: &scraper::ElementRef<'_>| {
                nav.value().attrs().any(|(name, value)| {
                    name.ends_with("type") && value.split_whitespace().any(|v| v == "toc")
                })
            };
            let toc_nav = doc
                .select(&navs)
                .find(is_toc)
                .or_else(|| doc.select(&navs).next());
            for a in toc_nav.iter().flat_map(|nav| nav.select(&links)) {
                let title = a.text().collect::<String>().trim().to_owned();
                entries.push((
                    resolve(&item.href, a.value().attr("href").unwrap_or("")),
                    title,
                ));
            }
        } else if let Some(item) = ncx
            && let Ok(xml) = self.read_item_string(&item.href)
            && let Ok(doc) = roxmltree::Document::parse(&xml)
        {
            for point in doc.descendants().filter(|n| n.has_tag_name("navPoint")) {
                let title = point
                    .descendants()
                    .find(|n| n.has_tag_name("text"))
                    .map(|n| text_of(&n))
                    .unwrap_or_default();
                if let Some(src) = point
                    .children()
                    .find(|n| n.has_tag_name("content"))
                    .and_then(|n| n.attribute("src"))
                {
                    entries.push((resolve(&item.href, src), title));
                }
            }
        }
        for (target, title) in entries {
            if title.is_empty() {
                continue;
            }
            let (file, fragment) = match target.split_once('#') {
                Some((f, frag)) => (
                    f.to_owned(),
                    Some(frag.to_owned()).filter(|s| !s.is_empty()),
                ),
                None => (target, None),
            };
            self.toc.push((file, fragment, title));
        }
    }

    pub fn cover(&mut self) -> Result<Option<Cover>> {
        let Some(item) = self
            .cover_id
            .as_ref()
            .and_then(|id| self.manifest.get(id))
            .cloned()
        else {
            return Ok(None);
        };
        let data = read_bytes(&mut self.archive, &format!("{}{}", self.base, item.href))?;
        let media_type = if item.media_type.is_empty() {
            "image/jpeg".to_owned()
        } else {
            item.media_type.clone()
        };
        Ok(Some(Cover { data, media_type }))
    }

    /// Spine documents converted to [`Chapter`]s, titled from the TOC.
    pub fn document(mut self) -> Result<Document> {
        let mut chapters = Vec::new();
        for id in self.spine.clone() {
            let Some(item) = self.manifest.get(&id).cloned() else {
                continue;
            };
            if !item.media_type.contains("html") {
                continue;
            }
            let html = self.read_item_string(&item.href)?;
            let entries = self.toc.iter().filter(|(file, ..)| *file == item.href);
            // A TOC entry for the whole file titles its start; entries with a
            // fragment split the file into separate chapters.
            let mut title = None;
            let mut anchors = Vec::new();
            for (_, fragment, t) in entries {
                match fragment {
                    None => title = title.or(Some(t.clone())),
                    Some(id) if anchors.is_empty() && title.is_none() && starts_file(&html, id) => {
                        title = Some(t.clone())
                    }
                    Some(id) => anchors.push((id.clone(), t.clone())),
                }
            }
            chapters.extend(
                crate::html::chapters_from_html(&html, title, &anchors)
                    .into_iter()
                    .filter(|c| !c.blocks.is_empty()),
            );
        }
        Ok(Document {
            title: self.metadata.title.clone(),
            chapters,
        })
    }

    fn read_item_string(&mut self, href: &str) -> Result<String> {
        read_string(&mut self.archive, &format!("{}{}", self.base, href))
    }
}

/// True when the element `id` comes before any text in the file, so the TOC
/// entry pointing at it is effectively a link to the whole file.
fn starts_file(html: &str, id: &str) -> bool {
    let Some(body) = html.find("<body") else {
        return false;
    };
    let needles = [format!("id=\"{id}\""), format!("id='{id}'")];
    let Some(pos) = needles
        .iter()
        .filter_map(|n| html[body..].find(n.as_str()))
        .min()
    else {
        return false;
    };
    let before = &html[body..body + pos];
    // Strip tags; anything left that is not whitespace is text before the anchor.
    let mut in_tag = false;
    !before.chars().any(|c| {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag && !c.is_whitespace() => return true,
            _ => {}
        }
        false
    })
}

fn text_of(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_identifier(scheme_attr: Option<String>, text: &str) -> Option<(String, String)> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    for prefix in ["urn:isbn:", "isbn:"] {
        if lower.starts_with(prefix) {
            return Some(("isbn".into(), text[prefix.len()..].to_owned()));
        }
    }
    if lower.starts_with("urn:uuid:") {
        return Some(("uuid".into(), text["urn:uuid:".len()..].to_owned()));
    }
    let scheme = scheme_attr?.to_ascii_lowercase();
    Some((scheme, text.to_owned()))
}

/// Resolves `href` relative to the document at `from` (both archive-relative to the OPF dir).
fn resolve(from: &str, href: &str) -> String {
    let href = percent_decode(href);
    let mut parts: Vec<&str> = match from.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').collect(),
        None => Vec::new(),
    };
    for seg in href.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_bytes<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| malformed(format!("missing `{name}`")))?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_string<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Result<String> {
    let bytes = read_bytes(archive, name)?;
    let s = String::from_utf8_lossy(&bytes);
    Ok(s.trim_start_matches('\u{feff}').to_owned())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Writes a minimal, valid EPUB 3 (with an EPUB 2 NCX for older readers).
pub fn write_epub(
    path: &Path,
    metadata: &Metadata,
    chapters: &[Chapter],
    cover: Option<&Cover>,
) -> Result<()> {
    use zip::write::SimpleFileOptions;

    let mut zip = zip::ZipWriter::new(File::create(path)?);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default();

    // The mimetype entry must come first and be uncompressed.
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", deflated)?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )?;

    let uid = metadata
        .identifiers
        .get("uuid")
        .cloned()
        .unwrap_or_else(|| pseudo_uuid(metadata));
    let title = |i: usize, c: &Chapter| {
        c.display_title()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Chapter {}", i + 1))
    };

    let mut manifest = String::new();
    let mut spine = String::new();
    let mut nav_items = String::new();
    let mut ncx_points = String::new();
    for (i, chapter) in chapters.iter().enumerate() {
        let name = format!("chapter{:03}.xhtml", i + 1);
        let t = esc(&title(i, chapter));
        zip.start_file(format!("OEBPS/{name}"), deflated)?;
        zip.write_all(chapter_xhtml(&t, chapter).as_bytes())?;
        manifest.push_str(&format!(
            r#"    <item id="c{i}" href="{name}" media-type="application/xhtml+xml"/>
"#
        ));
        spine.push_str(&format!("    <itemref idref=\"c{i}\"/>\n"));
        nav_items.push_str(&format!("      <li><a href=\"{name}\">{t}</a></li>\n"));
        ncx_points.push_str(&format!(
            r#"    <navPoint id="n{i}" playOrder="{}"><navLabel><text>{t}</text></navLabel><content src="{name}"/></navPoint>
"#,
            i + 1
        ));
    }
    let mut cover_meta = String::new();
    if let Some(cover) = cover {
        let name = format!("cover.{}", cover.extension());
        zip.start_file(format!("OEBPS/{name}"), stored)?;
        zip.write_all(&cover.data)?;
        manifest.push_str(&format!(
            "    <item id=\"cover-image\" href=\"{name}\" media-type=\"{}\" properties=\"cover-image\"/>\n",
            esc(&cover.media_type)
        ));
        cover_meta = "    <meta name=\"cover\" content=\"cover-image\"/>\n".to_owned();
    }

    let mut dc = format!(
        "    <dc:identifier id=\"uid\">urn:uuid:{}</dc:identifier>\n    <dc:title>{}</dc:title>\n    <dc:language>{}</dc:language>\n",
        esc(&uid),
        esc(&metadata.title),
        esc(metadata.language.as_deref().unwrap_or("en")),
    );
    for author in &metadata.authors {
        dc.push_str(&format!("    <dc:creator>{}</dc:creator>\n", esc(author)));
    }
    let optional = [
        ("publisher", &metadata.publisher),
        ("description", &metadata.description),
        ("date", &metadata.published),
    ];
    for (tag, value) in optional {
        if let Some(v) = value {
            dc.push_str(&format!("    <dc:{tag}>{}</dc:{tag}>\n", esc(v)));
        }
    }
    for tag in &metadata.tags {
        dc.push_str(&format!("    <dc:subject>{}</dc:subject>\n", esc(tag)));
    }
    if let Some(isbn) = metadata.isbn() {
        dc.push_str(&format!(
            "    <dc:identifier>urn:isbn:{}</dc:identifier>\n",
            esc(isbn)
        ));
    }
    if let Some(series) = &metadata.series {
        dc.push_str(&format!(
            "    <meta name=\"calibre:series\" content=\"{}\"/>\n",
            esc(series)
        ));
        if let Some(idx) = metadata.series_index {
            dc.push_str(&format!(
                "    <meta name=\"calibre:series_index\" content=\"{idx}\"/>\n"
            ));
        }
    }
    let modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");

    zip.start_file("OEBPS/content.opf", deflated)?;
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
{dc}    <meta property="dcterms:modified">{modified}</meta>
{cover_meta}  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
{manifest}  </manifest>
  <spine toc="ncx">
{spine}  </spine>
</package>
"#
        )
        .as_bytes(),
    )?;

    zip.start_file("OEBPS/nav.xhtml", deflated)?;
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Contents</title></head>
<body>
  <nav epub:type="toc"><ol>
{nav_items}  </ol></nav>
</body>
</html>
"#
        )
        .as_bytes(),
    )?;

    zip.start_file("OEBPS/toc.ncx", deflated)?;
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head><meta name="dtb:uid" content="urn:uuid:{}"/></head>
  <docTitle><text>{}</text></docTitle>
  <navMap>
{ncx_points}  </navMap>
</ncx>
"#,
            esc(&uid),
            esc(&metadata.title)
        )
        .as_bytes(),
    )?;

    zip.finish()?;
    Ok(())
}

fn chapter_xhtml(title: &str, chapter: &Chapter) -> String {
    use crate::Block;
    let mut body = String::new();
    for block in &chapter.blocks {
        let html = match block {
            Block::Heading { level, text } => {
                let l = (*level).clamp(1, 6);
                format!("<h{l}>{}</h{l}>", esc(text))
            }
            Block::Paragraph(t) => format!("<p>{}</p>", esc(t).replace('\n', "<br/>")),
            Block::Quote(t) => format!(
                "<blockquote><p>{}</p></blockquote>",
                esc(t).replace('\n', "<br/>")
            ),
            Block::Preformatted(t) => format!("<pre>{}</pre>", esc(t)),
            Block::Separator => "<hr/>".to_owned(),
        };
        body.push_str("  ");
        body.push_str(&html);
        body.push('\n');
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>{title}</title></head>
<body>
{body}</body>
</html>
"#
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Deterministic UUID-shaped id derived from title + authors, so re-converting
/// the same book yields the same identifier.
fn pseudo_uuid(m: &Metadata) -> String {
    use std::hash::{Hash, Hasher};
    let mut h1 = std::collections::hash_map::DefaultHasher::new();
    (&m.title, &m.authors).hash(&mut h1);
    let a = h1.finish();
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    (a, "nalanda").hash(&mut h2);
    let b = h2.finish();
    let hex = format!("{a:016x}{b:016x}");
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    #[test]
    fn resolves_relative_hrefs() {
        assert_eq!(resolve("text/nav.xhtml", "ch1.xhtml#a"), "text/ch1.xhtml#a");
        assert_eq!(
            resolve("nav/nav.xhtml", "../text/ch%201.xhtml"),
            "text/ch 1.xhtml"
        );
        assert_eq!(resolve("toc.ncx", "ch1.xhtml"), "ch1.xhtml");
    }

    #[test]
    fn identifiers_are_classified() {
        assert_eq!(
            parse_identifier(None, "urn:isbn:978-0"),
            Some(("isbn".into(), "978-0".into()))
        );
        assert_eq!(
            parse_identifier(Some("ISBN".into()), "123"),
            Some(("isbn".into(), "123".into()))
        );
        assert_eq!(parse_identifier(None, "plain"), None);
    }

    #[test]
    fn written_epub_round_trips_through_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        let mut metadata = Metadata {
            title: "Tom & Jerry <Collected>".into(),
            authors: vec!["Jane Doe".into(), "John Roe".into()],
            series: Some("Cartoons".into()),
            series_index: Some(2.5),
            publisher: Some("Acme".into()),
            language: Some("fr".into()),
            description: Some("A \"quoted\" description".into()),
            tags: vec!["Humor".into(), "Classics".into()],
            ..Default::default()
        };
        metadata
            .identifiers
            .insert("isbn".into(), "9780000000002".into());
        let chapters = vec![
            Chapter {
                title: Some("Opening".into()),
                blocks: vec![
                    Block::Heading {
                        level: 1,
                        text: "Opening".into(),
                    },
                    Block::Paragraph("Cats & mice\nchase.".into()),
                ],
            },
            Chapter {
                title: None,
                blocks: vec![Block::Quote("Indeed".into()), Block::Separator],
            },
        ];
        let cover = Cover {
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
            media_type: "image/png".into(),
        };
        write_epub(&path, &metadata, &chapters, Some(&cover)).unwrap();

        let mut epub = Epub::open(&path).unwrap();
        let read = epub.metadata().clone();
        assert_eq!(read.title, metadata.title);
        assert_eq!(read.authors, metadata.authors);
        assert_eq!(read.series, metadata.series);
        assert_eq!(read.series_index, Some(2.5));
        assert_eq!(read.publisher, metadata.publisher);
        assert_eq!(read.language.as_deref(), Some("fr"));
        assert_eq!(read.description, metadata.description);
        assert_eq!(read.tags, metadata.tags);
        assert_eq!(read.isbn(), Some("9780000000002"));
        assert_eq!(epub.cover().unwrap(), Some(cover));

        let doc = epub.document().unwrap();
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Opening"));
        assert_eq!(doc.chapters[0].blocks, chapters[0].blocks);
        // Untitled chapters get a generated TOC entry.
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter 2"));
        assert_eq!(doc.chapters[1].blocks, chapters[1].blocks);
    }
}

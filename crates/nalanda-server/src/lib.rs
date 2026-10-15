//! HTTP API over a [`Library`].
//!
//! | Method   | Path                              | Purpose                      |
//! | -------- | --------------------------------- | ---------------------------- |
//! | `GET`    | `/api/books?q=&sort=`             | list / search books          |
//! | `POST`   | `/api/books`                      | upload (multipart `file`)    |
//! | `GET`    | `/api/books/{id}`                 | book details                 |
//! | `PATCH`  | `/api/books/{id}`                 | replace metadata (JSON)      |
//! | `DELETE` | `/api/books/{id}`                 | remove book                  |
//! | `GET`    | `/api/books/{id}/cover`           | cover image                  |
//! | `GET`    | `/api/books/{id}/formats/{fmt}`   | download a format            |
//! | `POST`   | `/api/books/{id}/convert`         | convert (`ConvertRequest`)   |
//! | `GET`    | `/opds`                           | OPDS 1.2 catalog             |
//!
//! OPDS makes the library browsable from existing reader apps (KOReader,
//! Moon+ Reader, ...) before a dedicated mobile client exists.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use nalanda_convert::{Converters, LibraryConvertError};
use nalanda_core::{Book, BookFormat, BookId, ConvertRequest, Metadata};
use nalanda_library::{Library, SortBy};
use serde::Deserialize;

#[derive(Clone)]
pub struct AppState {
    pub library: Arc<Library>,
    pub converters: Arc<Converters>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/books", get(list_books).post(upload_book))
        .route(
            "/api/books/{id}",
            get(get_book).patch(update_book).delete(delete_book),
        )
        .route("/api/books/{id}/cover", get(get_cover))
        .route("/api/books/{id}/formats/{format}", get(download_format))
        .route("/api/books/{id}/convert", post(convert_book))
        .route("/opds", get(opds_feed))
        .layer(DefaultBodyLimit::max(512 * 1024 * 1024))
        .with_state(state)
}

pub use axum::Router;

/// Serves the API until the process receives Ctrl-C.
pub async fn serve(addr: std::net::SocketAddr, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serving library on http://{}", listener.local_addr()?);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Serves `app` on an already-bound listener until the runtime shuts down.
pub async fn serve_listener(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(listener, app).await
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<nalanda_library::Error> for ApiError {
    fn from(e: nalanda_library::Error) -> Self {
        use nalanda_library::Error::*;
        let status = match &e {
            NotFound(_) | MissingFormat(..) => StatusCode::NOT_FOUND,
            Format(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, e.to_string())
    }
}

impl From<LibraryConvertError> for ApiError {
    fn from(e: LibraryConvertError) -> Self {
        match e {
            LibraryConvertError::Library(e) => e.into(),
            LibraryConvertError::Convert(e @ nalanda_convert::Error::Unsupported { .. }) => {
                ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
            }
            LibraryConvertError::Convert(e) => {
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Runs blocking library work off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    sort: Option<String>,
}

fn parse_sort(s: Option<&str>) -> SortBy {
    match s {
        Some("title") => SortBy::Title,
        Some("author") => SortBy::Author,
        _ => SortBy::RecentlyAdded,
    }
}

async fn list_books(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<Book>>> {
    blocking(move || {
        let sort = parse_sort(q.sort.as_deref());
        Ok(Json(s.library.list(q.q.as_deref(), sort)?))
    })
    .await
}

async fn get_book(State(s): State<AppState>, Path(id): Path<BookId>) -> ApiResult<Json<Book>> {
    blocking(move || Ok(Json(s.library.get(id)?))).await
}

async fn update_book(
    State(s): State<AppState>,
    Path(id): Path<BookId>,
    Json(metadata): Json<Metadata>,
) -> ApiResult<Json<Book>> {
    blocking(move || Ok(Json(s.library.update_metadata(id, &metadata)?))).await
}

async fn delete_book(State(s): State<AppState>, Path(id): Path<BookId>) -> ApiResult<StatusCode> {
    blocking(move || {
        s.library.remove_book(id)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn upload_book(
    State(s): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Book>)> {
    let bad = |m: String| ApiError(StatusCode::BAD_REQUEST, m);
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let name = field
            .file_name()
            .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n).to_owned())
            .filter(|n| BookFormat::from_path(n).is_some())
            .ok_or_else(|| bad("`file` needs a file name with an ebook extension".into()))?;
        let data = field.bytes().await.map_err(|e| bad(e.to_string()))?;
        return blocking(move || {
            let dir = tempfile::tempdir()
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let path = dir.path().join(name);
            std::fs::write(&path, &data)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok((StatusCode::CREATED, Json(s.library.add_book(&path)?)))
        })
        .await;
    }
    Err(bad("missing multipart field `file`".into()))
}

async fn send_file(
    path: std::path::PathBuf,
    content_type: &str,
    download_name: Option<String>,
) -> ApiResult<Response> {
    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string()))?;
    let mut res = Response::new(Body::from(data));
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(name) = download_name {
        // ASCII fallback plus RFC 5987 UTF-8 name.
        let ascii: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_graphic() || c == ' ' {
                    c
                } else {
                    '_'
                }
            })
            .filter(|c| *c != '"')
            .collect();
        let encoded: String = name
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        if let Ok(v) = HeaderValue::from_str(&format!(
            "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
        )) {
            res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(res)
}

async fn get_cover(State(s): State<AppState>, Path(id): Path<BookId>) -> ApiResult<Response> {
    let path = blocking(move || {
        s.library
            .cover_path(id)?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("book {id} has no cover")))
    })
    .await?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => "image/jpeg",
    };
    send_file(path, mime, None).await
}

async fn download_format(
    State(s): State<AppState>,
    Path((id, format)): Path<(BookId, String)>,
) -> ApiResult<Response> {
    let format: BookFormat = format.parse().map_err(|e: nalanda_core::UnknownFormat| {
        ApiError(StatusCode::BAD_REQUEST, e.to_string())
    })?;
    let path = blocking(move || Ok(s.library.format_path(id, format)?)).await?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    send_file(path, format.mime_type(), name).await
}

async fn convert_book(
    State(s): State<AppState>,
    Path(id): Path<BookId>,
    Json(req): Json<ConvertRequest>,
) -> ApiResult<Json<Book>> {
    blocking(move || {
        nalanda_convert::convert_library_book(&s.library, &s.converters, id, req.to, req.from)?;
        Ok(Json(s.library.get(id)?))
    })
    .await
}

#[derive(Deserialize)]
struct OpdsQuery {
    q: Option<String>,
}

async fn opds_feed(State(s): State<AppState>, Query(q): Query<OpdsQuery>) -> ApiResult<Response> {
    let books =
        blocking(move || Ok(s.library.list(q.q.as_deref(), SortBy::RecentlyAdded)?)).await?;
    let mut res = Response::new(Body::from(opds::acquisition_feed(&books)));
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/atom+xml;profile=opds-catalog;kind=acquisition"),
    );
    Ok(res)
}

mod opds {
    use nalanda_core::Book;

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    pub fn acquisition_feed(books: &[Book]) -> String {
        let updated = books
            .iter()
            .map(|b| b.modified_at)
            .max()
            .unwrap_or_default()
            .to_rfc3339();
        let mut out = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <id>urn:nalanda:library</id>
  <title>Nalanda Library</title>
  <updated>{updated}</updated>
  <link rel="self" href="/opds" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  <link rel="search" href="/opds?q={{searchTerms}}" type="application/atom+xml"/>
"#
        );
        for b in books {
            let m = &b.metadata;
            out.push_str(&format!(
                "  <entry>\n    <id>urn:nalanda:book:{}</id>\n    <title>{}</title>\n    <updated>{}</updated>\n",
                b.id,
                esc(&m.title),
                b.modified_at.to_rfc3339()
            ));
            for a in &m.authors {
                out.push_str(&format!("    <author><name>{}</name></author>\n", esc(a)));
            }
            if let Some(d) = &m.description {
                out.push_str(&format!("    <summary>{}</summary>\n", esc(d)));
            }
            if b.has_cover {
                let href = format!("/api/books/{}/cover", b.id);
                out.push_str(&format!(
                    "    <link rel=\"http://opds-spec.org/image\" href=\"{href}\"/>\n    <link rel=\"http://opds-spec.org/image/thumbnail\" href=\"{href}\"/>\n"
                ));
            }
            for f in &b.formats {
                out.push_str(&format!(
                    "    <link rel=\"http://opds-spec.org/acquisition\" href=\"/api/books/{}/formats/{}\" type=\"{}\"/>\n",
                    b.id,
                    f.format.extension(),
                    esc(f.format.mime_type())
                ));
            }
            out.push_str("  </entry>\n");
        }
        out.push_str("</feed>\n");
        out
    }
}

#[cfg(test)]
mod tests;

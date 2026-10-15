use super::*;
use axum::body::to_bytes;
use axum::http::Request;
use tower::ServiceExt;

fn state() -> (tempfile::TempDir, AppState) {
    let root = tempfile::tempdir().unwrap();
    let library = Library::open(root.path().join("lib")).unwrap();
    let state = AppState {
        library: Arc::new(library),
        converters: Arc::new(Converters::new(vec![Box::new(
            nalanda_convert::NativeConverter,
        )])),
    };
    (root, state)
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, body, headers)
}

fn upload(name: &str, data: &[u8]) -> Request<Body> {
    let boundary = "XBOUNDARYX";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::post("/api/books")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn upload_convert_download_update_delete() {
    let (_root, state) = state();
    let app = router(state);

    let (status, body, _) = call(
        &app,
        upload("Ada Writer - Night Notes.txt", b"Chapter 1\n\nDark."),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let book: Book = serde_json::from_slice(&body).unwrap();
    assert_eq!(book.metadata.title, "Night Notes");
    assert_eq!(book.metadata.authors, ["Ada Writer"]);
    let id = book.id;

    let convert = Request::post(format!("/api/books/{id}/convert"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"to":"epub"}"#))
        .unwrap();
    let (status, body, _) = call(&app, convert).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let book: Book = serde_json::from_slice(&body).unwrap();
    assert!(book.has_format(BookFormat::Epub) && book.has_format(BookFormat::Txt));

    let (status, body, headers) = call(
        &app,
        Request::get(format!("/api/books/{id}/formats/epub"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/epub+zip");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("Night Notes - Ada Writer.epub")
    );
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("dl.epub");
    std::fs::write(&epub, body).unwrap();
    let doc = nalanda_formats::read_document(&epub).unwrap();
    assert_eq!(doc.chapters[0].blocks[1].text(), "Dark.");

    // Unsupported conversion and unknown ids map to client errors.
    let pdf = Request::post(format!("/api/books/{id}/convert"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"to":"pdf"}"#))
        .unwrap();
    assert_eq!(call(&app, pdf).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        call(
            &app,
            Request::get("/api/books/999").body(Body::empty()).unwrap()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            Request::get(format!("/api/books/{id}/cover"))
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let mut metadata = book.metadata.clone();
    metadata.tags = vec!["Noir".into()];
    let patch = Request::patch(format!("/api/books/{id}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&metadata).unwrap()))
        .unwrap();
    assert_eq!(call(&app, patch).await.0, StatusCode::OK);
    let (_, body, _) = call(
        &app,
        Request::get("/api/books?q=noir")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let found: Vec<Book> = serde_json::from_slice(&body).unwrap();
    assert_eq!(found.iter().map(|b| b.id).collect::<Vec<_>>(), [id]);

    let (status, body, headers) =
        call(&app, Request::get("/opds").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/atom+xml")
    );
    let feed = String::from_utf8(body).unwrap();
    assert!(feed.contains("<title>Night Notes</title>"));
    assert!(feed.contains(&format!(
        "href=\"/api/books/{id}/formats/epub\" type=\"application/epub+zip\""
    )));

    assert_eq!(
        call(
            &app,
            Request::delete(format!("/api/books/{id}"))
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, body, _) = call(
        &app,
        Request::get("/api/books").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(serde_json::from_slice::<Vec<Book>>(&body).unwrap(), []);
}

#[tokio::test]
async fn upload_rejects_non_ebook_names() {
    let (_root, state) = state();
    let app = router(state);
    assert_eq!(
        call(&app, upload("../../evil.sh", b"x")).await.0,
        StatusCode::BAD_REQUEST
    );
}

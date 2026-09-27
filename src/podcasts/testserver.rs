//! Fake podcast servers for the tests, all on 127.0.0.1.
//!
//! | Path              | Reply                                                   |
//! |-------------------|---------------------------------------------------------|
//! | `/feed.xml`       | the body set with [`FakeServer::set_feed`], counts hits |
//! | `/moved/{*path}`  | 302 to `/{path}`                                        |
//! | `/files/...`      | `ServeDir` over the folder given to [`FakeServer::start`], answers `Range` |
//! | `/plain/{name}`   | the same file, always 200 with the whole body           |
//! | `/gzip/{name}`    | the same file as is, with `Content-Encoding: gzip`      |
//! | `/status/{code}`  | that status, empty body                                 |

use std::fmt::Write as _;
use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, LOCATION};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use image::{ImageFormat, Rgb, RgbImage};
use tower_http::services::ServeDir;

pub struct FakeServer {
    pub base: String,
    shared: Shared,
}

#[derive(Clone)]
struct Shared {
    feed: Arc<Mutex<String>>,
    hits: Arc<AtomicUsize>,
    files: PathBuf,
}

impl FakeServer {
    pub fn start(files: &Path) -> FakeServer {
        let shared = Shared {
            feed: Arc::default(),
            hits: Arc::default(),
            files: files.to_path_buf(),
        };
        let app = Router::new()
            .route("/feed.xml", get(feed))
            .route("/moved/{*path}", get(moved))
            .route("/plain/{name}", get(plain))
            .route("/gzip/{name}", get(gzip))
            .route("/status/{code}", get(status))
            .nest_service("/files", ServeDir::new(files))
            .with_state(shared.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        FakeServer { base, shared }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn set_feed(&self, body: &str) {
        *self.shared.feed.lock().unwrap() = body.to_string();
    }

    pub fn hits(&self) -> usize {
        self.shared.hits.load(Ordering::SeqCst)
    }
}

async fn feed(State(shared): State<Shared>) -> Response {
    shared.hits.fetch_add(1, Ordering::SeqCst);
    let body = shared.feed.lock().unwrap().clone();
    ([(CONTENT_TYPE, "application/rss+xml")], body).into_response()
}

async fn plain(State(shared): State<Shared>, UrlPath(name): UrlPath<String>) -> Response {
    match std::fs::read(shared.files.join(name)) {
        Ok(body) => body.into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn gzip(State(shared): State<Shared>, UrlPath(name): UrlPath<String>) -> Response {
    match std::fs::read(shared.files.join(name)) {
        Ok(body) => ([(CONTENT_ENCODING, "gzip")], body).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn moved(UrlPath(path): UrlPath<String>) -> Response {
    (StatusCode::FOUND, [(LOCATION, format!("/{path}"))]).into_response()
}

async fn status(UrlPath(code): UrlPath<u16>) -> StatusCode {
    StatusCode::from_u16(code).unwrap()
}

/// A URL on a port nothing listens on.
pub fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

/// Every reply promises `claimed` bytes, sends `body` and hangs up.
pub fn truncated(claimed: usize, body: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/episode.mp3", listener.local_addr().unwrap());
    thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                request.push(byte[0]);
            }
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {claimed}\r\n\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    url
}

pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut png = Vec::new();
    RgbImage::from_pixel(width, height, Rgb([200, 40, 40]))
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    png
}

pub struct Item<'a> {
    pub guid: &'a str,
    pub title: &'a str,
    /// RFC 2822.
    pub date: &'a str,
    pub url: &'a str,
    pub picture: Option<&'a str>,
}

pub fn rss(title: &str, picture: Option<&str>, items: &[Item]) -> String {
    let picture = picture
        .map(|url| format!("<itunes:image href=\"{url}\"/>"))
        .unwrap_or_default();
    let items = items.iter().fold(String::new(), |mut out, item| {
        let picture = item
            .picture
            .map(|url| format!("<itunes:image href=\"{url}\"/>"))
            .unwrap_or_default();
        let _ = write!(
            out,
            "<item><title>{}</title><guid>{}</guid><pubDate>{}</pubDate>\
             <enclosure url=\"{}\" type=\"audio/mpeg\" length=\"0\"/>{picture}</item>",
            item.title, item.guid, item.date, item.url
        );
        out
    });
    format!(
        "<?xml version=\"1.0\"?>\
         <rss version=\"2.0\" xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\">\
         <channel><title>{title}</title>{picture}{items}</channel></rss>"
    )
}

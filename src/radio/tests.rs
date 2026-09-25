use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use super::*;

/// Answers `GET <path>` with the `(status, content type, body)` of the
/// first route whose path matches; `{base}` in a body is the server's URL.
fn serve(routes: &'static [(&'static str, u16, &'static str, &'static str)]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let url = base.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = String::new();
            let _ = BufReader::new(&stream).read_line(&mut request);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let path = path.split('?').next().unwrap_or(path);
            let (status, content_type, body) = routes
                .iter()
                .find(|(p, ..)| *p == path)
                .map_or((404, "", String::new()), |(_, s, t, b)| {
                    (*s, *t, b.replace("{base}", &url))
                });
            let mut reply = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
            if !content_type.is_empty() {
                let _ = write!(reply, "Content-Type: {content_type}\r\n");
            }
            let _ = write!(reply, "Content-Length: {}\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    base
}

const ROUTES: &[(&str, u16, &str, &str)] = &[
    ("/live.mp3", 200, "audio/mpeg; charset=x", "ID3 audio bytes"),
    (
        "/list.pls",
        200,
        "audio/x-scpls",
        "[playlist]\nFile2={base}/two.mp3\nFile1={base}/live.mp3\nNumberOfEntries=2\n",
    ),
    (
        "/list.m3u",
        200,
        "audio/x-mpegurl",
        "#EXTM3U\n#EXTINF:-1,Kinder\n\n{base}/live.mp3\n",
    ),
    (
        "/plain.pls",
        200,
        "text/plain",
        "[playlist]\nfile1={base}/live.mp3\n",
    ),
    ("/nested.m3u", 200, "", "{base}/list.pls\n"),
    (
        "/hls.m3u8",
        200,
        "application/vnd.apple.mpegurl",
        "#EXTM3U\n#EXT-X-VERSION:3\nchunk.m3u8\n",
    ),
    (
        "/empty.pls",
        200,
        "audio/x-scpls",
        "[playlist]\nNumberOfEntries=0\n",
    ),
    ("/loop.m3u", 200, "audio/x-mpegurl", "{base}/loop.m3u\n"),
    ("/gone.mp3", 404, "", ""),
];

fn resolve_at(path: &str) -> Result<Stream> {
    let base = serve(ROUTES);
    resolve(
        &crate::net::stream_agent(),
        &format!("{base}{path}?token=secret"),
    )
    .map(|stream| Stream {
        url: stream.url.replace(&base, ""),
        ..stream
    })
}

fn stream(url: &str, content_type: &str) -> Stream {
    Stream {
        url: url.into(),
        content_type: Some(content_type.into()),
    }
}

#[test]
fn a_stream_is_played_as_it_is() {
    assert_eq!(
        resolve_at("/live.mp3").unwrap(),
        stream("/live.mp3?token=secret", "audio/mpeg")
    );
}

#[test]
fn playlists_lead_to_their_first_stream() {
    for path in ["/list.pls", "/list.m3u", "/plain.pls", "/nested.m3u"] {
        assert_eq!(
            resolve_at(path).unwrap(),
            stream("/live.mp3", "audio/mpeg"),
            "{path}"
        );
    }
}

#[test]
fn an_hls_playlist_is_the_stream() {
    let hls = resolve_at("/hls.m3u8").unwrap();
    assert_eq!(hls, stream("/hls.m3u8?token=secret", HLS));
    assert!(hls.is_hls());
}

#[test]
fn broken_stations_are_errors_without_the_query() {
    for (path, needle) in [
        ("/empty.pls", "names no http(s) stream"),
        ("/loop.m3u", "more than"),
        ("/gone.mp3", "404"),
    ] {
        let err = format!("{:#}", resolve_at(path).unwrap_err());
        assert!(err.contains(needle), "{path}: {err}");
        assert!(!err.contains("secret"), "{path}: {err}");
    }
}

#[test]
fn a_content_type_that_says_nothing_falls_back_to_the_extension() {
    assert_eq!(playlist_kind("http://a/x.PLS?y", None), Some(List::Pls));
    assert_eq!(
        playlist_kind("http://a/x.m3u8", Some("text/plain")),
        Some(List::M3u)
    );
    assert_eq!(playlist_kind("http://a/x.pls", Some("audio/mpeg")), None);
    assert_eq!(playlist_kind("http://a/x.mp3", None), None);
}

#[test]
#[ignore = "needs the internet: cargo test -- --ignored"]
fn resolves_real_stations() {
    crate::net::install_crypto();
    let agent = crate::net::stream_agent();
    for url in [
        "http://streamtdy.ir-media-tec.com/kinderlieder/mp3-128/web/play.mp3",
        "https://radio-hls.mdr.de/hls/live/2112913/mdr-991101-0-hls/index.m3u8",
        "https://stream.rpr1.de/kidshits/mp3-128/radiobrowser",
    ] {
        let stream = resolve(&agent, url).unwrap();
        println!("{url}\n  -> {stream:?}");
        assert!(stream.content_type.is_some(), "{url}");
    }
}

//! HTTP for podcasts: feeds, episode files and pictures. Only http(s) URLs,
//! and every body has a size limit.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use image::codecs::jpeg::JpegEncoder;
use image::{ImageReader, Limits};
use ureq::http::Response;
use ureq::{Agent, Body};

use super::cache;
use super::feed::http_url;

const MB: u64 = 1024 * 1024;
const FEED_LIMIT: u64 = 20 * MB;
const PICTURE_LIMIT: u64 = 10 * MB;
pub const FEED_TIMEOUT: Duration = Duration::from_secs(30);
/// The stream agent has no deadline for the body, and a stalled download must
/// not block the podcasts thread for good; its `.part` resumes next time.
const EPISODE_DEADLINE: Duration = Duration::from_hours(1);
/// Pictures are stored at most this big; deck keys are smaller.
pub const PICTURE_SIDE: u32 = 512;
/// Larger pictures are refused before they are decoded: a Pi 3 has 1 GB.
const DECODE_MAX_SIDE: u32 = 6000;
const DECODE_MAX_ALLOC: u64 = 256 * MB;
const JPEG_QUALITY: u8 = 85;

pub fn fetch_feed(agent: &Agent, url: &str) -> Result<Vec<u8>> {
    let mut reply = get(agent, url)?;
    read_limited(reply.body_mut(), FEED_LIMIT, "feed")
}

fn get(agent: &Agent, url: &str) -> Result<Response<Body>> {
    let url = http_url(url).context("not an http(s) URL")?;
    let reply = agent.get(&url).call()?;
    let status = reply.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    Ok(reply)
}

fn read_limited(body: &mut Body, limit: u64, what: &str) -> Result<Vec<u8>> {
    body.with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|err| match err {
            ureq::Error::BodyExceedsLimit(_) => {
                anyhow!("the {what} is larger than {} MB", limit / MB)
            }
            err => anyhow::Error::new(err).context(format!("cannot read the {what}")),
        })
}

/// Why an episode download failed, which also says what happened to its
/// `.part` file.
#[derive(Debug)]
pub enum Failure {
    /// The connection broke or the server had a passing problem: the `.part`
    /// stays, and the next refresh resumes it.
    Network(anyhow::Error),
    /// The server refused the file or it is too big: the `.part` is deleted.
    Rejected(anyhow::Error),
    /// Writing failed, usually because the disk is full: the `.part` is
    /// deleted, and this refresh downloads nothing more.
    Disk(anyhow::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Failure::Network(err) | Failure::Rejected(err) | Failure::Disk(err)) = self;
        write!(f, "{err:#}")
    }
}

enum Attempt {
    Done(u64),
    StartOver,
}

/// Downloads `url` to `dir/name` through `.name.part`, resuming a `.part`
/// left by an earlier try. Returns the file size.
pub fn episode(
    agent: &Agent,
    url: &str,
    dir: &Path,
    name: &str,
    max_bytes: u64,
) -> Result<u64, Failure> {
    let url = http_url(url).ok_or_else(|| Failure::Rejected(anyhow!("not an http(s) URL")))?;
    fs::create_dir_all(dir).map_err(|err| disk(err, dir))?;
    let part = dir.join(cache::part_name(name));
    let mut started_over = false;
    loop {
        match attempt(agent, &url, &part, max_bytes)? {
            Attempt::Done(bytes) => {
                let dest = dir.join(name);
                return match fs::rename(&part, &dest) {
                    Ok(()) => Ok(bytes),
                    Err(err) => Err(discard(&part, disk(err, &dest))),
                };
            }
            Attempt::StartOver if !started_over => {
                started_over = true;
                cache::delete(&part).map_err(|err| disk(err, &part))?;
            }
            Attempt::StartOver => {
                let refused = anyhow!("the server refuses to resume the download");
                return Err(discard(&part, Failure::Rejected(refused)));
            }
        }
    }
}

fn attempt(agent: &Agent, url: &str, part: &Path, max_bytes: u64) -> Result<Attempt, Failure> {
    let have = fs::metadata(part).map_or(0, |meta| meta.len());
    // Ranges count stored bytes, so the body must not be compressed.
    let mut request = agent.get(url).header("accept-encoding", "identity");
    if have > 0 {
        request = request.header("range", format!("bytes={have}-"));
    }
    let mut reply = request
        .config()
        .timeout_recv_body(Some(EPISODE_DEADLINE))
        .build()
        .call()
        .map_err(|err| Failure::Network(err.into()))?;

    let status = reply.status().as_u16();
    let offset = match status {
        206 if have > 0 && range_start(&reply) == Some(have) => have,
        206 | 416 if have > 0 => return Ok(Attempt::StartOver),
        200..=299 if status != 206 => 0,
        // Timeouts and rate limits pass; the partial file is still good.
        408 | 429 | 500..=599 => return Err(Failure::Network(anyhow!("HTTP {status}"))),
        _ => return Err(discard(part, Failure::Rejected(anyhow!("HTTP {status}")))),
    };
    let expected = reply
        .body()
        .content_length()
        .map(|len| len.saturating_add(offset));
    if expected.is_some_and(|len| len > max_bytes) {
        return Err(discard(part, too_big(max_bytes)));
    }

    let file = if offset > 0 {
        OpenOptions::new().append(true).open(part)
    } else {
        File::create(part)
    };
    let mut file = file.map_err(|err| discard(part, disk(err, part)))?;
    let body = reply.body_mut().as_reader();
    let written = copy(body, &mut file, offset, max_bytes).map_err(|err| match err {
        Failure::Network(_) => err,
        err => discard(part, err),
    })?;
    file.sync_all()
        .map_err(|err| discard(part, disk(err, part)))?;
    Ok(Attempt::Done(written))
}

/// Like `io::copy`, but tells read errors (network) from write errors (disk)
/// and stops once the file would pass `max_bytes`.
fn copy(mut body: impl Read, file: &mut File, offset: u64, max_bytes: u64) -> Result<u64, Failure> {
    let mut buf = vec![0; 64 * 1024];
    let mut written = offset;
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => return Ok(written),
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                let err = anyhow::Error::new(err).context("the download broke off");
                return Err(Failure::Network(err));
            }
        };
        let total = written + n as u64;
        if total > max_bytes {
            return Err(too_big(max_bytes));
        }
        file.write_all(&buf[..n])
            .map_err(|err| Failure::Disk(anyhow::Error::new(err).context("cannot write")))?;
        written = total;
    }
}

/// The start of `Content-Range: bytes <start>-<end>/<total>`.
fn range_start(reply: &Response<Body>) -> Option<u64> {
    let value = reply.headers().get("content-range")?.to_str().ok()?;
    let range = value.trim().strip_prefix("bytes ")?;
    range.split('-').next()?.trim().parse().ok()
}

fn too_big(max_bytes: u64) -> Failure {
    Failure::Rejected(anyhow!("the episode is larger than {} MB", max_bytes / MB))
}

fn disk(err: io::Error, path: &Path) -> Failure {
    Failure::Disk(anyhow::Error::new(err).context(format!("cannot write {}", path.display())))
}

/// Deletes the `.part`; a failure to do so is worth a log line, not more.
fn discard(part: &Path, failure: Failure) -> Failure {
    if let Err(err) = cache::delete(part) {
        tracing::warn!("cannot delete {}: {err}", part.display());
    }
    failure
}

/// Downloads a picture and stores it as a JPEG of at most 512 px per side.
pub fn picture(agent: &Agent, url: &str, dir: &Path, name: &str) -> Result<()> {
    let mut reply = get(agent, url)?;
    let bytes = read_limited(reply.body_mut(), PICTURE_LIMIT, "picture")?;
    let jpeg = shrink(&bytes)?;
    fs::create_dir_all(dir)?;
    let part = dir.join(cache::part_name(name));
    let dest = dir.join(name);
    let write = || -> io::Result<()> {
        let mut file = File::create(&part)?;
        file.write_all(&jpeg)?;
        file.sync_all()?;
        fs::rename(&part, &dest)
    };
    write().map_err(|err| {
        let _ = cache::delete(&part);
        anyhow::Error::new(err).context(format!("cannot write {}", dest.display()))
    })
}

fn shrink(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODE_MAX_SIDE);
    limits.max_image_height = Some(DECODE_MAX_SIDE);
    limits.max_alloc = Some(DECODE_MAX_ALLOC);
    reader.limits(limits);
    let image = reader.decode().context("cannot decode the picture")?;
    let image = if image.width() > PICTURE_SIDE || image.height() > PICTURE_SIDE {
        image.thumbnail(PICTURE_SIDE, PICTURE_SIDE)
    } else {
        image
    };
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).encode_image(&image.to_rgb8())?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net;
    use crate::podcasts::testserver::{FakeServer, closed_port, png, truncated};

    const NAME: &str = "20250107-episode-1a2b3c4d.mp3";

    struct Setup {
        server: FakeServer,
        cache: tempfile::TempDir,
        files: tempfile::TempDir,
        audio: Vec<u8>,
    }

    fn setup() -> Setup {
        let files = tempfile::tempdir().unwrap();
        let audio: Vec<u8> = (0..1000_u32).map(|i| (i % 251) as u8).collect();
        fs::write(files.path().join("ep.mp3"), &audio).unwrap();
        Setup {
            server: FakeServer::start(files.path()),
            cache: tempfile::tempdir().unwrap(),
            files,
            audio,
        }
    }

    impl Setup {
        fn part(&self) -> std::path::PathBuf {
            self.cache.path().join(cache::part_name(NAME))
        }

        fn dest(&self) -> std::path::PathBuf {
            self.cache.path().join(NAME)
        }

        fn download(&self, path: &str, max_bytes: u64) -> Result<u64, Failure> {
            let url = self.server.url(path);
            episode(
                &net::stream_agent(),
                &url,
                self.cache.path(),
                NAME,
                max_bytes,
            )
        }
    }

    fn api() -> Agent {
        net::api_agent(Duration::from_secs(5))
    }

    #[test]
    fn fetches_a_feed_through_a_redirect() {
        let s = setup();
        s.server.set_feed("<rss/>");

        let body = fetch_feed(&api(), &s.server.url("/moved/feed.xml")).unwrap();

        assert_eq!(body, b"<rss/>");
        assert_eq!(s.server.hits(), 1);
    }

    #[test]
    fn feed_errors_name_the_problem() {
        let s = setup();
        let err = fetch_feed(&api(), &s.server.url("/status/404")).unwrap_err();
        assert!(err.to_string().contains("404"), "{err:#}");
        assert!(fetch_feed(&api(), &closed_port()).is_err());
        let err = fetch_feed(&api(), "file:///etc/passwd").unwrap_err();
        assert!(err.to_string().contains("http"), "{err:#}");
    }

    #[test]
    fn downloads_through_a_part_file() {
        let s = setup();

        assert_eq!(s.download("/files/ep.mp3", 10_000).unwrap(), 1000);

        assert_eq!(fs::read(s.dest()).unwrap(), s.audio);
        assert!(!s.part().exists());
    }

    #[test]
    fn a_cut_off_body_leaves_no_final_file() {
        let s = setup();
        let url = truncated(1000, b"0123456789");

        let result = episode(&net::stream_agent(), &url, s.cache.path(), NAME, 10_000);

        assert!(matches!(result, Err(Failure::Network(_))), "{result:?}");
        assert!(!s.dest().exists());
        assert_eq!(fs::read(s.part()).unwrap(), b"0123456789");
    }

    #[test]
    fn resumes_a_part_file_with_a_range() {
        let s = setup();
        fs::write(s.part(), [b'X'; 400]).unwrap();

        assert_eq!(s.download("/files/ep.mp3", 10_000).unwrap(), 1000);

        let file = fs::read(s.dest()).unwrap();
        assert_eq!(&file[..400], &[b'X'; 400]);
        assert_eq!(&file[400..], &s.audio[400..]);
    }

    #[test]
    fn a_whole_body_replaces_the_part_file() {
        let s = setup();
        fs::write(s.part(), b"XXXX").unwrap();

        assert_eq!(s.download("/plain/ep.mp3", 10_000).unwrap(), 1000);

        assert_eq!(fs::read(s.dest()).unwrap(), s.audio);
    }

    #[test]
    fn an_unsatisfiable_range_starts_over() {
        let s = setup();
        fs::write(s.part(), [b'X'; 2000]).unwrap();

        assert_eq!(s.download("/files/ep.mp3", 10_000).unwrap(), 1000);

        assert_eq!(fs::read(s.dest()).unwrap(), s.audio);
    }

    #[test]
    fn too_big_episodes_are_refused() {
        let s = setup();
        let result = s.download("/files/ep.mp3", 999);
        assert!(matches!(result, Err(Failure::Rejected(_))), "{result:?}");
        assert!(!s.part().exists() && !s.dest().exists());

        fs::write(s.part(), [b'X'; 400]).unwrap();
        let result = s.download("/files/ep.mp3", 700);
        assert!(matches!(result, Err(Failure::Rejected(_))), "{result:?}");
        assert!(!s.part().exists() && !s.dest().exists());

        assert_eq!(s.download("/files/ep.mp3", 1000).unwrap(), 1000);
    }

    #[test]
    fn copy_stops_at_the_size_limit_even_without_a_length() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = File::create(dir.path().join("f")).unwrap();
        let result = copy(Cursor::new(vec![0; 1000]), &mut file, 0, 500);
        assert!(matches!(result, Err(Failure::Rejected(_))), "{result:?}");
        let result = copy(Cursor::new(vec![0; 100]), &mut file, 400, 500);
        assert_eq!(result.unwrap(), 500);
    }

    #[test]
    fn copy_reports_read_errors_as_network_failures() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut file = File::create(dir.path().join("f")).unwrap();
        let result = copy(Broken, &mut file, 0, 500);
        assert!(matches!(result, Err(Failure::Network(_))), "{result:?}");
    }

    #[test]
    fn client_errors_delete_the_part_and_server_errors_keep_it() {
        let s = setup();
        fs::write(s.part(), b"XXXX").unwrap();
        for passing in ["/status/503", "/status/429"] {
            let result = s.download(passing, 10_000);
            assert!(matches!(result, Err(Failure::Network(_))), "{result:?}");
            assert!(s.part().exists());
        }

        let result = s.download("/status/404", 10_000);
        assert!(matches!(result, Err(Failure::Rejected(_))), "{result:?}");
        assert!(!s.part().exists());
    }

    #[test]
    fn only_http_urls_are_downloaded() {
        let s = setup();
        let result = episode(
            &net::stream_agent(),
            "ftp://x/a.mp3",
            s.cache.path(),
            NAME,
            10,
        );
        assert!(matches!(result, Err(Failure::Rejected(_))), "{result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_folder_is_a_disk_failure() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        let locked = s.cache.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        let url = s.server.url("/files/ep.mp3");
        let result = episode(&net::stream_agent(), &url, &locked, NAME, 10_000);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(Failure::Disk(_))), "{result:?}");
    }

    fn picture_size(s: &Setup, width: u32, height: u32) -> Result<(u32, u32)> {
        fs::write(s.files.path().join("pic.png"), png(width, height)).unwrap();
        let name = cache::picture_name("x");
        picture(
            &api(),
            &s.server.url("/files/pic.png"),
            s.cache.path(),
            &name,
        )?;
        let stored = image::open(s.cache.path().join(&name)).unwrap();
        assert!(!s.cache.path().join(cache::part_name(&name)).exists());
        Ok((stored.width(), stored.height()))
    }

    #[test]
    fn pictures_shrink_to_512_px() {
        let s = setup();
        assert_eq!(picture_size(&s, 1600, 1200).unwrap(), (512, 384));
        assert_eq!(picture_size(&s, 100, 80).unwrap(), (100, 80));
    }

    #[test]
    fn huge_or_broken_pictures_are_refused() {
        let s = setup();
        assert!(picture_size(&s, 7000, 4).is_err());
        fs::write(s.files.path().join("pic.png"), b"not a picture").unwrap();
        let name = cache::picture_name("y");
        assert!(
            picture(
                &api(),
                &s.server.url("/files/pic.png"),
                s.cache.path(),
                &name
            )
            .is_err()
        );
        assert!(!s.cache.path().join(&name).exists());
    }
}

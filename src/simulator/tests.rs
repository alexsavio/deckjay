//! The simulator API over real HTTP: `serve` on a free port, ureq as client.

use std::io::Cursor;
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use image::{ImageFormat, Rgb, RgbImage};
use serde::de::DeserializeOwned;

use super::api::MAX_QUEUED;
use super::{Brightness, Info, Model, State, serve};

fn start(info: Info) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || serve(listener, info));
    url
}

fn png(shade: u8) -> Vec<u8> {
    let mut png = Vec::new();
    RgbImage::from_pixel(4, 4, Rgb([shade, 0, 0]))
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    png
}

fn get_json<T: DeserializeOwned>(url: &str) -> T {
    let body = ureq::get(url)
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    serde_json::from_str(&body).unwrap()
}

fn state(url: &str) -> State {
    get_json(&format!("{url}/api/state"))
}

fn presses(url: &str, wait_ms: u64) -> Vec<usize> {
    get_json(&format!("{url}/api/presses?wait_ms={wait_ms}"))
}

fn put_key(url: &str, key: usize, png: &[u8]) {
    ureq::put(format!("{url}/api/keys/{key}"))
        .header("content-type", "image/png")
        .send(png)
        .unwrap();
}

fn press(url: &str, key: usize) {
    ureq::post(format!("{url}/api/press/{key}"))
        .send_empty()
        .unwrap();
}

fn set_brightness(url: &str, percent: u8) {
    ureq::put(format!("{url}/api/brightness"))
        .header("content-type", "application/json")
        .send(serde_json::to_string(&Brightness { percent }).unwrap())
        .unwrap();
}

fn status<T>(reply: Result<T, ureq::Error>) -> u16 {
    match reply {
        Ok(_) => 200,
        Err(ureq::Error::StatusCode(code)) => code,
        Err(err) => panic!("request failed: {err}"),
    }
}

#[test]
fn info_is_the_simulated_grid() {
    let url = start(Model::Xl.info());
    let info: Info = get_json(&format!("{url}/api/info"));
    assert_eq!(info, Model::Xl.info());
}

#[test]
fn a_key_image_comes_back_unchanged_with_a_new_version() {
    let url = start(Model::Mk2.info());
    assert_eq!(state(&url).versions, vec![0; 15]);

    put_key(&url, 3, &png(10));
    let mut reply = ureq::get(format!("{url}/api/keys/3")).call().unwrap();
    assert_eq!(reply.headers()["content-type"], "image/png");
    assert_eq!(reply.body_mut().read_to_vec().unwrap(), png(10));
    let first = state(&url).versions;
    assert!(first[3] > 0);
    assert!(first.iter().enumerate().all(|(n, &v)| n == 3 || v == 0));

    put_key(&url, 3, &png(20));
    assert!(state(&url).versions[3] > first[3]);
}

#[test]
fn keys_outside_the_grid_are_not_found() {
    let url = start(Model::Mk2.info());
    for key in ["15", "99", "-1", "x"] {
        let key_url = format!("{url}/api/keys/{key}");
        assert_eq!(status(ureq::get(&key_url).call()), 404, "GET {key}");
        let put = ureq::put(&key_url).send(&png(1)[..]);
        assert_eq!(status(put), 404, "PUT {key}");
        let press = ureq::post(format!("{url}/api/press/{key}")).send_empty();
        assert_eq!(status(press), 404, "press {key}");
    }
}

#[test]
fn a_key_without_an_image_is_not_found() {
    let url = start(Model::Mk2.info());
    assert_eq!(status(ureq::get(format!("{url}/api/keys/0")).call()), 404);
}

#[test]
fn a_key_image_must_be_a_png() {
    let url = start(Model::Mk2.info());
    let reply = ureq::put(format!("{url}/api/keys/0")).send("not a picture");
    assert_eq!(status(reply), 415);
    assert_eq!(state(&url).versions[0], 0);
}

#[test]
fn presses_are_returned_once_oldest_first() {
    let url = start(Model::Mk2.info());
    press(&url, 7);
    press(&url, 2);
    assert_eq!(presses(&url, 0), [7, 2]);
    assert!(presses(&url, 0).is_empty());
}

#[test]
fn a_full_queue_drops_the_oldest_press() {
    let url = start(Model::Mk2.info());
    press(&url, 0);
    for _ in 0..MAX_QUEUED {
        press(&url, 1);
    }
    assert_eq!(presses(&url, 0), vec![1; MAX_QUEUED]);
}

#[test]
fn a_long_poll_ends_soon_after_a_press() {
    let url = start(Model::Mk2.info());
    let presser = {
        let url = url.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            press(&url, 5);
        })
    };

    let started = Instant::now();
    assert_eq!(presses(&url, 5000), [5]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    presser.join().unwrap();
}

#[test]
fn a_long_poll_without_presses_waits_then_returns_nothing() {
    let url = start(Model::Mk2.info());
    let started = Instant::now();
    assert!(presses(&url, 300).is_empty());
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[test]
fn brightness_is_kept_and_capped_at_100() {
    let url = start(Model::Mk2.info());
    assert_eq!(state(&url).brightness, 100);
    set_brightness(&url, 40);
    assert_eq!(state(&url).brightness, 40);
    set_brightness(&url, 250);
    assert_eq!(state(&url).brightness, 100);
}

#[test]
fn reset_clears_the_deck_and_versions_keep_growing() {
    let url = start(Model::Mk2.info());
    put_key(&url, 0, &png(1));
    put_key(&url, 1, &png(2));
    press(&url, 4);
    set_brightness(&url, 30);
    let before = state(&url).versions.into_iter().max().unwrap();

    ureq::post(format!("{url}/api/reset")).send_empty().unwrap();

    let after = state(&url);
    assert_eq!(after.versions, vec![0; 15]);
    assert_eq!(after.brightness, 100);
    assert_eq!(status(ureq::get(format!("{url}/api/keys/0")).call()), 404);
    assert!(presses(&url, 0).is_empty());

    put_key(&url, 0, &png(3));
    assert!(state(&url).versions[0] > before);
}

#[test]
fn the_player_is_connected_after_asking_for_presses() {
    let url = start(Model::Mk2.info());
    assert!(!state(&url).connected);
    presses(&url, 0);
    assert!(state(&url).connected);
}

#[test]
fn the_page_is_served_at_the_root() {
    let url = start(Model::Mk2.info());
    let mut reply = ureq::get(format!("{url}/")).call().unwrap();
    let content_type = reply.headers()["content-type"].to_str().unwrap();
    assert!(content_type.starts_with("text/html"), "{content_type}");
    let html = reply.body_mut().read_to_string().unwrap();
    assert!(html.contains("/api/state"));
}

//! Spotify Connect: sign-in, tokens and the Web API.
//!
//! kids-deck does not play Spotify audio itself: it remote-controls a Connect
//! device (a speaker, receiver or Chromecast) through the Web API, which needs
//! Premium. `login` signs in once and saves the refresh token (`token`);
//! `api::Client` then keeps an access token fresh.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired in when Spotify playback lands")
)]

pub mod api;
pub mod auth;
#[cfg(test)]
mod fake;
pub mod login;
pub mod token;

use anyhow::{Result, bail};

use api::Device;

/// `spotify:playlist:<id>` from that URI itself or from a share link such as
/// `https://open.spotify.com/intl-de/playlist/<id>?si=…`.
pub fn normalize_playlist(uri_or_url: &str) -> Result<String> {
    let text = uri_or_url.trim();
    let id = if let Some(id) = text.strip_prefix("spotify:playlist:") {
        Some(id)
    } else {
        playlist_id_in_link(text)
    };
    match id {
        Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Ok(format!("spotify:playlist:{id}"))
        }
        _ => bail!(
            "{uri_or_url:?} is not a Spotify playlist: use spotify:playlist:… \
             or https://open.spotify.com/playlist/…"
        ),
    }
}

fn playlist_id_in_link(text: &str) -> Option<&str> {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text)
        .strip_prefix("open.spotify.com/")?;
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let mut kind = segments.next()?;
    if kind.starts_with("intl-") {
        kind = segments.next()?;
    }
    let id = segments.next()?;
    (kind == "playlist" && segments.next().is_none()).then_some(id)
}

/// The first controllable device whose name contains `name_part`, ignoring
/// case; a device with exactly that name wins over the others.
pub fn pick_device<'a>(devices: &'a [Device], name_part: &str) -> Result<&'a Device> {
    let wanted = name_part.trim().to_lowercase();
    let controllable = || devices.iter().filter(|device| device.id.is_some());
    let found = controllable()
        .find(|device| device.name.to_lowercase() == wanted)
        .or_else(|| controllable().find(|device| device.name.to_lowercase().contains(&wanted)));
    if let Some(device) = found {
        return Ok(device);
    }
    let seen = if devices.is_empty() {
        "none right now".to_string()
    } else {
        let names: Vec<String> = devices
            .iter()
            .map(|device| match device.id {
                Some(_) => device.name.clone(),
                None => format!("{} (cannot be controlled)", device.name),
            })
            .collect();
        names.join(", ")
    };
    bail!(
        "no Spotify Connect device matches {name_part:?}; Spotify sees: {seen} \
         (asleep? open the Spotify app once and play something on it)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "37i9dQZF1DX8Uebhn9wzrS";

    #[test]
    fn a_playlist_uri_stays_as_it_is() {
        let uri = format!("spotify:playlist:{ID}");
        assert_eq!(normalize_playlist(&uri).unwrap(), uri);
        assert_eq!(normalize_playlist(&format!("  {uri}\n")).unwrap(), uri);
    }

    #[test]
    fn a_share_link_becomes_a_uri() {
        for link in [
            format!("https://open.spotify.com/playlist/{ID}"),
            format!("https://open.spotify.com/playlist/{ID}?si=a1b2c3d4e5f60718"),
            format!("https://open.spotify.com/intl-de/playlist/{ID}?si=x&pt=y"),
            format!("http://open.spotify.com/playlist/{ID}/"),
            format!("open.spotify.com/playlist/{ID}#top"),
        ] {
            assert_eq!(
                normalize_playlist(&link).unwrap(),
                format!("spotify:playlist:{ID}"),
                "{link}"
            );
        }
    }

    #[test]
    fn anything_else_is_refused() {
        for text in [
            String::new(),
            "spotify:playlist:".into(),
            format!("spotify:album:{ID}"),
            format!("https://open.spotify.com/album/{ID}"),
            format!("https://open.spotify.com/playlist/{ID}/tracks"),
            format!("https://example.com/playlist/{ID}"),
            "spotify:playlist:abc/../def".into(),
            "Bedtime songs".into(),
        ] {
            let err = normalize_playlist(&text).unwrap_err().to_string();
            assert!(err.contains("is not a Spotify playlist"), "{text}: {err}");
        }
    }

    fn device(id: Option<&str>, name: &str) -> Device {
        Device {
            id: id.map(Into::into),
            name: name.into(),
            kind: "Speaker".into(),
            is_active: false,
            is_restricted: false,
            supports_volume: true,
            volume_percent: None,
        }
    }

    fn devices() -> Vec<Device> {
        vec![
            device(Some("garden"), "Garden Speaker"),
            device(None, "Den TV"),
            device(Some("den"), "Den"),
            device(Some("kitchen"), "Kitchen"),
        ]
    }

    #[test]
    fn a_device_is_found_by_part_of_its_name_in_any_case() {
        let devices = devices();
        let picked = pick_device(&devices, "kITch").unwrap();
        assert_eq!(picked.id.as_deref(), Some("kitchen"));
    }

    #[test]
    fn an_exact_name_wins_over_an_earlier_partial_match() {
        let devices = devices();
        assert_eq!(pick_device(&devices, "den").unwrap().name, "Den");
        assert_eq!(
            pick_device(&devices, "gard").unwrap().name,
            "Garden Speaker"
        );
    }

    #[test]
    fn a_device_without_an_id_is_skipped() {
        let devices = devices();
        let err = pick_device(&devices, "TV").unwrap_err().to_string();
        assert!(err.contains("\"TV\""), "{err}");
        assert!(
            err.contains("Garden Speaker, Den TV (cannot be controlled), Den, Kitchen"),
            "{err}"
        );
        assert!(err.contains("open the Spotify app"), "{err}");
    }

    #[test]
    fn no_devices_at_all_is_explained() {
        let err = pick_device(&[], "Den").unwrap_err().to_string();
        assert!(err.contains("Spotify sees: none right now"), "{err}");
        assert!(err.contains("asleep?"), "{err}");
    }
}

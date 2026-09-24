//! See `config.example.toml` for every key.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Unknown keys are an error, so typos are caught.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Folder with one sub-folder per album. Relative paths are resolved
    /// against the folder the config file lives in.
    pub music_dir: PathBuf,

    /// IP address (or host name) of the speaker.
    pub speaker_host: String,
    #[serde(default)]
    pub speaker_type: SpeakerType,
    /// `None` means the default port for `speaker_type`.
    #[serde(default)]
    speaker_port: Option<u16>,

    /// Port of the built-in web server the speaker downloads music from.
    #[serde(default = "default_http_port")]
    pub http_port: u16,

    /// Address the speaker should use to reach this machine. Detected
    /// automatically if not set.
    #[serde(default)]
    pub advertise_host: Option<String>,

    /// Hard volume ceiling (0.0–1.0). The deck's volume keys never go above it.
    #[serde(default = "default_max_volume")]
    pub max_volume: f32,
    /// Volume used when the program starts.
    #[serde(default = "default_start_volume")]
    pub start_volume: f32,
    /// How much one press of a volume key changes the volume.
    #[serde(default = "default_volume_step")]
    pub volume_step: f32,

    /// Stream Deck screen brightness in percent.
    #[serde(default = "default_brightness")]
    pub brightness: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeakerType {
    /// Chromecast built-in (Google Cast).
    #[default]
    Cast,
    /// Denon / Marantz HEOS.
    Heos,
}

impl SpeakerType {
    fn default_port(self) -> u16 {
        match self {
            SpeakerType::Cast => 8009,
            SpeakerType::Heos => 1255,
        }
    }
}
fn default_http_port() -> u16 {
    8765
}
fn default_max_volume() -> f32 {
    0.4
}
fn default_start_volume() -> f32 {
    0.2
}
fn default_volume_step() -> f32 {
    0.05
}
fn default_brightness() -> u8 {
    60
}

impl Config {
    /// Out-of-range volumes are errors; `start_volume` and `brightness` are clamped into range.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, base).with_context(|| format!("invalid config in {}", path.display()))
    }

    /// A relative `music_dir` is resolved against `base`.
    fn parse(text: &str, base: &Path) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;

        if cfg.music_dir.is_relative() {
            cfg.music_dir = base.join(&cfg.music_dir);
        }

        if cfg.speaker_host.trim().is_empty() {
            bail!("speaker_host is empty");
        }
        if cfg.http_port == 0 {
            bail!("http_port must not be 0: the speaker needs a fixed port in the music URLs");
        }
        if let Some(host) = &cfg.advertise_host {
            check_advertise_host(host).context("invalid advertise_host")?;
        }
        if !(0.0..=1.0).contains(&cfg.max_volume) {
            bail!("max_volume must be between 0.0 and 1.0");
        }
        // The UI rounds volumes to a 0.001 grid, so a smaller step would never move.
        if !(0.01..=0.5).contains(&cfg.volume_step) {
            bail!("volume_step must be between 0.01 and 0.5");
        }
        if cfg.start_volume.is_nan() {
            bail!("start_volume must be a number");
        }
        cfg.start_volume = cfg.start_volume.clamp(0.0, cfg.max_volume);
        cfg.brightness = cfg.brightness.min(100);
        Ok(cfg)
    }

    pub fn speaker_port(&self) -> u16 {
        self.speaker_port
            .unwrap_or(self.speaker_type.default_port())
    }
}

/// The music URLs are `http://{host}:{port}/…`, and the file server listens on
/// IPv4 only, so the host must be an IPv4 address or a host name.
pub fn check_advertise_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("the host is empty");
    }
    if host.contains(['/', ':']) {
        bail!("{host:?} must be an IPv4 address or a host name, without scheme, port or path");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "music_dir = \"music\"\nspeaker_host = \"192.168.1.50\"\n";

    fn parse(extra: &str) -> Result<Config> {
        Config::parse(&format!("{MINIMAL}{extra}"), Path::new("/srv/deck"))
    }

    #[test]
    fn fills_in_defaults() {
        let cfg = parse("").unwrap();
        assert_eq!(cfg.speaker_type, SpeakerType::Cast);
        assert_eq!(cfg.speaker_port(), 8009);
        assert_eq!(cfg.http_port, 8765);
        assert_eq!(cfg.advertise_host, None);
        assert!((cfg.max_volume - 0.4).abs() < f32::EPSILON);
        assert!((cfg.start_volume - 0.2).abs() < f32::EPSILON);
        assert_eq!(cfg.brightness, 60);
    }

    #[test]
    fn a_heos_speaker_defaults_to_the_heos_port() {
        let cfg = parse("speaker_type = \"heos\"\n").unwrap();
        assert_eq!(cfg.speaker_type, SpeakerType::Heos);
        assert_eq!(cfg.speaker_port(), 1255);
    }

    #[test]
    fn an_explicit_speaker_port_wins() {
        assert_eq!(parse("speaker_port = 9000\n").unwrap().speaker_port(), 9000);
    }

    #[test]
    fn rejects_an_unknown_speaker_type() {
        assert!(parse("speaker_type = \"airplay\"\n").is_err());
    }

    #[test]
    fn resolves_relative_music_dir_against_config_folder() {
        assert_eq!(parse("").unwrap().music_dir, Path::new("/srv/deck/music"));
    }

    #[test]
    fn keeps_absolute_music_dir() {
        let cfg = Config::parse(
            "music_dir = \"/data/music\"\nspeaker_host = \"x\"\n",
            Path::new("/srv/deck"),
        )
        .unwrap();
        assert_eq!(cfg.music_dir, Path::new("/data/music"));
    }

    #[test]
    fn clamps_start_volume_to_max_volume() {
        let cfg = parse("max_volume = 0.3\nstart_volume = 0.9\n").unwrap();
        assert!((cfg.start_volume - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn clamps_brightness_to_100() {
        assert_eq!(parse("brightness = 250\n").unwrap().brightness, 100);
    }

    #[test]
    fn rejects_max_volume_above_one() {
        let err = parse("max_volume = 1.5\n").unwrap_err();
        assert!(err.to_string().contains("max_volume"), "{err:#}");
    }

    #[test]
    fn rejects_zero_volume_step() {
        let err = parse("volume_step = 0.0\n").unwrap_err();
        assert!(err.to_string().contains("volume_step"), "{err:#}");
    }

    #[test]
    fn rejects_volume_step_below_one_hundredth() {
        let err = parse("volume_step = 0.005\n").unwrap_err();
        assert!(err.to_string().contains("volume_step"), "{err:#}");
    }

    #[test]
    fn rejects_nan_volume_step() {
        let err = parse("volume_step = nan\n").unwrap_err();
        assert!(err.to_string().contains("volume_step"), "{err:#}");
    }

    #[test]
    fn rejects_nan_start_volume() {
        let err = parse("start_volume = nan\n").unwrap_err();
        assert!(err.to_string().contains("start_volume"), "{err:#}");
    }

    #[test]
    fn rejects_http_port_zero() {
        let err = parse("http_port = 0\n").unwrap_err();
        assert!(err.to_string().contains("http_port"), "{err:#}");
    }

    #[test]
    fn rejects_empty_speaker_host() {
        let err =
            Config::parse("music_dir = \"m\"\nspeaker_host = \"\"\n", Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("speaker_host"), "{err:#}");
    }

    #[test]
    fn rejects_empty_advertise_host() {
        let err = parse("advertise_host = \"\"\n").unwrap_err();
        assert!(err.to_string().contains("advertise_host"), "{err:#}");
    }

    #[test]
    fn rejects_advertise_host_with_scheme() {
        let err = parse("advertise_host = \"http://10.0.0.2\"\n").unwrap_err();
        assert!(err.to_string().contains("advertise_host"), "{err:#}");
    }

    #[test]
    fn rejects_advertise_host_with_port() {
        let err = parse("advertise_host = \"10.0.0.2:8765\"\n").unwrap_err();
        assert!(err.to_string().contains("advertise_host"), "{err:#}");
    }

    #[test]
    fn rejects_ipv6_advertise_host() {
        let err = parse("advertise_host = \"fe80::2\"\n").unwrap_err();
        assert!(err.to_string().contains("advertise_host"), "{err:#}");
    }

    #[test]
    fn accepts_ipv4_and_host_name_as_advertise_host() {
        for host in ["192.168.1.20", "pi.local"] {
            let cfg = parse(&format!("advertise_host = \"{host}\"\n")).unwrap();
            assert_eq!(cfg.advertise_host.as_deref(), Some(host));
        }
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(parse("max_volum = 0.3\n").is_err());
    }

    #[test]
    fn requires_speaker_host() {
        assert!(Config::parse("music_dir = \"music\"\n", Path::new(".")).is_err());
    }
}

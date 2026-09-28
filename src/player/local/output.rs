//! The sound card, through cpal: finds the output, opens a stream in the
//! output's own format, and hands each of its buffers to [`Sink::fill`].
//!
//! The stream keeps running from the first album to the end of the program,
//! playing silence between albums; only an error closes it.

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, ErrorKind, FromSample, Host, SampleFormat, SizedSample, Stream, StreamConfig};
use tracing::{debug, info};

use super::engine::{Engine, Failures, Format, Sink};

/// An open sound output and the engine that feeds it.
pub(super) struct OpenOutput {
    pub(super) engine: Engine,
    /// `None` in tests, which play the sink themselves.
    _stream: Option<Stream>,
}

impl OpenOutput {
    /// `wanted` is part of an output's name, as [`output_devices`] lists it;
    /// `None` is the system's default output.
    pub(super) fn open(wanted: Option<&str>) -> Result<OpenOutput> {
        let host = cpal::default_host();
        let (name, device) = find(&host, wanted)?;
        let supported = device
            .default_output_config()
            .with_context(|| format!("{name} has no output format"))?;
        let config = supported.config();
        let (engine, sink) = Engine::new(Format {
            rate: config.sample_rate,
            channels: usize::from(config.channels),
        })?;
        let failures = engine.failures();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, sink, failures),
            SampleFormat::F64 => build::<f64>(&device, config, sink, failures),
            SampleFormat::I8 => build::<i8>(&device, config, sink, failures),
            SampleFormat::I16 => build::<i16>(&device, config, sink, failures),
            SampleFormat::I24 => build::<cpal::I24>(&device, config, sink, failures),
            SampleFormat::I32 => build::<i32>(&device, config, sink, failures),
            SampleFormat::I64 => build::<i64>(&device, config, sink, failures),
            SampleFormat::U8 => build::<u8>(&device, config, sink, failures),
            SampleFormat::U16 => build::<u16>(&device, config, sink, failures),
            SampleFormat::U24 => build::<cpal::U24>(&device, config, sink, failures),
            SampleFormat::U32 => build::<u32>(&device, config, sink, failures),
            SampleFormat::U64 => build::<u64>(&device, config, sink, failures),
            other => bail!("{name} wants {other} samples, which deckjay cannot make"),
        }
        .with_context(|| format!("cannot open {name}"))?;
        stream
            .play()
            .with_context(|| format!("cannot start {name}"))?;
        info!(
            output = %name,
            rate = config.sample_rate,
            channels = config.channels,
            format = %supported.sample_format(),
            "sound output open"
        );
        Ok(OpenOutput {
            engine,
            _stream: Some(stream),
        })
    }

    #[cfg(test)]
    pub(super) fn without_device(engine: Engine) -> OpenOutput {
        OpenOutput {
            engine,
            _stream: None,
        }
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &Device,
    config: StreamConfig,
    mut sink: Sink,
    failures: Failures,
) -> Result<Stream, cpal::Error> {
    device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| sink.fill(out),
        move |err: cpal::Error| match err.kind() {
            // Playback goes on after these.
            ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
                debug!("sound output: {err}");
            }
            _ => failures.report(err.to_string()),
        },
        None,
    )
}

fn find(host: &Host, wanted: Option<&str>) -> Result<(String, Device)> {
    let Some(wanted) = wanted else {
        let device = host
            .default_output_device()
            .context("this computer has no sound output")?;
        return Ok((label(&device), device));
    };
    let mut outputs = outputs(host)?;
    let names: Vec<String> = outputs.iter().map(|(name, _)| name.clone()).collect();
    let index = pick(&names, wanted)?;
    Ok(outputs.swap_remove(index))
}

/// Names of the sound outputs, the default one first, for `--check`.
pub fn output_devices() -> Result<Vec<String>> {
    let outputs = outputs(&cpal::default_host())?;
    Ok(outputs.into_iter().map(|(name, _)| name).collect())
}

fn outputs(host: &Host) -> Result<Vec<(String, Device)>> {
    let default = host.default_output_device();
    let default_id = default.as_ref().and_then(|device| device.id().ok());
    let others = host
        .output_devices()
        .context("cannot list the sound outputs")?
        .filter(|device| default_id.is_none() || device.id().ok() != default_id);
    Ok(default
        .into_iter()
        .chain(others)
        .map(|device| (label(&device), device))
        .collect())
}

/// The output's name. On Linux one sound card has several ALSA devices with
/// the same name (`hw:`, `plughw:`, `sysdefault:`, `hdmi:` ...), so the ALSA
/// device follows in brackets.
fn label(device: &Device) -> String {
    match device.description() {
        Ok(description) => match description.driver() {
            Some(driver) if driver != description.name() => {
                format!("{} ({driver})", description.name())
            }
            _ => description.name().to_owned(),
        },
        Err(_) => device
            .id()
            .map_or_else(|_| "unknown output".to_owned(), |id| id.to_string()),
    }
}

/// The first of `names` that contains `wanted`, ignoring case.
pub(super) fn pick(names: &[String], wanted: &str) -> Result<usize> {
    let needle = wanted.to_lowercase();
    names
        .iter()
        .position(|name| name.to_lowercase().contains(&needle))
        .ok_or_else(|| {
            if names.is_empty() {
                anyhow!("no sound output matches audio_device {wanted:?}: there are none")
            } else {
                anyhow!(
                    "no sound output matches audio_device {wanted:?}; there are: {}",
                    names.join(", ")
                )
            }
        })
}

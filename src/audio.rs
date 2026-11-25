//! The audio formats the speakers play.

/// File extension and content type of the formats the Chromecast default
/// receiver can play. The library scan and the podcast feed read this one table.
pub const AUDIO_TYPES: &[(&str, &str)] = &[
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("m4b", "audio/mp4"),
    ("mp4", "audio/mp4"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("wav", "audio/wav"),
];

/// The content type for an extension without the dot, any case.
pub fn audio_type(ext: &str) -> Option<&'static str> {
    AUDIO_TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(ext))
        .map(|&(_, content_type)| content_type)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_case_extensions_have_their_content_type() {
        for (ext, content_type) in [
            ("MP3", "audio/mpeg"),
            ("M4A", "audio/mp4"),
            ("M4B", "audio/mp4"),
            ("MP4", "audio/mp4"),
            ("AAC", "audio/aac"),
            ("FLAC", "audio/flac"),
            ("OGG", "audio/ogg"),
            ("OGA", "audio/ogg"),
            ("OPUS", "audio/ogg"),
            ("WAV", "audio/wav"),
        ] {
            assert_eq!(audio_type(ext), Some(content_type), "{ext}");
        }
    }
}

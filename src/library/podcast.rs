//! Podcast sources as shelves: their cached episodes as items.

use std::path::{Path, PathBuf};

use super::{Item, ItemKey, Kind, Media, Track, scan};
use crate::config::Source;
use crate::podcasts::Snapshot;

impl ItemKey {
    /// The last segment of the key: the episode id of a podcast item
    /// (`<source>/<feed folder>/<episode id>`).
    pub fn episode_id(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(_, id)| id)
    }
}

/// One item per episode, feed after feed. Keys are
/// `<source>/<feed folder>/<episode id>`, so progress follows an episode
/// across refreshes.
pub fn items(source: &Source, snapshot: &Snapshot) -> Vec<Item> {
    let served = |path: &Path| -> Option<PathBuf> {
        Some(Path::new(&source.name).join(path.strip_prefix(&source.path).ok()?))
    };
    snapshot
        .feeds
        .iter()
        .flat_map(|feed| {
            feed.episodes.iter().filter_map(move |episode| {
                let track = Track {
                    content_type: scan::content_type(&episode.file)?,
                    path: episode.file.clone(),
                    rel_path: Path::new(&source.name).join(&episode.rel),
                    title: episode.title.clone(),
                };
                let cover = episode.picture.clone().or_else(|| feed.picture.clone());
                Some(Item {
                    kind: Kind::Podcast,
                    key: ItemKey(format!("{}/{}/{}", source.name, feed.slug, episode.id)),
                    name: episode.title.clone(),
                    media: Media::Tracks(vec![track]),
                    cover_rel: cover.as_deref().and_then(served),
                    cover,
                    picture: None,
                    color: None,
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;

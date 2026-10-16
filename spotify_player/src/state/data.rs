use std::io::{BufReader, BufWriter};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::{de::DeserializeOwned, Serialize};
use std::sync::LazyLock;

use super::model::{
    Album, Artist, Category, Context, ContextId, Id, Playlist, PlaylistFolderItem,
    PlaylistFolderNode, SearchResults, Show, Track,
};
use super::Lyrics;

pub type DataReadGuard<'a> = parking_lot::RwLockReadGuard<'a, AppData>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum FileCacheKey {
    Playlists,
    PlaylistFolders,
    FollowedArtists,
    SavedShows,
    SavedAlbums,
    SavedTracks,
}

/// default time-to-live cache duration
pub static TTL_CACHE_DURATION: LazyLock<std::time::Duration> =
    LazyLock::new(|| std::time::Duration::from_hours(1));

/// the application's data
pub struct AppData {
    pub user_data: UserData,
    pub caches: MemoryCaches,
    pub browse: BrowseData,
}

#[derive(Debug)]
/// current user's data
pub struct UserData {
    pub user: Option<rspotify::model::PrivateUser>,
    pub playlists: Vec<PlaylistFolderItem>,
    pub playlist_folder_node: Option<PlaylistFolderNode>,
    pub followed_artists: Vec<Artist>,
    pub saved_shows: Vec<Show>,
    pub saved_albums: Vec<Album>,
    pub saved_tracks: HashMap<String, Track>,
    /// Library collections already loaded from a file cache or Spotify, which are only
    /// re-downloaded on an explicit refresh.
    pub loaded: HashSet<FileCacheKey>,
}

#[derive(Debug)]
pub enum SearchCacheEntry {
    Loading,
    Ready(SearchResults),
    Failed,
}

/// the application's in-memory caches
pub struct MemoryCaches {
    pub context: ttl_cache::TtlCache<String, Context>,
    pub search: ttl_cache::TtlCache<String, SearchCacheEntry>,
    pub lyrics: ttl_cache::TtlCache<String, Option<Lyrics>>,
    pub genres: ttl_cache::TtlCache<String, Vec<String>>,
    #[cfg(feature = "image")]
    pub images: ttl_cache::TtlCache<String, image::DynamicImage>,
}

#[derive(Default, Debug)]
/// Spotify browse data
pub struct BrowseData {
    pub categories: Option<Vec<Category>>,
    pub category_playlists: HashMap<String, Vec<Playlist>>,
}

impl MemoryCaches {
    pub fn new() -> Self {
        Self {
            context: ttl_cache::TtlCache::new(64),
            search: ttl_cache::TtlCache::new(64),
            lyrics: ttl_cache::TtlCache::new(64),
            genres: ttl_cache::TtlCache::new(64),
            #[cfg(feature = "image")]
            images: ttl_cache::TtlCache::new(64),
        }
    }

    pub fn begin_search(&mut self, query: &str) -> bool {
        let should_begin = matches!(
            self.search.get(query),
            None | Some(SearchCacheEntry::Failed)
        );

        if should_begin {
            self.search.insert(
                query.to_string(),
                SearchCacheEntry::Loading,
                *TTL_CACHE_DURATION,
            );
        }

        should_begin
    }

    pub fn complete_search(&mut self, query: String, results: SearchResults) {
        self.search
            .insert(query, SearchCacheEntry::Ready(results), *TTL_CACHE_DURATION);
    }

    pub fn fail_search(&mut self, query: String) {
        if matches!(self.search.get(&query), Some(SearchCacheEntry::Loading)) {
            self.search
                .insert(query, SearchCacheEntry::Failed, *TTL_CACHE_DURATION);
        }
    }

    pub fn search_results(&self, query: &str) -> Option<&SearchResults> {
        match self.search.get(query) {
            Some(SearchCacheEntry::Ready(results)) => Some(results),
            _ => None,
        }
    }
}

impl AppData {
    pub fn new(cache_folder: &Path) -> Self {
        Self {
            user_data: UserData::new_from_file_caches(cache_folder),
            caches: MemoryCaches::new(),
            browse: BrowseData::default(),
        }
    }

    /// Get a list of tracks inside a given context
    pub fn context_tracks_mut(&mut self, id: &ContextId) -> Option<&mut Vec<Track>> {
        let c = self.caches.context.get_mut(&id.uri())?;

        Some(match c {
            Context::Album { tracks, .. }
            | Context::Playlist { tracks, .. }
            | Context::Tracks { tracks, .. }
            | Context::Artist {
                top_tracks: tracks, ..
            } => tracks,
            Context::Show { .. } => {
                return None;
            }
        })
    }

    pub fn context_tracks(&self, id: &ContextId) -> Option<&Vec<Track>> {
        let c = self.caches.context.get(&id.uri())?;
        Some(match c {
            Context::Album { tracks, .. }
            | Context::Playlist { tracks, .. }
            | Context::Tracks { tracks, .. }
            | Context::Artist {
                top_tracks: tracks, ..
            } => tracks,
            Context::Show { .. } => {
                return None;
            }
        })
    }
}

impl UserData {
    /// Construct a new user data based on file caches
    pub fn new_from_file_caches(cache_folder: &Path) -> Self {
        fn load<T: DeserializeOwned>(
            key: FileCacheKey,
            cache_folder: &Path,
            loaded: &mut HashSet<FileCacheKey>,
        ) -> Option<T> {
            let data = load_data_from_file_cache(key, cache_folder);
            if data.is_some() {
                loaded.insert(key);
            }
            data
        }

        let mut loaded = HashSet::new();
        let playlists =
            load(FileCacheKey::Playlists, cache_folder, &mut loaded).unwrap_or_default();
        let playlist_folder_node = load(FileCacheKey::PlaylistFolders, cache_folder, &mut loaded);
        let followed_artists =
            load(FileCacheKey::FollowedArtists, cache_folder, &mut loaded).unwrap_or_default();
        let saved_shows =
            load(FileCacheKey::SavedShows, cache_folder, &mut loaded).unwrap_or_default();
        let saved_albums =
            load(FileCacheKey::SavedAlbums, cache_folder, &mut loaded).unwrap_or_default();
        let saved_tracks: HashMap<String, Track> =
            load(FileCacheKey::SavedTracks, cache_folder, &mut loaded).unwrap_or_default();

        // caches written before liked dates were stored cannot order the liked tracks,
        // so they are re-downloaded once
        if saved_tracks.values().any(|track| track.added_at == 0) {
            loaded.remove(&FileCacheKey::SavedTracks);
        }

        Self {
            user: None,
            playlists,
            playlist_folder_node,
            followed_artists,
            saved_shows,
            saved_albums,
            saved_tracks,
            loaded,
        }
    }

    pub fn is_loaded(&self, key: FileCacheKey) -> bool {
        self.loaded.contains(&key)
    }

    /// Persist the in-memory copy of a library collection into its file cache.
    pub fn store_into_file_cache(
        &self,
        key: FileCacheKey,
        cache_folder: &Path,
    ) -> std::io::Result<()> {
        match key {
            FileCacheKey::Playlists => {
                store_data_into_file_cache(key, cache_folder, &self.playlists)
            }
            // playlist folders are generated outside the app and never changed by it
            FileCacheKey::PlaylistFolders => Ok(()),
            FileCacheKey::FollowedArtists => {
                store_data_into_file_cache(key, cache_folder, &self.followed_artists)
            }
            FileCacheKey::SavedShows => {
                store_data_into_file_cache(key, cache_folder, &self.saved_shows)
            }
            FileCacheKey::SavedAlbums => {
                store_data_into_file_cache(key, cache_folder, &self.saved_albums)
            }
            FileCacheKey::SavedTracks => {
                store_data_into_file_cache(key, cache_folder, &self.saved_tracks)
            }
        }
    }

    /// Get the user's liked tracks, most recently liked first (the order Spotify returns them in)
    pub fn liked_tracks(&self) -> Vec<Track> {
        let mut tracks: Vec<Track> = self.saved_tracks.values().cloned().collect();
        tracks.sort_by(|x, y| {
            y.added_at
                .cmp(&x.added_at)
                .then_with(|| x.id.uri().cmp(&y.id.uri()))
        });
        tracks
    }

    /// Get a list of playlist items that are **possibly** modifiable by user
    ///
    /// If `folder_id` is provided, returns items in the given folder id.
    /// Otherwise, returns the all items.
    pub fn modifiable_playlist_items(&self, folder_id: Option<usize>) -> Vec<&PlaylistFolderItem> {
        match self.user {
            None => vec![],
            Some(ref u) => self
                .playlists
                .iter()
                // filter items in a folder (if specified)
                .filter(|item| {
                    if let Some(folder_id) = folder_id {
                        match item {
                            PlaylistFolderItem::Playlist(p) => p.current_folder_id == folder_id,
                            PlaylistFolderItem::Folder(f) => f.current_id == folder_id,
                        }
                    } else {
                        true
                    }
                })
                // filter modifiable items
                .filter(|item| match item {
                    PlaylistFolderItem::Playlist(p) => p.owner.1 == u.id || p.collaborative,
                    PlaylistFolderItem::Folder(_) => true,
                })
                .collect(),
        }
    }

    /// Get playlists items for the given folder id
    pub fn folder_playlists_items(&self, folder_id: usize) -> Vec<&PlaylistFolderItem> {
        self.playlists
            .iter()
            .filter(|item| match item {
                PlaylistFolderItem::Playlist(p) => p.current_folder_id == folder_id,
                PlaylistFolderItem::Folder(f) => f.current_id == folder_id,
            })
            .collect()
    }

    /// Check if a track is a liked track
    pub fn is_liked_track(&self, track: &Track) -> bool {
        self.saved_tracks.contains_key(&track.id.uri())
    }

    /// Get the user's liked tracks by the given artist, sorted by liked date (newest first)
    pub fn liked_tracks_by_artist(&self, artist: &Artist) -> Vec<Track> {
        let mut tracks: Vec<Track> = self
            .saved_tracks
            .values()
            .filter(|t| t.artists.iter().any(|a| a.id == artist.id))
            .cloned()
            .collect();
        tracks.sort_by_key(|t| std::cmp::Reverse(t.added_at));
        tracks
    }

    /// Check if a playlist is followed
    pub fn is_followed_playlist(&self, playlist: &Playlist) -> bool {
        self.playlists.iter().any(|x| match x {
            PlaylistFolderItem::Playlist(p) => p.id == playlist.id,
            PlaylistFolderItem::Folder(_) => false,
        })
    }
}

pub fn store_data_into_file_cache<T: Serialize>(
    key: FileCacheKey,
    cache_folder: &Path,
    data: &T,
) -> std::io::Result<()> {
    let path = cache_folder.join(format!("{key:?}_cache.json"));
    let f = BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(f, data)?;
    Ok(())
}

pub fn load_data_from_file_cache<T>(key: FileCacheKey, cache_folder: &Path) -> Option<T>
where
    T: DeserializeOwned,
{
    let path = cache_folder.join(format!("{key:?}_cache.json"));
    if path.exists() {
        tracing::info!("Loading {key:?} data from {}...", path.display());
        let f = BufReader::new(std::fs::File::open(path).expect("path exists"));
        match serde_json::from_reader(f) {
            Ok(data) => {
                tracing::info!("Successfully loaded {key:?} data!");
                Some(data)
            }
            Err(err) => {
                tracing::error!("Failed to load {key:?} data: {err:#}");
                None
            }
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        store_data_into_file_cache, FileCacheKey, Id, MemoryCaches, SearchCacheEntry,
        SearchResults, Track, UserData,
    };
    use rspotify::model::TrackId;
    use std::{collections::HashMap, path::PathBuf};

    /// A cache folder unique to one test, removed when dropped.
    struct TempCacheFolder(PathBuf);

    impl TempCacheFolder {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("spotify-player-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempCacheFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn track(id: &str, added_at: u64) -> Track {
        Track {
            id: TrackId::from_id(id).unwrap().into_static(),
            name: id.to_string(),
            artists: vec![],
            album: None,
            duration: std::time::Duration::from_secs(1),
            explicit: false,
            added_at,
        }
    }

    fn tracks_by_uri(tracks: impl IntoIterator<Item = Track>) -> HashMap<String, Track> {
        tracks.into_iter().map(|t| (t.id.uri(), t)).collect()
    }

    #[test]
    fn only_cached_collections_are_loaded() {
        let folder = TempCacheFolder::new("only-cached");
        store_data_into_file_cache(FileCacheKey::SavedAlbums, &folder.0, &Vec::<()>::new())
            .unwrap();
        store_data_into_file_cache(
            FileCacheKey::SavedTracks,
            &folder.0,
            &tracks_by_uri([track("a", 1)]),
        )
        .unwrap();

        let user_data = UserData::new_from_file_caches(&folder.0);

        assert!(user_data.is_loaded(FileCacheKey::SavedAlbums));
        assert!(user_data.is_loaded(FileCacheKey::SavedTracks));
        assert!(!user_data.is_loaded(FileCacheKey::Playlists));
        assert!(!user_data.is_loaded(FileCacheKey::FollowedArtists));
    }

    #[test]
    fn saved_tracks_cache_without_liked_dates_is_not_loaded() {
        let folder = TempCacheFolder::new("no-liked-dates");
        store_data_into_file_cache(
            FileCacheKey::SavedTracks,
            &folder.0,
            &tracks_by_uri([track("a", 1), track("b", 0)]),
        )
        .unwrap();

        let user_data = UserData::new_from_file_caches(&folder.0);

        assert_eq!(user_data.saved_tracks.len(), 2);
        assert!(!user_data.is_loaded(FileCacheKey::SavedTracks));
    }

    #[test]
    fn stored_saved_tracks_keep_liked_dates() {
        let folder = TempCacheFolder::new("store-liked-dates");
        let mut user_data = UserData::new_from_file_caches(&folder.0);
        user_data.saved_tracks = tracks_by_uri([track("a", 42)]);
        user_data
            .store_into_file_cache(FileCacheKey::SavedTracks, &folder.0)
            .unwrap();

        let reloaded = UserData::new_from_file_caches(&folder.0);

        assert!(reloaded.is_loaded(FileCacheKey::SavedTracks));
        assert_eq!(reloaded.liked_tracks()[0].added_at, 42);
    }

    #[test]
    fn liked_tracks_are_ordered_most_recent_first() {
        let folder = TempCacheFolder::new("liked-order");
        let mut user_data = UserData::new_from_file_caches(&folder.0);
        user_data.saved_tracks = tracks_by_uri([track("old", 1), track("new", 3), track("mid", 2)]);

        let names: Vec<String> = user_data
            .liked_tracks()
            .into_iter()
            .map(|t| t.name)
            .collect();

        assert_eq!(names, ["new", "mid", "old"]);
    }

    #[test]
    fn search_lifecycle_suppresses_duplicate_requests() {
        let mut caches = MemoryCaches::new();

        assert!(caches.begin_search("query"));
        assert!(matches!(
            caches.search.get("query"),
            Some(SearchCacheEntry::Loading)
        ));
        assert!(!caches.begin_search("query"));

        caches.complete_search("query".to_string(), SearchResults::default());

        assert!(caches.search_results("query").is_some());
        assert!(!caches.begin_search("query"));
    }

    #[test]
    fn failed_search_can_be_retried() {
        let mut caches = MemoryCaches::new();

        assert!(caches.begin_search("query"));
        caches.fail_search("query".to_string());

        assert!(matches!(
            caches.search.get("query"),
            Some(SearchCacheEntry::Failed)
        ));
        assert!(caches.begin_search("query"));
        assert!(matches!(
            caches.search.get("query"),
            Some(SearchCacheEntry::Loading)
        ));
    }
}

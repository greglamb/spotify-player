use std::io::{BufReader, BufWriter};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::LazyLock;

use super::model::{
    Album, Artist, Category, Context, ContextId, Id, Playlist, PlaylistFolderItem,
    PlaylistFolderNode, SearchResults, Show, Track,
};
use super::Lyrics;
use rspotify::model::PlaylistId;

pub type DataReadGuard<'a> = parking_lot::RwLockReadGuard<'a, AppData>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileCacheKey {
    Playlists,
    PlaylistFolders,
    FollowedArtists,
    SavedShows,
    SavedAlbums,
    SavedTracks,
    LibrarySync,
}

/// Library collections that can be downloaded from Spotify
pub const LIBRARY_COLLECTIONS: [FileCacheKey; 5] = [
    FileCacheKey::Playlists,
    FileCacheKey::FollowedArtists,
    FileCacheKey::SavedAlbums,
    FileCacheKey::SavedShows,
    FileCacheKey::SavedTracks,
];

/// Spotify's state of a collection ordered by date added (liked tracks, saved albums) when it
/// was last synced, so a refresh can download only the items added since.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySync {
    /// Number of items Spotify reports, including ones the app drops (e.g. unplayable tracks)
    pub total: usize,
    pub newest_added_at: u64,
}

impl LibrarySync {
    /// The sync state of a fully downloaded collection.
    pub fn from_items<T>(items: &[T], added_at: impl Fn(&T) -> u64) -> Self {
        Self {
            total: items.len(),
            newest_added_at: items.iter().map(added_at).max().unwrap_or_default(),
        }
    }

    /// Whether Spotify's current `total` is explained by `new_count` items added since the sync,
    /// `re_added` of which were already in the collection (re-liked or re-saved).
    fn only_added(self, new_count: usize, re_added: usize, total: usize) -> bool {
        self.total + new_count - re_added == total
    }
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
    pub sync: HashMap<FileCacheKey, LibrarySync>,
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
            sync: load_data_from_file_cache(FileCacheKey::LibrarySync, cache_folder)
                .unwrap_or_default(),
        }
    }

    /// Sync state of a collection whose cached copy can be brought up to date incrementally.
    pub fn incremental_sync(&self, key: FileCacheKey) -> Option<LibrarySync> {
        if self.is_loaded(key) {
            self.sync.get(&key).copied()
        } else {
            None
        }
    }

    /// Keep a collection's sync state in step with an item added to or removed from it in the app.
    pub fn record_library_change(&mut self, key: FileCacheKey, added: bool) {
        if let Some(sync) = self.sync.get_mut(&key) {
            sync.total = if added {
                sync.total + 1
            } else {
                sync.total.saturating_sub(1)
            };
        }
    }

    /// Merge tracks liked since the last sync, given the `new` sync state of the items Spotify
    /// returned (including unplayable tracks absent from `tracks`) and its current `total`.
    ///
    /// Returns `false`, changing nothing, when `total` shows other changes (e.g. tracks unliked
    /// in another app) that only a full download can pick up.
    pub fn merge_liked_tracks(
        &mut self,
        tracks: Vec<Track>,
        new: LibrarySync,
        total: usize,
    ) -> bool {
        let Some(sync) = self.sync.get(&FileCacheKey::SavedTracks).copied() else {
            return false;
        };
        let re_liked = tracks
            .iter()
            .filter(|t| self.saved_tracks.contains_key(&t.id.uri()))
            .count();
        if !sync.only_added(new.total, re_liked, total) {
            return false;
        }

        self.saved_tracks
            .extend(tracks.into_iter().map(|t| (t.id.uri(), t)));
        self.sync.insert(
            FileCacheKey::SavedTracks,
            LibrarySync {
                total,
                newest_added_at: new.newest_added_at.max(sync.newest_added_at),
            },
        );
        true
    }

    /// Merge albums saved since the last sync, given Spotify's current `total`; see
    /// [`Self::merge_liked_tracks`].
    pub fn merge_saved_albums(&mut self, albums: Vec<Album>, total: usize) -> bool {
        let Some(sync) = self.sync.get(&FileCacheKey::SavedAlbums).copied() else {
            return false;
        };
        let re_saved = albums
            .iter()
            .filter(|a| self.saved_albums.iter().any(|s| s.id == a.id))
            .count();
        if !sync.only_added(albums.len(), re_saved, total) {
            return false;
        }

        let new = LibrarySync::from_items(&albums, |a| a.added_at);
        self.saved_albums
            .retain(|s| !albums.iter().any(|a| a.id == s.id));
        self.saved_albums.splice(0..0, albums);
        self.sync.insert(
            FileCacheKey::SavedAlbums,
            LibrarySync {
                total,
                newest_added_at: new.newest_added_at.max(sync.newest_added_at),
            },
        );
        true
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
            FileCacheKey::LibrarySync => store_data_into_file_cache(key, cache_folder, &self.sync),
        }
    }

    /// The `snapshot_id` recorded for a playlist in the user's playlist list.
    pub fn playlist_snapshot(&self, id: &PlaylistId<'_>) -> Option<&str> {
        self.playlists.iter().find_map(|item| match item {
            PlaylistFolderItem::Playlist(p) if p.id == *id => Some(p.snapshot_id.as_str()),
            _ => None,
        })
    }

    /// Record a newer `snapshot_id` for a playlist in the user's playlist list, returning
    /// whether the list changed.
    pub fn update_playlist_snapshot(&mut self, id: &PlaylistId<'_>, snapshot_id: &str) -> bool {
        for item in &mut self.playlists {
            if let PlaylistFolderItem::Playlist(p) = item {
                if p.id == *id && p.snapshot_id != snapshot_id {
                    p.snapshot_id = snapshot_id.to_string();
                    return true;
                }
            }
        }
        false
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

/// A playlist's tracks as cached on disk; valid while `playlist.snapshot_id` matches Spotify's.
#[derive(Deserialize)]
pub struct CachedPlaylist {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

/// Serialized form of [`CachedPlaylist`], borrowing the data to store.
#[derive(Serialize)]
struct CachedPlaylistRef<'a> {
    playlist: &'a Playlist,
    tracks: &'a [Track],
}

impl CachedPlaylist {
    pub fn into_context(self) -> Context {
        Context::Playlist {
            playlist: self.playlist,
            tracks: self.tracks,
        }
    }
}

fn playlist_cache_path(cache_folder: &Path, id: &PlaylistId<'_>) -> PathBuf {
    cache_folder
        .join("playlists")
        .join(format!("{}.json", id.id()))
}

pub fn store_playlist_into_file_cache(
    cache_folder: &Path,
    playlist: &Playlist,
    tracks: &[Track],
) -> std::io::Result<()> {
    let path = playlist_cache_path(cache_folder, &playlist.id);
    std::fs::create_dir_all(path.parent().expect("playlist cache path has a parent"))?;
    let f = BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(f, &CachedPlaylistRef { playlist, tracks })?;
    Ok(())
}

pub fn load_playlist_from_file_cache(
    cache_folder: &Path,
    id: &PlaylistId<'_>,
) -> Option<CachedPlaylist> {
    let path = playlist_cache_path(cache_folder, id);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::error!("Failed to open {}: {err:#}", path.display());
            return None;
        }
    };
    match serde_json::from_reader(BufReader::new(file)) {
        Ok(cached) => Some(cached),
        Err(err) => {
            tracing::error!("Failed to load {}: {err:#}", path.display());
            None
        }
    }
}

pub fn remove_playlist_from_file_cache(
    cache_folder: &Path,
    id: &PlaylistId<'_>,
) -> std::io::Result<()> {
    match std::fs::remove_file(playlist_cache_path(cache_folder, id)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
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
        load_playlist_from_file_cache, remove_playlist_from_file_cache, store_data_into_file_cache,
        store_playlist_into_file_cache, Album, FileCacheKey, Id, LibrarySync, MemoryCaches,
        Playlist, PlaylistFolderItem, SearchCacheEntry, SearchResults, Track, UserData,
    };
    use rspotify::model::{AlbumId, PlaylistId, TrackId, UserId};
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

    fn playlist(id: &str, snapshot_id: &str) -> Playlist {
        Playlist {
            id: PlaylistId::from_id(id).unwrap().into_static(),
            collaborative: false,
            name: id.to_string(),
            owner: (
                "owner".to_string(),
                UserId::from_id("owner").unwrap().into_static(),
            ),
            desc: String::new(),
            current_folder_id: 0,
            snapshot_id: snapshot_id.to_string(),
        }
    }

    #[test]
    fn cached_playlist_round_trips() {
        let folder = TempCacheFolder::new("playlist-round-trip");
        let playlist = playlist("p", "v1");
        store_playlist_into_file_cache(&folder.0, &playlist, &[track("a", 7)]).unwrap();

        let cached = load_playlist_from_file_cache(&folder.0, &playlist.id).unwrap();
        assert_eq!(cached.playlist.snapshot_id, "v1");
        assert_eq!(cached.tracks.len(), 1);
        assert_eq!(cached.tracks[0].added_at, 7);

        remove_playlist_from_file_cache(&folder.0, &playlist.id).unwrap();
        assert!(load_playlist_from_file_cache(&folder.0, &playlist.id).is_none());
        // removing a playlist that is not cached is not an error
        remove_playlist_from_file_cache(&folder.0, &playlist.id).unwrap();
    }

    #[test]
    fn records_newer_playlist_snapshots_in_the_list() {
        let folder = TempCacheFolder::new("playlist-snapshots");
        let mut user_data = UserData::new_from_file_caches(&folder.0);
        user_data.playlists = vec![PlaylistFolderItem::Playlist(playlist("p", "v1"))];
        let id = PlaylistId::from_id("p").unwrap();
        let unknown = PlaylistId::from_id("q").unwrap();

        assert_eq!(user_data.playlist_snapshot(&id), Some("v1"));
        assert_eq!(user_data.playlist_snapshot(&unknown), None);

        assert!(user_data.update_playlist_snapshot(&id, "v2"));
        assert!(!user_data.update_playlist_snapshot(&id, "v2"));
        assert!(!user_data.update_playlist_snapshot(&unknown, "v2"));
        assert_eq!(user_data.playlist_snapshot(&id), Some("v2"));
    }

    fn album(id: &str, added_at: u64) -> Album {
        Album {
            id: AlbumId::from_id(id).unwrap().into_static(),
            release_date: String::new(),
            name: id.to_string(),
            artists: vec![],
            typ: None,
            added_at,
        }
    }

    /// User data whose liked tracks `a` and `b` are in sync with Spotify.
    fn synced_liked_tracks(folder: &TempCacheFolder) -> UserData {
        let mut user_data = UserData::new_from_file_caches(&folder.0);
        user_data.saved_tracks = tracks_by_uri([track("a", 1), track("b", 2)]);
        user_data.sync.insert(
            FileCacheKey::SavedTracks,
            LibrarySync {
                total: 2,
                newest_added_at: 2,
            },
        );
        user_data
    }

    fn liked_names(user_data: &UserData) -> Vec<String> {
        user_data
            .liked_tracks()
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    #[test]
    fn merges_newly_liked_tracks() {
        let folder = TempCacheFolder::new("merge-new-likes");
        let mut user_data = synced_liked_tracks(&folder);

        let new = LibrarySync {
            total: 1,
            newest_added_at: 3,
        };
        assert!(user_data.merge_liked_tracks(vec![track("c", 3)], new, 3));

        assert_eq!(liked_names(&user_data), ["c", "b", "a"]);
        assert_eq!(
            user_data.sync[&FileCacheKey::SavedTracks],
            LibrarySync {
                total: 3,
                newest_added_at: 3
            }
        );
    }

    #[test]
    fn merges_re_liked_tracks() {
        let folder = TempCacheFolder::new("merge-re-likes");
        let mut user_data = synced_liked_tracks(&folder);

        let new = LibrarySync {
            total: 1,
            newest_added_at: 5,
        };
        assert!(user_data.merge_liked_tracks(vec![track("a", 5)], new, 2));

        assert_eq!(liked_names(&user_data), ["a", "b"]);
    }

    #[test]
    fn counts_new_unplayable_tracks_towards_the_total() {
        let folder = TempCacheFolder::new("merge-unplayable");
        let mut user_data = synced_liked_tracks(&folder);

        let new = LibrarySync {
            total: 1,
            newest_added_at: 3,
        };
        assert!(user_data.merge_liked_tracks(vec![], new, 3));

        assert_eq!(user_data.saved_tracks.len(), 2);
        assert_eq!(user_data.sync[&FileCacheKey::SavedTracks].total, 3);
    }

    #[test]
    fn rejects_merge_when_tracks_were_unliked_elsewhere() {
        let folder = TempCacheFolder::new("merge-removed");
        let mut user_data = synced_liked_tracks(&folder);

        let new = LibrarySync {
            total: 1,
            newest_added_at: 3,
        };
        assert!(!user_data.merge_liked_tracks(vec![track("c", 3)], new, 2));

        assert_eq!(liked_names(&user_data), ["b", "a"]);
        assert_eq!(user_data.sync[&FileCacheKey::SavedTracks].total, 2);
    }

    #[test]
    fn local_library_changes_keep_the_sync_total() {
        let folder = TempCacheFolder::new("local-changes");
        let mut user_data = synced_liked_tracks(&folder);

        user_data.record_library_change(FileCacheKey::SavedTracks, true);
        assert_eq!(user_data.sync[&FileCacheKey::SavedTracks].total, 3);
        user_data.record_library_change(FileCacheKey::SavedTracks, false);
        user_data.record_library_change(FileCacheKey::SavedTracks, false);
        assert_eq!(user_data.sync[&FileCacheKey::SavedTracks].total, 1);
    }

    #[test]
    fn merges_newly_saved_albums_first() {
        let folder = TempCacheFolder::new("merge-albums");
        let mut user_data = UserData::new_from_file_caches(&folder.0);
        user_data.saved_albums = vec![album("b", 2), album("a", 1)];
        user_data.sync.insert(
            FileCacheKey::SavedAlbums,
            LibrarySync {
                total: 2,
                newest_added_at: 2,
            },
        );

        assert!(!user_data.merge_saved_albums(vec![album("c", 3)], 2));
        assert!(user_data.merge_saved_albums(vec![album("c", 4), album("a", 3)], 3));

        let names: Vec<&str> = user_data
            .saved_albums
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["c", "a", "b"]);
        assert_eq!(
            user_data.sync[&FileCacheKey::SavedAlbums],
            LibrarySync {
                total: 3,
                newest_added_at: 4
            }
        );
    }

    #[test]
    fn incremental_sync_requires_a_loaded_cache() {
        let folder = TempCacheFolder::new("sync-needs-cache");
        let mut user_data = synced_liked_tracks(&folder);
        user_data
            .store_into_file_cache(FileCacheKey::LibrarySync, &folder.0)
            .unwrap();

        // the sync state is reloaded, but describes a liked tracks cache that no longer exists
        let reloaded = UserData::new_from_file_caches(&folder.0);
        assert_eq!(reloaded.sync.len(), 1);
        assert_eq!(reloaded.incremental_sync(FileCacheKey::SavedTracks), None);

        user_data.loaded.insert(FileCacheKey::SavedTracks);
        assert!(user_data
            .incremental_sync(FileCacheKey::SavedTracks)
            .is_some());
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

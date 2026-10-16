//! Loading and refreshing the user's library and playlists, reusing their file-cached copies
//! where possible.

use anyhow::{Context as _, Result};
use rspotify::{
    clients::OAuthClient,
    http::Query,
    model::{Page, PlaylistId, SavedAlbum, SavedTrack},
    prelude::Id,
};

use super::{request::LibraryRefresh, AppClient};
use crate::{
    config,
    state::{
        load_playlist_from_file_cache, remove_playlist_from_file_cache,
        store_playlist_into_file_cache, write_file_cache, Album, Context, FileCacheKey,
        LibrarySync, Playlist, PlaylistFolderItem, SharedState, Track, TTL_CACHE_DURATION,
        USER_LIKED_TRACKS_URI,
    },
};

fn saved_track_added_at(track: &SavedTrack) -> u64 {
    track.added_at.timestamp() as u64
}

fn saved_album_added_at(album: &SavedAlbum) -> u64 {
    album.added_at.timestamp() as u64
}

/// Whether a collection keeps a [`LibrarySync`] state for incremental refreshes.
fn is_synced_incrementally(key: FileCacheKey) -> bool {
    matches!(key, FileCacheKey::SavedTracks | FileCacheKey::SavedAlbums)
}

impl AppClient {
    /// Download a library collection unless a copy is already loaded (from a file cache or an
    /// earlier download); `RefreshUserLibrary` updates it on demand.
    pub(super) async fn load_user_library(
        &self,
        state: &SharedState,
        key: FileCacheKey,
    ) -> Result<()> {
        if state.data.read().user_data.is_loaded(key) {
            return Ok(());
        }
        self.fetch_user_library(state, key).await
    }

    /// Bring library collections up to date, so changes made outside the app become visible.
    ///
    /// `refresh` decides how much of each collection is downloaded and how a failure is
    /// reported; see [`LibraryRefresh`].
    pub(super) async fn refresh_user_library(
        &self,
        state: &SharedState,
        keys: &[FileCacheKey],
        refresh: LibraryRefresh,
    ) {
        tracing::info!("Refreshing the user's library: {keys:?} ({refresh:?})");
        let full = refresh == LibraryRefresh::Full;
        for &key in keys {
            // one failed collection must not prevent refreshing the others
            if let Err(err) = self.refresh_library_collection(state, key, full).await {
                if refresh == LibraryRefresh::Startup {
                    tracing::info!("Failed to refresh {key:?}, keeping the saved copy: {err:#}");
                } else {
                    tracing::error!("Failed to refresh {key:?}: {err:#}");
                }
            }
        }

        if keys.contains(&FileCacheKey::SavedTracks) {
            let liked_tracks = Self::liked_tracks_context(state);
            state.data.write().caches.context.insert(
                USER_LIKED_TRACKS_URI.to_string(),
                liked_tracks,
                *TTL_CACHE_DURATION,
            );
        }
    }

    /// Update a collection by downloading only what was added since its last sync when possible,
    /// and in full otherwise or when `full` is set.
    async fn refresh_library_collection(
        &self,
        state: &SharedState,
        key: FileCacheKey,
        full: bool,
    ) -> Result<()> {
        let sync = if full {
            None
        } else {
            state.data.read().user_data.incremental_sync(key)
        };
        let merged = match (key, sync) {
            (FileCacheKey::SavedTracks, Some(sync)) => {
                self.merge_new_liked_tracks(state, sync).await?
            }
            (FileCacheKey::SavedAlbums, Some(sync)) => {
                self.merge_new_saved_albums(state, sync).await?
            }
            _ => false,
        };

        if merged {
            Self::store_user_library(state, key)
        } else {
            self.fetch_user_library(state, key).await
        }
    }

    /// Download the tracks liked since `sync` and merge them into the liked tracks, returning
    /// whether that was enough to bring them up to date.
    async fn merge_new_liked_tracks(&self, state: &SharedState, sync: LibrarySync) -> Result<bool> {
        let (saved, total) = self
            .items_added_since("me/tracks", sync.newest_added_at, saved_track_added_at)
            .await?;
        let new = LibrarySync::from_items(&saved, saved_track_added_at);
        let tracks = saved
            .into_iter()
            .filter_map(Track::try_from_saved_track)
            .collect();

        let merged = state
            .data
            .write()
            .user_data
            .merge_liked_tracks(tracks, new, total);
        Self::log_merge(FileCacheKey::SavedTracks, new.total, merged);
        Ok(merged)
    }

    /// Download the albums saved since `sync` and merge them into the saved albums, returning
    /// whether that was enough to bring them up to date.
    async fn merge_new_saved_albums(&self, state: &SharedState, sync: LibrarySync) -> Result<bool> {
        let (saved, total) = self
            .items_added_since("me/albums", sync.newest_added_at, saved_album_added_at)
            .await?;
        let new_count = saved.len();
        let albums = saved.into_iter().map(Album::from).collect();

        let merged = state
            .data
            .write()
            .user_data
            .merge_saved_albums(albums, total);
        Self::log_merge(FileCacheKey::SavedAlbums, new_count, merged);
        Ok(merged)
    }

    fn log_merge(key: FileCacheKey, new_count: usize, merged: bool) {
        if merged {
            tracing::info!("Updated {key:?} with {new_count} new item(s) from its cached copy");
        } else {
            tracing::info!(
                "{key:?} changed by more than new additions since its last sync; downloading it in full"
            );
        }
    }

    /// Download the items of a collection ordered by date added (most recent first) that were
    /// added after `since`, along with the collection's current total.
    async fn items_added_since<T>(
        &self,
        url: &str,
        since: u64,
        added_at: impl Fn(&T) -> u64,
    ) -> Result<(Vec<T>, usize)>
    where
        T: serde::de::DeserializeOwned,
    {
        const PAGE_LIMIT: usize = 50;

        let mut items = Vec::new();
        let mut offset = 0;
        loop {
            let limit = PAGE_LIMIT.to_string();
            let offset_param = offset.to_string();
            let params = Query::from([
                ("market", "from_token"),
                ("limit", limit.as_str()),
                ("offset", offset_param.as_str()),
            ]);
            let page = self.http_get::<Page<T>>(url, &params).await?;
            let total = page.total as usize;
            let page_len = page.items.len();

            for item in page.items {
                if added_at(&item) <= since {
                    return Ok((items, total));
                }
                items.push(item);
            }

            offset += page_len;
            if page_len < PAGE_LIMIT || offset >= total {
                return Ok((items, total));
            }
        }
    }

    /// Download a library collection from Spotify in full, replacing its in-memory and
    /// file-cached copies.
    pub(super) async fn fetch_user_library(
        &self,
        state: &SharedState,
        key: FileCacheKey,
    ) -> Result<()> {
        match key {
            FileCacheKey::Playlists => {
                let playlists = self.current_user_playlists().await?;
                let node = state.data.read().user_data.playlist_folder_node.clone();
                let playlists = if let Some(node) = node.filter(|n| !n.children.is_empty()) {
                    crate::playlist_folders::structurize(playlists, &node.children)
                } else {
                    playlists
                        .into_iter()
                        .map(PlaylistFolderItem::Playlist)
                        .collect()
                };
                state.data.write().user_data.playlists = playlists;
            }
            FileCacheKey::FollowedArtists => {
                let artists = self.current_user_followed_artists().await?;
                state.data.write().user_data.followed_artists = artists;
            }
            FileCacheKey::SavedAlbums => {
                let saved = self.all_paging_items::<SavedAlbum>("me/albums", 0).await?;
                let sync = LibrarySync::from_items(&saved, saved_album_added_at);
                let albums = saved.into_iter().map(Album::from).collect();

                let mut data = state.data.write();
                data.user_data.saved_albums = albums;
                data.user_data.sync.insert(key, sync);
            }
            FileCacheKey::SavedShows => {
                let shows = self.current_user_saved_shows().await?;
                state.data.write().user_data.saved_shows = shows;
            }
            FileCacheKey::SavedTracks => {
                let saved = self.all_paging_items::<SavedTrack>("me/tracks", 0).await?;
                let sync = LibrarySync::from_items(&saved, saved_track_added_at);
                let tracks = saved
                    .into_iter()
                    .filter_map(Track::try_from_saved_track)
                    .map(|t| (t.id.uri(), t))
                    .collect();

                let mut data = state.data.write();
                data.user_data.saved_tracks = tracks;
                data.user_data.sync.insert(key, sync);
            }
            FileCacheKey::PlaylistFolders
            | FileCacheKey::LibrarySync
            | FileCacheKey::PlaylistPlays => {
                anyhow::bail!("{key:?} cannot be downloaded from Spotify")
            }
        }
        state.data.write().user_data.loaded.insert(key);
        Self::store_user_library(state, key)
    }

    /// Persist a library collection after an item was added to or removed from it in the app, so
    /// a later startup that skips downloading it still sees the change.
    ///
    /// The change already stands on Spotify and in memory, so a failed cache write is only
    /// logged; refreshing the collection repairs the saved copy.
    pub(super) fn store_library_change(state: &SharedState, key: FileCacheKey, added: bool) {
        state
            .data
            .write()
            .user_data
            .record_library_change(key, added);
        if let Err(err) = Self::store_user_library(state, key) {
            tracing::error!("Failed to cache the change to {key:?}: {err:#}");
        }
    }

    /// Persist a library collection, along with its sync state if it has one.
    fn store_user_library(state: &SharedState, key: FileCacheKey) -> Result<()> {
        // serialized while holding the lock and written without it, so that a large collection
        // does not block the application's data for the length of a disk write
        let caches = {
            let data = state.data.read();
            let mut caches = vec![(key, data.user_data.to_file_cache(key))];
            if is_synced_incrementally(key) {
                caches.push((
                    FileCacheKey::LibrarySync,
                    data.user_data.to_file_cache(FileCacheKey::LibrarySync),
                ));
            }
            caches
        };

        let cache_folder = &config::get_config().cache_folder;
        for (key, cache) in caches {
            let Some(data) = cache.with_context(|| format!("serialize {key:?}"))? else {
                continue;
            };
            write_file_cache(key, cache_folder, &data)
                .with_context(|| format!("store {key:?} into the cache folder"))?;
        }
        Ok(())
    }

    /// Load a playlist's tracks, reusing the disk copy (unless `refresh` is set) while its
    /// `snapshot_id` matches Spotify's: the one recorded in the playlist list, or, for playlists
    /// outside the user's library, the one returned by a lightweight request.
    pub(super) async fn load_playlist_context(
        &self,
        state: &SharedState,
        id: PlaylistId<'static>,
        refresh: bool,
    ) -> Result<Context> {
        if !refresh {
            if let Some(context) = self.cached_playlist_context(state, &id).await? {
                return Ok(context);
            }
        }

        let context = self.playlist_context(id).await?;
        if let Context::Playlist { playlist, tracks } = &context {
            // the downloaded tracks are still usable without a disk copy
            if let Err(err) = Self::store_playlist(state, playlist, tracks) {
                tracing::error!("Failed to cache playlist {}: {err:#}", playlist.id.id());
            }
        }
        Ok(context)
    }

    async fn cached_playlist_context(
        &self,
        state: &SharedState,
        id: &PlaylistId<'_>,
    ) -> Result<Option<Context>> {
        let Some(cached) = load_playlist_from_file_cache(&config::get_config().cache_folder, id)
        else {
            return Ok(None);
        };

        let listed = state
            .data
            .read()
            .user_data
            .playlist_snapshot(id)
            .map(ToOwned::to_owned);
        let snapshot_id = match listed {
            Some(snapshot_id) => snapshot_id,
            None => self.playlist_snapshot_id(id).await?,
        };

        if cached.playlist.snapshot_id == snapshot_id {
            tracing::info!("Using the cached tracks of playlist {}", id.id());
            Ok(Some(cached.into_context()))
        } else {
            Ok(None)
        }
    }

    async fn playlist_snapshot_id(&self, id: &PlaylistId<'_>) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Snapshot {
            snapshot_id: String,
        }

        let snapshot: Snapshot = self
            .http_get(
                &format!("playlists/{}", id.id()),
                &Query::from([("fields", "snapshot_id")]),
            )
            .await?;
        Ok(snapshot.snapshot_id)
    }

    /// Cache a playlist's tracks on disk, recording its `snapshot_id` in the playlist list.
    fn store_playlist(state: &SharedState, playlist: &Playlist, tracks: &[Track]) -> Result<()> {
        store_playlist_into_file_cache(&config::get_config().cache_folder, playlist, tracks)
            .context("store the playlist into the cache folder")?;

        let list_changed = state
            .data
            .write()
            .user_data
            .update_playlist_snapshot(&playlist.id, &playlist.snapshot_id);
        if list_changed {
            Self::store_user_library(state, FileCacheKey::Playlists)?;
        }
        Ok(())
    }

    /// Discard a playlist's disk copy after an edit, so its next load downloads it again.
    pub(crate) fn discard_cached_playlist(id: &PlaylistId<'_>) -> Result<()> {
        // the edited in-memory copy is not written back instead: it may be sorted for display,
        // which would then be mistaken for Spotify's order
        remove_playlist_from_file_cache(&config::get_config().cache_folder, id)
            .with_context(|| format!("remove the cached playlist {}", id.id()))
    }

    /// Note the playlist that is playing, so the library lists the recent ones first.
    pub(super) fn record_playlist_play(state: &SharedState) {
        let Some(crate::state::ContextId::Playlist(id)) = state.player.read().playing_context_id()
        else {
            return;
        };

        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or_default();
        let worth_storing = state
            .data
            .write()
            .user_data
            .record_playlist_play(id.uri(), now);

        if worth_storing {
            if let Err(err) = Self::store_user_library(state, FileCacheKey::PlaylistPlays) {
                tracing::error!("Failed to cache the playlist play: {err:#}");
            }
        }
    }

    /// Record the playlists behind the user's recently played tracks, so the library has an
    /// order to show before anything has played in the app.
    pub(super) async fn seed_playlist_plays(&self, state: &SharedState) -> Result<()> {
        let history = self.current_user_recently_played(Some(50), None).await?;

        let mut worth_storing = false;
        {
            let mut data = state.data.write();
            for played in history.items {
                let Some(context) = played.context else {
                    continue;
                };
                // a context that is not a playlist, an album for instance, fails to parse here
                let uri = crate::utils::parse_uri(&context.uri);
                let Ok(id) = PlaylistId::from_uri(&uri) else {
                    continue;
                };
                let at = u64::try_from(played.played_at.timestamp()).unwrap_or_default();
                worth_storing |= data.user_data.record_playlist_play(id.uri(), at);
            }
        }

        if worth_storing {
            Self::store_user_library(state, FileCacheKey::PlaylistPlays)?;
        }
        Ok(())
    }

    pub(super) fn liked_tracks_context(state: &SharedState) -> Context {
        Context::Tracks {
            tracks: state.data.read().user_data.liked_tracks(),
            desc: "User's liked tracks".to_string(),
        }
    }
}

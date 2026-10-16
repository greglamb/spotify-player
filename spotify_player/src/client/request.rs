use crate::state::{
    AlbumId, Category, ContextId, FileCacheKey, Item, ItemId, PlayableId, Playback, PlaylistId,
    TrackId,
};

/// How a library refresh was asked for, which decides how much of a collection is downloaded
/// and how a failure is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryRefresh {
    /// The automatic refresh at startup. Nobody asked for it and the saved copy stands in for
    /// what cannot be downloaded, so a failure is only written to the log.
    Startup,
    /// Only the items added since the last sync, for the collections that record one.
    Incremental,
    /// The whole collection, the only way to pick up items removed in another app.
    Full,
}

#[derive(Clone, Debug)]
/// A request that modifies the player's playback
pub enum PlayerRequest {
    NextTrack,
    PreviousTrack,
    Resume,
    Pause,
    ResumePause,
    SeekTrack(chrono::Duration),
    Repeat,
    Shuffle,
    Volume(u8),
    ToggleMute,
    TransferPlayback(String, bool),
    StartPlayback(Playback, Option<bool>),
}

#[derive(Clone, Debug)]
/// A request to the client
pub enum ClientRequest {
    GetCurrentUser,
    GetDevices,
    GetBrowseCategories,
    GetBrowseCategoryPlaylists(Category),
    GetUserPlaylists,
    GetUserSavedAlbums,
    GetUserSavedShows,
    GetUserFollowedArtists,
    /// Note the playlists behind the user's recently played tracks
    GetRecentPlaylistPlays,
    /// Bring the given library collections up to date, even if a copy is already loaded
    RefreshUserLibrary {
        keys: Vec<FileCacheKey>,
        refresh: LibraryRefresh,
    },
    GetContext(ContextId),
    /// Download a context again, even if it is cached
    RefreshContext(ContextId),
    GetCurrentPlayback,
    Search(String),
    AddPlayableToQueue(PlayableId<'static>),
    AddAlbumToQueue(AlbumId<'static>),
    AddPlayableToPlaylist(PlaylistId<'static>, PlayableId<'static>),
    DeleteTrackFromPlaylist(PlaylistId<'static>, TrackId<'static>),
    ReorderPlaylistItems {
        playlist_id: PlaylistId<'static>,
        insert_index: usize,
        range_start: usize,
        range_length: Option<usize>,
        snapshot_id: Option<String>,
    },
    AddToLibrary(Item),
    DeleteFromLibrary(ItemId),
    Player(PlayerRequest),
    GetCurrentUserQueue,
    GetLyrics {
        track_id: TrackId<'static>,
    },
    #[cfg(feature = "streaming")]
    RestartIntegratedClient,
    CreatePlaylist {
        playlist_name: String,
        public: bool,
        collab: bool,
        desc: String,
    },
}

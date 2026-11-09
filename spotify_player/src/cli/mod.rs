mod client;
mod commands;
mod handlers;

use crate::config;
use rspotify::model::{AlbumId, ArtistId, Id, PlaylistId, TrackId};
use serde::{Deserialize, Serialize};

const MAX_REQUEST_SIZE: usize = 4096;

pub use client::{handle_playback_request, start_socket};
pub use handlers::handle_cli_subcommand;

#[derive(Debug, Serialize, Deserialize, clap::ValueEnum, Clone)]
pub enum Key {
    Playback,
    Devices,
    UserPlaylists,
    UserLikedTracks,
    UserSavedAlbums,
    UserFollowedArtists,
    UserTopTracks,
    Queue,
}

#[derive(Debug, Serialize, Deserialize, clap::ValueEnum, Clone)]
pub enum ContextType {
    Playlist,
    Album,
    Artist,
}

#[derive(Debug, Serialize, Deserialize, clap::ValueEnum, Clone)]
pub enum ItemType {
    Playlist,
    Album,
    Artist,
    Track,
}

/// Spotify item's ID
enum ItemId {
    Playlist(PlaylistId<'static>),
    Artist(ArtistId<'static>),
    Album(AlbumId<'static>),
    Track(TrackId<'static>),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum GetRequest {
    Key(Key),
    Item(ItemType, IdOrName),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IdOrName {
    Id(String),
    Name(String),
}

/// What the `--play` option names: a Spotify URI or link, or else a playlist by name.
///
/// A URI or a link names the kind of context itself. Anything else is taken as a playlist
/// name, the form people know by heart.
pub fn parse_play_target(value: &str) -> (ContextType, IdOrName) {
    let value = value.trim();
    let uri = crate::utils::parse_uri(value);

    for (kind, context_type) in [
        ("playlist", ContextType::Playlist),
        ("album", ContextType::Album),
        ("artist", ContextType::Artist),
    ] {
        for prefix in [
            format!("spotify:{kind}:"),
            format!("https://open.spotify.com/{kind}/"),
        ] {
            let Some(rest) = uri.strip_prefix(&prefix) else {
                continue;
            };
            // a link carries a query string, and sometimes a trailing path
            let id = rest.split(['?', '/']).next().unwrap_or(rest);
            return (context_type, IdOrName::Id(id.to_string()));
        }
    }

    (ContextType::Playlist, IdOrName::Name(value.to_string()))
}

#[derive(Debug, Serialize, Deserialize, clap::ValueEnum, Clone, Copy)]
pub enum EditAction {
    Add,
    Delete,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PlaylistCommand {
    New {
        name: String,
        public: bool,
        collab: bool,
        description: String,
    },
    Delete {
        id: PlaylistId<'static>,
    },
    List,
    Import {
        from: PlaylistId<'static>,
        to: PlaylistId<'static>,
        delete: bool,
    },
    Fork {
        id: PlaylistId<'static>,
    },
    Sync {
        id: Option<PlaylistId<'static>>,
        delete: bool,
    },
    Edit {
        action: EditAction,
        playlist_id: PlaylistId<'static>,
        track_id: Option<TrackId<'static>>,
        album_id: Option<AlbumId<'static>>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    StartContext {
        context_type: ContextType,
        id_or_name: IdOrName,
        shuffle: bool,
    },
    StartTrack(IdOrName),
    StartLikedTracks {
        limit: usize,
        random: bool,
    },
    StartRadio(ItemType, IdOrName),
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    Volume {
        percent: i8,
        is_offset: bool,
    },
    Seek(i64),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Get(GetRequest),
    Playback(Command),
    Connect(IdOrName),
    Like { unlike: bool },
    Playlist(PlaylistCommand),
    Search { query: String },
    Lyrics { id_or_name: Option<IdOrName> },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok(Vec<u8>),
    Err(Vec<u8>),
}

impl From<ContextType> for ItemType {
    fn from(value: ContextType) -> Self {
        match value {
            ContextType::Playlist => Self::Playlist,
            ContextType::Album => Self::Album,
            ContextType::Artist => Self::Artist,
        }
    }
}

impl ItemId {
    pub fn uri(&self) -> String {
        match self {
            ItemId::Playlist(id) => id.uri(),
            ItemId::Artist(id) => id.uri(),
            ItemId::Album(id) => id.uri(),
            ItemId::Track(id) => id.uri(),
        }
    }
}

/// The application's version.
///
/// Release builds stamp the git tag into `SPOTIFY_PLAYER_VERSION` (see the CD workflow)
/// so a fork's releases are identifiable; other builds report the crate version.
pub const VERSION: &str = match option_env!("SPOTIFY_PLAYER_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub fn init_cli() -> anyhow::Result<clap::Command> {
    let default_cache_folder = config::get_cache_folder_path()?;
    let default_config_folder = config::get_config_folder_path()?;

    let cmd = clap::Command::new(env!("CARGO_PKG_NAME"))
        .version(VERSION)
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .subcommand(commands::init_get_subcommand())
        .subcommand(commands::init_playback_subcommand())
        .subcommand(commands::init_connect_subcommand())
        .subcommand(commands::init_like_command())
        .subcommand(commands::init_authenticate_command())
        .subcommand(commands::init_playlist_subcommand())
        .subcommand(commands::init_generate_command())
        .subcommand(commands::init_search_command())
        .subcommand(commands::init_print_features_command())
        .subcommand(commands::init_lyrics_command())
        .arg(
            clap::Arg::new("play")
                .long("play")
                .value_name("CONTEXT")
                .help(
                    "Start playing a playlist by name, or the playlist, album or artist a \
                     Spotify URI or link names, once the application has a device",
                ),
        )
        .arg(
            clap::Arg::new("theme")
                .short('t')
                .long("theme")
                .value_name("THEME")
                .help("Application theme"),
        )
        .arg(
            clap::Arg::new("config-folder")
                .short('c')
                .long("config-folder")
                .value_name("FOLDER")
                .default_value(default_config_folder.into_os_string())
                .help("Path to the application's config folder"),
        )
        .arg(
            clap::Arg::new("cache-folder")
                .short('C')
                .long("cache-folder")
                .value_name("FOLDER")
                .default_value(default_cache_folder.into_os_string())
                .help("Path to the application's cache folder"),
        )
        .arg(
            clap::Arg::new("config-override")
                .short('o')
                .long("config-override")
                .value_name("KEY=VALUE")
                .action(clap::ArgAction::Append)
                .help("Override a config option (e.g. -o device.volume=80 -o theme=dracula)"),
        );

    #[cfg(feature = "daemon")]
    let cmd = cmd.arg(
        clap::Arg::new("daemon")
            .short('d')
            .long("daemon")
            .action(clap::ArgAction::SetTrue)
            .help("Running the application as a daemon"),
    );

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::{parse_play_target, ContextType, IdOrName};

    #[test]
    fn what_to_play_comes_from_a_uri_a_link_or_a_name() {
        let id = "4MOWlyL7zyp03rsJHCmsGu";

        assert!(matches!(
            parse_play_target(&format!("spotify:playlist:{id}")),
            (ContextType::Playlist, IdOrName::Id(parsed)) if parsed == id
        ));
        assert!(matches!(
            parse_play_target(&format!("https://open.spotify.com/album/{id}?si=abc")),
            (ContextType::Album, IdOrName::Id(parsed)) if parsed == id
        ));
        // the user form of a playlist URI, which `parse_uri` normalizes
        assert!(matches!(
            parse_play_target(&format!("spotify:user:someone:playlist:{id}")),
            (ContextType::Playlist, IdOrName::Id(parsed)) if parsed == id
        ));
        assert!(matches!(
            parse_play_target("  Code Caper  "),
            (ContextType::Playlist, IdOrName::Name(name)) if name == "Code Caper"
        ));
    }
}

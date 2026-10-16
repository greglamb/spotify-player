use crate::{
    command::Command,
    config::{self, KeymapConfig},
    state::{
        Album, Artist, ArtistFocusState, BrowsePageUIState, Context, ContextPageUIState,
        DataReadGuard, Id, LibraryFocusState, MutableWindowState, PageState, PageType,
        PlaybackMetadata, PlaylistCreateCurrentField, PlaylistFolderItem, PlaylistPopupAction,
        PopupState, SearchFocusState, SharedState, Track, UIStateGuard,
    },
};
use anyhow::{Context as AnyhowContext, Result};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Gauge, LineGauge, List, ListItem, ListState,
        Paragraph, Row, Table, TableState, Wrap,
    },
    Frame,
};

#[cfg(feature = "image")]
use crate::state::ImageRenderInfo;

type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

#[cfg(feature = "image")]
pub mod cover_image;
mod page;
mod playback;
mod popup;
pub mod single_line_input;
#[cfg(feature = "streaming")]
pub mod streaming;
pub mod utils;

/// Run the application UI
pub fn run(state: &SharedState, mut terminal: Terminal) -> Result<()> {
    let ui_refresh_duration = std::time::Duration::from_millis(
        config::get_config().app_config.app_refresh_duration_in_ms,
    );
    let mut last_terminal_size = None;

    loop {
        {
            let mut ui = state.ui.lock();
            if !ui.is_running {
                clean_up(terminal).context("clean up UI resources")?;
                std::process::exit(0);
            }

            let terminal_size = terminal.size()?;
            if Some(terminal_size) != last_terminal_size {
                last_terminal_size = Some(terminal_size);
                #[cfg(feature = "image")]
                {
                    // redraw the cover image when the terminal's size changes
                    ui.last_cover_image_render_info = ImageRenderInfo::default();
                }
            }

            if let Err(err) = terminal.draw(|frame| {
                // set the background and foreground colors for the application
                let rect = frame.area();
                let block = Block::default().style(ui.theme.app());
                frame.render_widget(block, rect);

                render_application(frame, state, &mut ui, rect);
            }) {
                tracing::error!("Failed to render the application: {err:#}");
            }
        }

        std::thread::sleep(ui_refresh_duration);
    }
}

pub fn init_terminal() -> Result<Terminal> {
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

#[cfg(feature = "image")]
pub fn init_image_picker(state: &SharedState) -> Result<()> {
    let mut ui = state.ui.lock();
    crossterm::terminal::enable_raw_mode()?;
    ui.picker = match ratatui_image::picker::Picker::from_query_stdio() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!("Failed to initialize query_stdio picker, error: {err:#}");
            ratatui_image::picker::Picker::halfblocks()
        }
    };
    crossterm::terminal::disable_raw_mode()?;

    // ratatui_image might detect the wrong protocol for iTerm2, so override it to the native iTerm2 protocol if detected
    // https://github.com/ratatui/ratatui-image/issues/158
    if is_iterm2() && ui.picker.protocol_type() != ratatui_image::picker::ProtocolType::Iterm2 {
        ui.picker
            .set_protocol_type(ratatui_image::picker::ProtocolType::Iterm2);
        tracing::info!("Detected iTerm2; overriding image protocol to native iTerm2");
    }
    tracing::info!("Image protocol: {:?}", ui.picker.protocol_type());
    Ok(())
}

/// Whether the application is running inside iTerm2.
///
/// iTerm2 sets `TERM_PROGRAM=iTerm.app` locally and forwards `LC_TERMINAL=iTerm2`
/// over SSH, so checking both covers the common cases.
#[cfg(feature = "image")]
fn is_iterm2() -> bool {
    std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app")
        || std::env::var("LC_TERMINAL").is_ok_and(|v| v.eq_ignore_ascii_case("iTerm2"))
}

/// Clean up UI resources before quitting the application
fn clean_up(mut terminal: Terminal) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Area below which the shortcut hints cost more than the row they take.
const MIN_SHORTCUT_HINTS_HEIGHT: u16 = 8;

/// Render the application
fn render_application(frame: &mut Frame, state: &SharedState, ui: &mut UIStateGuard, rect: Rect) {
    // rendering order: playback window -> shortcut help popup -> other popups -> main layout -> alert popup

    // the hints keep the last row of the screen, whichever side the playback window is on
    let rect = render_shortcut_hints(frame, ui, rect);

    // render playback window before other popups and windows to ensure nothing is rendered on top
    // of the playback window, which is to avoid "duplicated images" issue
    // See: https://github.com/aome510/spotify-player/issues/498
    let rect = playback::render_playback_window(frame, state, ui, rect);
    let content_rect = rect;

    let rect = popup::render_shortcut_help_popup(frame, ui, rect);

    let (rect, is_active) = popup::render_popup(frame, state, ui, rect);

    render_main_layout(is_active, frame, state, ui, rect);

    // the command palette floats above the page it runs a command on
    popup::render_command_palette_popup(frame, ui, content_rect);

    // render the alert popup last so it appears on top of the main layout,
    // but within the area left by the playback window to avoid cover image artifacts
    popup::render_alert_popup(frame, state, ui, content_rect);
}

/// Render the keys that work right now along the bottom, returning the area above them.
fn render_shortcut_hints(frame: &mut Frame, ui: &UIStateGuard, rect: Rect) -> Rect {
    let configs = config::get_config();
    if !configs.app_config.enable_shortcut_hints || rect.height < MIN_SHORTCUT_HINTS_HEIGHT {
        return rect;
    }

    let chunks = Layout::vertical([Constraint::Fill(0), Constraint::Length(1)]).split(rect);
    let hints = shortcut_hints(
        &configs.keymap_config,
        ui.popup.as_ref(),
        ui.current_page().page_type(),
        usize::from(chunks[1].width.saturating_sub(1)),
    );

    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" {hints}"),
            Style::default().add_modifier(Modifier::DIM),
        )),
        chunks[1],
    );
    chunks[0]
}

/// The commands worth hinting at, for the popup in the way or else for the current page.
///
/// The command palette and the help page come first, so that a narrow screen keeps the two
/// that lead everywhere else.
fn hint_commands(popup: Option<&PopupState>, page: PageType) -> Vec<(Command, &'static str)> {
    let mut hints = vec![
        (Command::OpenCommandPalette, "commands"),
        (Command::OpenCommandHelp, "help"),
    ];

    if let Some(popup) = popup {
        match popup {
            PopupState::CommandPalette { .. } => hints.push((Command::ChooseSelected, "run")),
            // the search popup filters the page underneath, which stays in charge
            PopupState::Search { .. } => {}
            _ => hints.push((Command::ChooseSelected, "choose")),
        }
        hints.push((Command::ClosePopup, "close"));
        return hints;
    }

    match page {
        PageType::Library => {
            hints.extend([
                (Command::ChooseSelected, "open"),
                (Command::Search, "search"),
                (Command::RefreshLibrary, "refresh"),
            ]);
        }
        PageType::Context => {
            hints.extend([
                (Command::ChooseSelected, "play"),
                (Command::ShowActionsOnSelectedItem, "actions"),
                (Command::RefreshCurrentContext, "refresh"),
            ]);
        }
        PageType::Browse => {
            hints.extend([
                (Command::ChooseSelected, "open"),
                (Command::PreviousPage, "back"),
            ]);
        }
        PageType::Search => {
            hints.extend([
                (Command::ChooseSelected, "select"),
                (Command::PreviousPage, "back"),
            ]);
        }
        PageType::CommandHelp => {
            hints.extend([(Command::Search, "search"), (Command::PreviousPage, "back")]);
        }
        PageType::Queue | PageType::Lyrics | PageType::Logs => {
            hints.push((Command::PreviousPage, "back"));
        }
    }
    hints.push((Command::Quit, "quit"));
    hints
}

/// A row of `[key] label` hints that fits `width` columns, leaving out what does not.
fn shortcut_hints(
    keymap: &KeymapConfig,
    popup: Option<&PopupState>,
    page: PageType,
    width: usize,
) -> String {
    let mut hints = String::new();
    for (command, label) in hint_commands(popup, page) {
        let Some(keys) = keymap.first_key(command) else {
            continue;
        };
        let separator = if hints.is_empty() { "" } else { "  " };
        let hint = format!("{separator}[{keys}] {label}");
        if hints.chars().count() + hint.chars().count() > width {
            break;
        }
        hints.push_str(&hint);
    }
    hints
}

/// Render the application's main layout
fn render_main_layout(
    is_active: bool,
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    rect: Rect,
) {
    let page_type = ui.current_page().page_type();
    match page_type {
        PageType::Library => page::render_library_page(is_active, frame, state, ui, rect),
        PageType::Search => page::render_search_page(is_active, frame, state, ui, rect),
        PageType::Context => page::render_context_page(is_active, frame, state, ui, rect),
        PageType::Browse => page::render_browse_page(is_active, frame, state, ui, rect),
        PageType::Lyrics => page::render_lyrics_page(is_active, frame, state, ui, rect),
        PageType::Queue => page::render_queue_page(frame, state, ui, rect),
        PageType::CommandHelp => page::render_commands_help_page(frame, ui, rect),
        PageType::Logs => page::render_logs_page(frame, state, ui, rect),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Orientation {
    Vertical,
    #[default]
    Horizontal,
}

impl Orientation {
    /// Construct screen orientation based on the terminal's size
    pub fn from_size(columns: u16, rows: u16) -> Self {
        let ratio = f64::from(columns) / f64::from(rows);

        // a larger ratio has to be used since terminal cells aren't square
        if ratio > 2.3 {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }

    pub fn layout<I>(self, constraints: I) -> Layout
    where
        I: IntoIterator,
        I::Item: Into<Constraint>,
    {
        match self {
            Self::Vertical => Layout::vertical(constraints),
            Self::Horizontal => Layout::horizontal(constraints),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{shortcut_hints, Command, KeymapConfig, PageType, PopupState};

    #[test]
    fn the_shortcut_hints_lead_with_the_palette_and_the_help_page() {
        let hints = shortcut_hints(&KeymapConfig::default(), None, PageType::Library, 200);

        assert!(hints.starts_with("[f1] commands  [?] help"), "{hints}");
        assert!(hints.ends_with("[q] quit"), "{hints}");
    }

    #[test]
    fn the_shortcut_hints_follow_the_page_and_the_popup() {
        let keymap = KeymapConfig::default();

        let library = shortcut_hints(&keymap, None, PageType::Library, 200);
        assert!(library.contains("[U U] refresh"), "{library}");

        let popup = PopupState::Search {
            query: String::new(),
        };
        let searching = shortcut_hints(&keymap, Some(&popup), PageType::Library, 200);
        assert!(searching.contains("[esc] close"), "{searching}");
        assert!(!searching.contains("refresh"), "{searching}");
    }

    #[test]
    fn the_shortcut_hints_leave_out_what_does_not_fit() {
        let keymap = KeymapConfig::default();
        let full = shortcut_hints(&keymap, None, PageType::Library, 200);

        let narrow = shortcut_hints(&keymap, None, PageType::Library, 23);

        assert_eq!(narrow, "[f1] commands  [?] help");
        assert!(full.len() > narrow.len(), "{full}");
        assert!(shortcut_hints(&keymap, None, PageType::Library, 4).is_empty());
    }

    #[test]
    fn an_unbound_command_is_left_out_of_the_shortcut_hints() {
        let mut keymap = KeymapConfig::default();
        keymap
            .keymaps
            .retain(|keymap| keymap.command != Command::OpenCommandPalette);

        let hints = shortcut_hints(&keymap, None, PageType::Library, 200);

        assert!(!hints.contains("commands"), "{hints}");
        assert!(hints.starts_with("[?] help"), "{hints}");
    }
}

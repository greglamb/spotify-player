use crate::{command::Command, utils::filtered_items_from_query};

use super::{
    config, utils, utils::construct_and_render_block, Block, Borders, Cell, Clear, Constraint,
    Frame, Layout, Line, Paragraph, PlaylistCreateCurrentField, PlaylistPopupAction, PopupState,
    Rect, Row, SharedState, Span, Style, Table, UIStateGuard, Wrap,
};

const SHORTCUT_TABLE_N_COLUMNS: usize = 3;
const SHORTCUT_TABLE_CONSTRAINS: [Constraint; SHORTCUT_TABLE_N_COLUMNS] =
    [Constraint::Ratio(1, 3); 3];

const MIN_ALERT_POPUP_WIDTH: u16 = 24;
const MIN_ALERT_POPUP_HEIGHT: u16 = 5;
const MAX_ALERT_POPUP_WIDTH: u16 = 80;

/// Render a popup (if any) to handle a command or show additional information
/// depending on the current popup state.
///
/// The function returns a rectangle area to render the main layout and
/// a boolean value determining whether the focus should be placed in the main layout.
pub fn render_popup(
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    rect: Rect,
) -> (Rect, bool) {
    match ui.popup {
        None => (rect, true),
        Some(ref popup) => match popup {
            PopupState::PlaylistCreate {
                name,
                desc,
                current_field,
            } => {
                let chunks =
                    Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).split(rect);

                let popup_chunks =
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(chunks[1]);

                let name_input = construct_and_render_block(
                    "Enter Name for New Playlist:",
                    &ui.theme,
                    Borders::ALL,
                    frame,
                    popup_chunks[0],
                );

                let desc_input = construct_and_render_block(
                    "Enter Description for New Playlist:",
                    &ui.theme,
                    Borders::ALL,
                    frame,
                    popup_chunks[1],
                );

                frame.render_widget(
                    name.widget(PlaylistCreateCurrentField::Name == *current_field),
                    name_input,
                );
                frame.render_widget(
                    desc.widget(PlaylistCreateCurrentField::Desc == *current_field),
                    desc_input,
                );
                (chunks[0], true)
            }
            PopupState::Search { query } => {
                let chunks =
                    Layout::vertical([Constraint::Fill(0), Constraint::Length(3)]).split(rect);

                let rect =
                    construct_and_render_block("Search", &ui.theme, Borders::ALL, frame, chunks[1]);

                frame.render_widget(Paragraph::new(format!("/{query}")), rect);
                (chunks[0], true)
            }
            PopupState::ActionList(item, _) => {
                let rect = render_list_popup(
                    frame,
                    rect,
                    &format!("Actions on {}", item.name()),
                    item.actions_desc()
                        .into_iter()
                        .enumerate()
                        .map(|(id, d)| (format!("[{id}] {d}"), false))
                        .collect(),
                    item.n_actions() as u16 + 2, // 2 for top/bot paddings
                    ui,
                );
                (rect, false)
            }
            PopupState::DeviceList { .. } => {
                let player = state.player.read();

                let current_device_id = match player.playback {
                    Some(ref playback) => playback.device.id.as_deref().unwrap_or_default(),
                    None => "",
                };
                let items = player
                    .devices
                    .iter()
                    .map(|d| {
                        // Mark the integrated device of this running instance to distinguish it
                        // from other `spotify-player` instances running elsewhere.
                        let name = if d.is_integrated {
                            format!("{} (integrated)", d.name)
                        } else {
                            d.name.clone()
                        };
                        (format!("{name} | {}", d.id), current_device_id == d.id)
                    })
                    .collect();

                let rect = render_list_popup(frame, rect, "Devices", items, 5, ui);
                (rect, false)
            }
            PopupState::ThemeList(themes, ..) => {
                let items = themes.iter().map(|t| (t.name.clone(), false)).collect();

                let rect = render_list_popup(frame, rect, "Themes", items, 7, ui);
                (rect, false)
            }
            PopupState::UserPlaylistList(action, _) => {
                let data = state.data.read();
                let (items, search_query) = match action {
                    PlaylistPopupAction::Browse {
                        folder_id,
                        search_query,
                    } => (
                        data.user_data.folder_playlists_items(*folder_id),
                        search_query,
                    ),
                    PlaylistPopupAction::AddTrack {
                        folder_id,
                        search_query,
                        ..
                    }
                    | PlaylistPopupAction::AddEpisode {
                        folder_id,
                        search_query,
                        ..
                    } => (
                        data.user_data.modifiable_playlist_items(Some(*folder_id)),
                        search_query,
                    ),
                };

                // Filter items based on search query if present
                let filtered_items = filtered_items_from_query(search_query, &items);

                let display_items = filtered_items
                    .iter()
                    .map(|p| (p.to_string(), false))
                    .collect();

                let chunks = Layout::vertical([
                    Constraint::Length(3),
                    Constraint::Fill(0),
                    Constraint::Length(10),
                ])
                .split(rect);

                // Render search input
                let search_rect = construct_and_render_block(
                    "Search Playlists (type to search, backspace on empty to close)",
                    &ui.theme,
                    Borders::ALL,
                    frame,
                    chunks[0],
                );
                frame.render_widget(Paragraph::new(format!("🔍 {search_query}")), search_rect);

                // Render filtered playlist list
                let rect =
                    render_list_popup(frame, chunks[2], "User Playlists", display_items, 10, ui);
                (rect, false)
            }
            PopupState::UserFollowedArtistList { .. } => {
                let items = state
                    .data
                    .read()
                    .user_data
                    .followed_artists
                    .iter()
                    .map(|a| (a.to_string(), false))
                    .collect();

                let rect = render_list_popup(frame, rect, "User Followed Artists", items, 7, ui);
                (rect, false)
            }
            PopupState::UserSavedAlbumList { .. } => {
                let items = state
                    .data
                    .read()
                    .user_data
                    .saved_albums
                    .iter()
                    .map(|a| (a.to_string(), false))
                    .collect();

                let rect = render_list_popup(frame, rect, "User Saved Albums", items, 7, ui);
                (rect, false)
            }
            PopupState::ArtistList(_, artists, ..) => {
                let items = artists.iter().map(|a| (a.to_string(), false)).collect();

                let rect = render_list_popup(frame, rect, "Artists", items, 5, ui);
                (rect, false)
            }
            PopupState::ConfirmAction { message, .. } => {
                let chunks =
                    Layout::vertical([Constraint::Fill(0), Constraint::Length(3)]).split(rect);

                let confirm_rect = construct_and_render_block(
                    "Confirm",
                    &ui.theme,
                    Borders::ALL,
                    frame,
                    chunks[1],
                );

                frame.render_widget(Paragraph::new(format!("{message} (y/n)")), confirm_rect);

                (chunks[0], true)
            }
        },
    }
}

/// Render the alert popup showing the oldest pending alert, if any.
///
/// Alerts are raised from log events at any time, so the popup is rendered last
/// (on top of the main layout) and handles its own keys in `event::handle_key_event`.
pub fn render_alert_popup(frame: &mut Frame, state: &SharedState, ui: &UIStateGuard, rect: Rect) {
    // copy the alert out before rendering: logging while holding the lock deadlocks the alert layer
    let (alert, n_alerts) = {
        let alerts = state.alerts.lock();
        match alerts.current() {
            Some(alert) => (alert.clone(), alerts.len()),
            None => return,
        }
    };

    if rect.width < MIN_ALERT_POPUP_WIDTH || rect.height < MIN_ALERT_POPUP_HEIGHT {
        return;
    }

    let level_style = match alert.level {
        config::AlertLevel::Error => Style::default().fg(ratatui::style::Color::Red),
        config::AlertLevel::Warn => Style::default().fg(ratatui::style::Color::Yellow),
    };
    let dim_style = Style::default().add_modifier(ratatui::style::Modifier::DIM);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", alert.level), level_style),
            Span::styled(
                format!("{} {}", alert.last_seen.format("%H:%M:%S"), alert.target),
                dim_style,
            ),
        ]),
        Line::raw(alert.message.clone()),
    ];
    lines.extend(
        alert
            .fields
            .iter()
            .map(|(name, value)| Line::raw(format!("  {name}: {value}"))),
    );
    if alert.count > 1 {
        lines.push(Line::styled(
            format!(
                "  (seen {} times since {})",
                alert.count,
                alert.first_seen.format("%H:%M:%S")
            ),
            dim_style,
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "[esc] dismiss  [a] dismiss all  [y] copy  [Y] copy all  [g o] logs",
        dim_style,
    ));

    let area = alert_popup_area(rect, &lines);

    let title = if n_alerts > 1 {
        format!("Alert (1/{n_alerts})")
    } else {
        "Alert".to_string()
    };

    // `Clear` resets the cells to the terminal's default style, so re-apply the app's style
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(ui.theme.app()), area);

    let inner_rect = construct_and_render_block(&title, &ui.theme, Borders::ALL, frame, area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner_rect);
}

/// The centered area of `rect` that fits the alert popup's `lines`,
/// including the surrounding block's borders.
fn alert_popup_area(rect: Rect, lines: &[Line]) -> Rect {
    let width = rect
        .width
        .saturating_sub(4)
        .clamp(MIN_ALERT_POPUP_WIDTH, MAX_ALERT_POPUP_WIDTH);
    // the block's left/right borders reduce the width available for wrapping
    let inner_width = std::cmp::max(width.saturating_sub(2), 1) as usize;
    let n_rows: usize = lines
        .iter()
        .map(|line| std::cmp::max(line.width(), 1).div_ceil(inner_width))
        .sum();
    let height = std::cmp::min(
        u16::try_from(n_rows).unwrap_or(u16::MAX).saturating_add(2),
        rect.height,
    );

    Rect {
        x: rect.x + (rect.width - width) / 2,
        y: rect.y + (rect.height - height) / 2,
        width,
        height,
    }
}

/// A helper function to render a list popup
fn render_list_popup(
    frame: &mut Frame,
    rect: Rect,
    title: &str,
    items: Vec<(String, bool)>,
    length: u16,
    ui: &mut UIStateGuard,
) -> Rect {
    let chunks = Layout::vertical([Constraint::Fill(0), Constraint::Length(length)]).split(rect);

    let rect = construct_and_render_block(title, &ui.theme, Borders::ALL, frame, chunks[1]);
    let selected_index = ui.popup.as_ref().and_then(PopupState::list_selected);
    let (list, len) = utils::construct_list_widget(&ui.theme, items, true, selected_index);

    utils::render_list_window(
        frame,
        list,
        rect,
        len,
        ui.popup.as_mut().unwrap().list_state_mut().unwrap(),
    );

    chunks[0]
}

/// Render a shortcut help popup to show the available shortcuts based on user's inputs
pub fn render_shortcut_help_popup(frame: &mut Frame, ui: &mut UIStateGuard, rect: Rect) -> Rect {
    let input = &ui.input_key_sequence;

    // get the matches (keymaps) from the current key sequence input,
    // if there is at lease one match, render the shortcut help popup
    let matches = {
        if input.keys.is_empty() {
            vec![]
        } else {
            config::get_config()
                .keymap_config
                .find_matched_prefix_keymaps(input)
                .into_iter()
                .map(|keymap| {
                    let mut keymap = keymap.clone();
                    keymap.key_sequence.keys.drain(0..input.keys.len());
                    keymap
                })
                .filter(|keymap| {
                    !keymap.key_sequence.keys.is_empty() && keymap.command != Command::None
                })
                .collect::<Vec<_>>()
        }
    };

    if matches.is_empty() {
        rect
    } else {
        let chunks = Layout::vertical([Constraint::Fill(0), Constraint::Length(7)]).split(rect);

        let rect =
            construct_and_render_block("Shortcuts", &ui.theme, Borders::ALL, frame, chunks[1]);

        let help_table = Table::new(
            matches
                .into_iter()
                .map(|km| format!("{}: {:?}", km.key_sequence, km.command))
                .collect::<Vec<_>>()
                .chunks(SHORTCUT_TABLE_N_COLUMNS)
                .map(|c| Row::new(c.iter().map(|i| Cell::from(i.to_owned()))))
                .collect::<Vec<_>>(),
            SHORTCUT_TABLE_CONSTRAINS,
        );

        frame.render_widget(help_table, rect);
        chunks[0]
    }
}

#[cfg(test)]
mod tests {
    use super::{alert_popup_area, Line, Rect, MAX_ALERT_POPUP_WIDTH};

    #[test]
    fn centers_the_alert_popup() {
        let rect = Rect::new(0, 0, 100, 40);
        let lines = vec![Line::raw("rate limited"), Line::raw("retry_after: 30")];

        let area = alert_popup_area(rect, &lines);

        assert_eq!(area.width, MAX_ALERT_POPUP_WIDTH);
        assert_eq!(area.height, 4); // 2 lines + top/bottom borders
        assert_eq!(area.x, (rect.width - area.width) / 2);
        assert_eq!(area.y, (rect.height - area.height) / 2);
    }

    #[test]
    fn accounts_for_wrapped_lines() {
        let rect = Rect::new(0, 0, 100, 40);
        // 3 rows once wrapped at the popup's inner width
        let lines = vec![Line::raw(
            "x".repeat(usize::from(MAX_ALERT_POPUP_WIDTH - 2) * 3),
        )];

        assert_eq!(alert_popup_area(rect, &lines).height, 5);
    }

    #[test]
    fn fits_inside_a_small_area() {
        let rect = Rect::new(3, 2, 30, 6);
        let lines = (0..20).map(|_| Line::raw("failure")).collect::<Vec<_>>();

        let area = alert_popup_area(rect, &lines);

        assert!(area.width <= rect.width);
        assert_eq!(area.height, rect.height);
        assert!(area.x >= rect.x && area.x + area.width <= rect.x + rect.width);
        assert!(area.y >= rect.y && area.y + area.height <= rect.y + rect.height);
    }
}

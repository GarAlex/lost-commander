//! The classic Norton Commander palette: blue panels, cyan frames, a bright
//! cursor bar and yellow for marked files.

use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Blue;
pub const FILE_FG: Color = Color::Cyan;
pub const DIR_FG: Color = Color::White;
pub const MARK_FG: Color = Color::Yellow;
pub const BORDER_FG: Color = Color::Cyan;
pub const BORDER_ACTIVE_FG: Color = Color::White;
pub const TITLE_FG: Color = Color::Yellow;
pub const CURSOR_BG: Color = Color::Cyan;
pub const CURSOR_FG: Color = Color::Black;
pub const KEYBAR_BG: Color = Color::Cyan;
pub const KEYBAR_FG: Color = Color::Black;
pub const KEYNUM_FG: Color = Color::White;
pub const DIALOG_BG: Color = Color::Blue;
pub const ERROR_FG: Color = Color::LightRed;
/// A line the right-hand file has and the left one does not.
///
/// Red for gone and green for arrived is what every diff has used since
/// diff(1) grew colours, and the two are the one pair of colours a reader
/// already knows the meaning of without a legend.
pub const ADDED_FG: Color = Color::LightGreen;

pub fn base() -> Style {
    Style::default().bg(BG).fg(FILE_FG)
}

pub fn entry_style(is_dir: bool, marked: bool, under_cursor: bool, panel_active: bool) -> Style {
    let fg = if marked {
        MARK_FG
    } else if is_dir {
        DIR_FG
    } else {
        FILE_FG
    };

    // Only the active panel shows a filled cursor bar, so it is always clear
    // which side has focus.
    if under_cursor && panel_active {
        Style::default()
            .bg(CURSOR_BG)
            .fg(CURSOR_FG)
            .add_modifier(Modifier::BOLD)
    } else if under_cursor {
        Style::default()
            .bg(BG)
            .fg(fg)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        let style = Style::default().bg(BG).fg(fg);
        if is_dir || marked {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}

/// The strip of tabs across the top of a pane.
///
/// The tab on show wears the cursor bar's colours, so the thing you are
/// looking at and the tab it belongs to are marked the same way. The rest are
/// the panel's ordinary text, which is what makes the current one stand out
/// without a second bright colour competing with the cursor. A pane that does
/// not have the keyboard underlines its tab instead of filling it, for the
/// same reason its cursor bar is not filled either.
pub fn tab_style(current: bool, panel_active: bool) -> Style {
    if current && panel_active {
        Style::default()
            .bg(CURSOR_BG)
            .fg(CURSOR_FG)
            .add_modifier(Modifier::BOLD)
    } else if current {
        Style::default()
            .bg(BG)
            .fg(DIR_FG)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().bg(BG).fg(FILE_FG)
    }
}

pub fn border_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(BG)
            .fg(BORDER_ACTIVE_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(BG).fg(BORDER_FG)
    }
}

pub fn title_style() -> Style {
    Style::default()
        .bg(BG)
        .fg(TITLE_FG)
        .add_modifier(Modifier::BOLD)
}

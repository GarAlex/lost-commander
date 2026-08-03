//! All drawing. Keeping it in one place means the rest of the program never
//! touches ratatui types.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, ConnTab, FindField, Mode, RenameField, Side};
use crate::theme;
use lost_commander_core::entry::{fit, format_time, human_size, size_cell, size_in_words};
use lost_commander_core::panel::Panel;
use lost_commander_core::tabs::Tabs;
use lost_commander_core::tree::Tree;

const SIZE_COL: usize = 9;
const DATE_COL: usize = 14;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::base()), area);

    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1), // status
        Constraint::Length(1), // the command line
        Constraint::Length(1), // function keys
    ])
    .split(area);

    let panes =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[0]);

    draw_pane(
        frame,
        panes[0],
        &app.left,
        app.active == Side::Left,
        app.on_tree[0],
    );
    draw_pane(
        frame,
        panes[1],
        &app.right,
        app.active == Side::Right,
        app.on_tree[1],
    );
    draw_status(frame, rows[1], app);
    draw_command_line(frame, rows[2], app);
    draw_keybar(frame, rows[3]);

    match &app.mode {
        Mode::Normal => {}
        Mode::Input(dialog) => draw_input(frame, area, dialog),
        Mode::Confirm(dialog) => draw_confirm(frame, area, dialog),
        Mode::Viewer {
            title,
            lines,
            scroll,
            forced,
            detected,
            ..
        } => draw_viewer(frame, area, title, lines, *scroll, *forced, *detected),
        Mode::Connections { tab, cursor } => draw_connections(frame, area, app, *tab, *cursor),
        Mode::Progress => draw_progress(frame, area, app),
        Mode::Help { scroll } => draw_help(frame, area, *scroll),
        Mode::Overwrite { conflict } => draw_overwrite(frame, area, conflict),
        Mode::Properties { now, cursor, .. } => draw_properties(frame, area, now, *cursor),
        Mode::Find {
            query,
            root,
            field,
            cursor,
        } => draw_find(frame, area, app, query, root, *field, *cursor),
        Mode::MultiRename {
            rules,
            changes,
            field,
            scroll,
            ..
        } => draw_multi_rename(frame, area, rules, changes, *field, *scroll),
        Mode::Bytes {
            name,
            dump,
            scroll,
            editing,
            cursor,
            edits,
            goto,
        } => draw_bytes(
            frame,
            area,
            name,
            dump,
            *scroll,
            *editing,
            *cursor,
            edits,
            goto.as_deref(),
        ),
        Mode::Journal {
            shown,
            days,
            at,
            rows,
            filter,
            cursor,
            searching,
        } => draw_journal(
            frame, area, *shown, days, *at, rows, filter, *cursor, *searching,
        ),
        Mode::Duplicates {
            root,
            options,
            groups,
            cursor,
        } => draw_duplicates(frame, area, app, root, options, groups, *cursor),
        Mode::Difference {
            left,
            right,
            diff,
            scroll,
        } => draw_difference(frame, area, left, right, diff, *scroll),
        Mode::Sync {
            left,
            right,
            options,
            show,
            pairs,
            cursor,
            capped,
        } => draw_sync(
            frame, area, app, left, right, options, show, pairs, *cursor, *capped,
        ),
        Mode::OpenWith {
            target,
            applications,
            typed,
            cursor,
            as_admin,
        } => draw_open_with(frame, area, target, applications, typed, *cursor, *as_admin),
    }
}

fn draw_find(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    query: &lost_commander_core::find::Query,
    root: &std::path::Path,
    field: FindField,
    cursor: usize,
) {
    let found = app.search.as_ref().map(|s| s.snapshot());
    let hits = found.as_ref().map(|f| f.hits.len()).unwrap_or(0);
    let rows = hits.clamp(1, 12);
    let rect = centered(78, rows as u16 + 9, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block("Find");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(4), // where, and the two boxes
        Constraint::Length(1), // the options and the count
        Constraint::Min(1),    // the results
        Constraint::Length(1), // key hints
    ])
    .split(inner);

    // The box with the keyboard carries the cursor, so it is visible which
    // one is being typed into.
    let box_line = |label: &str, value: &str, active: bool| {
        Line::from(Span::styled(
            format!(" {label:<11}{value}{}", if active { "_" } else { " " }),
            Style::default().bg(theme::DIALOG_BG).fg(if active {
                theme::CURSOR_FG
            } else {
                theme::FILE_FG
            }),
        ))
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" in {}", fit(&root.display().to_string(), 70)),
                Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
            )),
            Line::from(""),
            box_line("named", &query.pattern, field == FindField::Named),
            box_line(
                "containing",
                &query.contains,
                field == FindField::Containing,
            ),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    let tick = |on: bool| if on { "x" } else { " " };
    let count = match &found {
        Some(f) if !f.finished => format!("{} so far...", f.hits.len()),
        Some(f) if f.truncated => format!("the first {} - there were more", f.hits.len()),
        Some(f) if f.hits.is_empty() => "nothing found".to_string(),
        Some(f) => format!("{} found", f.hits.len()),
        None => String::new(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                " [{}] F3 match case   [{}] F4 hidden      {count}",
                tick(query.case_sensitive),
                tick(query.include_hidden)
            ),
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )))
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[1],
    );

    match &found {
        Some(found) if !found.hits.is_empty() => {
            let width = split[2].width.saturating_sub(2) as usize;
            let items: Vec<ListItem> = found
                .hits
                .iter()
                .enumerate()
                .map(|(index, hit)| {
                    // Relative to where the search started: the common prefix
                    // is the one part of every path that says nothing.
                    let shown = hit
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&hit.path)
                        .display()
                        .to_string();
                    let text = match (hit.line, &hit.excerpt) {
                        (Some(line), Some(excerpt)) => format!(" {shown}:{line}   {excerpt}"),
                        _ => format!(" {shown}"),
                    };
                    let style = if index == cursor && field == FindField::Results {
                        Style::default()
                            .bg(theme::CURSOR_BG)
                            .fg(theme::CURSOR_FG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)
                    };
                    ListItem::new(Line::from(Span::styled(fit(&text, width), style)))
                })
                .collect();
            let mut state = ListState::default();
            state.select(Some(cursor));
            frame.render_stateful_widget(
                List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
                split[2],
                &mut state,
            );
        }
        Some(found) if !found.finished => {
            frame.render_widget(
                Paragraph::new(format!(" looking in {}", fit(&found.current, 68)))
                    .style(Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)),
                split[2],
            );
        }
        _ => {}
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            " Tab next field  Enter search / go there  Esc close",
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )),
        split[3],
    );
}

/// Files that are the same file twice, and which copies to let go of.
#[allow(clippy::too_many_arguments)]
fn draw_duplicates(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    root: &std::path::Path,
    options: &lost_commander_core::dupes::Options,
    groups: &[lost_commander_core::dupes::Group],
    cursor: usize,
) {
    // `Line` is ratatui's in this module, so the list's own row type keeps
    // its full name.
    use lost_commander_core::dupes::{self, Line as Row};

    let live = app.hunt.as_ref().map(|scan| scan.snapshot());
    let running = live.as_ref().map(|d| !d.finished).unwrap_or(false);

    let rect = centered(92, 24, area);
    frame.render_widget(Clear, rect);
    let block = dialog_block("Duplicate files");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(2), // where, and the options
        Constraint::Min(1),    // the groups
        Constraint::Length(2), // the tally and the keys
    ])
    .split(inner);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    let heading = Style::default()
        .bg(theme::DIALOG_BG)
        .fg(theme::DIR_FG)
        .add_modifier(Modifier::BOLD);
    let going = Style::default().bg(theme::DIALOG_BG).fg(theme::ERROR_FG);
    let tick = |on: bool| if on { "x" } else { " " };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" under  {}", fit(&root.display().to_string(), 78)),
                dim,
            )),
            Line::from(Span::styled(
                format!(" [{}] F4 hidden files", tick(options.include_hidden)),
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    let width = split[1].width.saturating_sub(4) as usize;
    if running {
        let where_ = live.as_ref().map(|d| d.current.clone()).unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" looking at {}", fit(&where_, width)),
                plain,
            )))
            .style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
        );
    } else {
        let lines = dupes::lines(groups);
        let items: Vec<ListItem> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let (text, style) = match *line {
                    Row::Heading { group } => {
                        let set = &groups[group];
                        (
                            format!(
                                " {} copies of {} each",
                                set.copies.len(),
                                size_in_words(set.size)
                            ),
                            heading,
                        )
                    }
                    Row::Copy { group, copy } => {
                        let copy = &groups[group].copies[copy];
                        let shown = copy
                            .path
                            .strip_prefix(root)
                            .unwrap_or(&copy.path)
                            .display()
                            .to_string();
                        (
                            format!("   [{}] {}", tick(copy.remove), fit(&shown, width)),
                            if copy.remove { going } else { plain },
                        )
                    }
                };
                let style = if index == cursor {
                    Style::default()
                        .bg(theme::CURSOR_BG)
                        .fg(theme::CURSOR_FG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style
                };
                ListItem::new(Line::from(Span::styled(text, style)))
            })
            .collect();
        // The cursor is handed to the widget rather than sliced out by hand:
        // ratatui scrolls a stateful list to keep its selection on screen,
        // which is the part a hand-rolled window forgets - walk far enough
        // down a thousand sets and the cursor simply leaves the box.
        let mut state = ListState::default();
        state.select(Some(cursor.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
            &mut state,
        );
    }

    let say = if running {
        " looking...".to_string()
    } else if groups.is_empty() {
        " no duplicates here".to_string()
    } else {
        format!(
            " {} sets, {} the same thing twice, {} ticked to go",
            groups.len(),
            size_in_words(dupes::wasted(groups)),
            size_in_words(dupes::reclaimed(groups))
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(say, plain)),
            Line::from(Span::styled(
                " Space tick / thin a set  Enter go there  F8 delete the ticked  Esc close",
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[2],
    );
}

/// A file that is not text, as bytes.
#[allow(clippy::too_many_arguments)]
fn draw_bytes(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    dump: &lost_commander_core::hex::Dump,
    scroll: u64,
    editing: bool,
    cursor: lost_commander_core::hex::Cursor,
    edits: &lost_commander_core::hex::Edits,
    goto: Option<&str>,
) {
    use lost_commander_core::hex;

    let rect = centered(area.width, area.height, area);
    frame.render_widget(Clear, rect);
    let heading = format!(
        "{name}   {}{}",
        size_in_words(dump.size),
        if editing { "   [editing]" } else { "" }
    );
    let block = dialog_block(&heading);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    // A changed byte, and the one under the cursor. Two different questions,
    // so two different marks: what you altered, and where you are.
    let altered = Style::default()
        .bg(theme::DIALOG_BG)
        .fg(theme::ADDED_FG)
        .add_modifier(Modifier::BOLD);
    let on_it = Style::default()
        .bg(theme::CURSOR_BG)
        .fg(theme::CURSOR_FG)
        .add_modifier(Modifier::BOLD);

    // Only what is on screen is read, which is the whole point of holding a
    // path rather than a file.
    let width = dump.offset_width();
    let mut rows = dump
        .read(scroll, split[0].height as usize)
        .unwrap_or_default();
    for row in &mut rows {
        edits.overlay(row);
    }

    let body: Vec<Line> = rows
        .iter()
        .map(|row| {
            let mut spans = vec![Span::styled(
                format!(" {:0width$x}  ", row.offset, width = width),
                dim,
            )];
            // Byte by byte rather than one string, so the cursor and the
            // changes can be coloured where they are.
            for at in 0..hex::PER_ROW {
                let offset = row.offset + at as u64;
                let here = editing && offset == cursor.at;
                let style = match (
                    here && cursor.pane == hex::Pane::Hex,
                    edits.is_changed(offset),
                ) {
                    (true, _) => on_it,
                    (false, true) => altered,
                    _ => plain,
                };
                spans.push(Span::styled(row.pair(at), style));
                spans.push(Span::styled(
                    if at + 1 == hex::PER_ROW / 2 {
                        "  "
                    } else {
                        " "
                    },
                    plain,
                ));
            }
            spans.push(Span::styled(" |", dim));
            for (at, byte) in row.bytes.iter().enumerate() {
                let offset = row.offset + at as u64;
                let here = editing && offset == cursor.at;
                let style = match (
                    here && cursor.pane == hex::Pane::Text,
                    edits.is_changed(offset),
                ) {
                    (true, _) => on_it,
                    (false, true) => altered,
                    _ => dim,
                };
                spans.push(Span::styled(hex::printable(*byte).to_string(), style));
            }
            spans.push(Span::styled("|", dim));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(body).style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    // While an offset is being typed it is the only thing the footer says.
    // The hints below it are for keys that are not reaching the view anyway.
    let footer = if let Some(typed) = goto {
        let understood = hex::parse_offset(typed, dump.size).is_some();
        format!(
            " go to offset: {typed}{}   hex, or 0n for decimal   Enter jumps  Esc stops{}",
            if typed.is_empty() { "_" } else { "" },
            if understood || typed.is_empty() {
                String::new()
            } else {
                "   not an offset".to_string()
            }
        )
    } else if editing {
        let changed = edits.describe();
        format!(
            " {:#x}  {} column  0-9a-f types  Tab swaps  Backspace undoes  F2 writes  Esc stops{}",
            cursor.at,
            cursor.pane.label(),
            if changed.is_empty() {
                String::new()
            } else {
                format!("   {changed}")
            }
        )
    } else {
        let at = scroll * hex::PER_ROW as u64;
        format!(
            " offset {at:#x} of {:#x}   Up/Down PgUp/PgDn Home/End move  g goes to  F4 edits  Esc close",
            dump.size
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, dim)))
            .style(Style::default().bg(theme::DIALOG_BG)),
        split[1],
    );
}

/// Two files side by side, with what differs marked.
fn draw_difference(
    frame: &mut Frame,
    area: Rect,
    left_path: &std::path::Path,
    right_path: &std::path::Path,
    diff: &lost_commander_core::diff::Diff,
    scroll: usize,
) {
    use lost_commander_core::diff::{gutter, gutter_width};

    // The whole window: two files side by side want every column there is.
    let rect = centered(area.width, area.height, area);
    frame.render_widget(Clear, rect);
    let block = dialog_block("Compare files");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(1), // the two names
        Constraint::Min(1),    // the lines
        Constraint::Length(1), // the tally and the keys
    ])
    .split(inner);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    let half = (split[1].width as usize).saturating_sub(1) / 2;
    let numbers = gutter_width(diff);
    let text_width = half.saturating_sub(numbers + 2);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {:<w$}", fit(&name_of(left_path), half), w = half),
                dim,
            ),
            Span::styled(
                format!("{:<w$}", fit(&name_of(right_path), half), w = half),
                dim,
            ),
        ]))
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    let removed = Style::default().bg(theme::DIALOG_BG).fg(theme::ERROR_FG);
    let added = Style::default().bg(theme::DIALOG_BG).fg(theme::ADDED_FG);
    let cell = |side: Option<(usize, &str)>, style: Style| -> Span<'static> {
        match side {
            Some((number, text)) => Span::styled(
                format!(
                    " {} {:<w$}",
                    gutter(Some(number), numbers),
                    fit(text, text_width),
                    w = text_width
                ),
                style,
            ),
            // Nothing on this side is drawn as nothing, which is what says
            // the line was added or removed without a word of explanation.
            None => Span::styled(" ".repeat(half), dim),
        }
    };

    let rows: Vec<Line> = diff
        .rows
        .iter()
        .skip(scroll)
        .take(split[1].height as usize)
        .map(|row| {
            let (left_style, right_style) = if row.is_same() {
                (plain, plain)
            } else {
                (removed, added)
            };
            Line::from(vec![
                cell(row.left(), left_style),
                cell(row.right(), right_style),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows).style(Style::default().bg(theme::DIALOG_BG)),
        split[1],
    );

    let say = if diff.unaligned {
        " too different to line up - both shown as they are".to_string()
    } else {
        format!(" {} line(s) differ", diff.changes)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{say}   "), plain),
            Span::styled(
                "Tab/n next difference  p previous  Up/Down scroll  Esc close",
                dim,
            ),
        ]))
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[2],
    );
}

/// A path's last component, which is all a heading has room for.
fn name_of(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// The synchronize form: what differs, and which way each pair would go.
#[allow(clippy::too_many_arguments)]
fn draw_sync(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    left: &std::path::Path,
    right: &std::path::Path,
    options: &lost_commander_core::compare::Options,
    show: &lost_commander_core::compare::Show,
    pairs: &[lost_commander_core::compare::Pair],
    cursor: usize,
    capped: bool,
) {
    use lost_commander_core::compare;

    let live = app.scan.as_ref().map(|scan| scan.snapshot());
    let running = live.as_ref().map(|c| !c.finished).unwrap_or(false);

    let rect = centered(100, 22, area);
    frame.render_widget(Clear, rect);
    let block = dialog_block("Synchronize");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(3), // the two roots and the options
        Constraint::Min(1),    // the pairs
        Constraint::Length(2), // the tally and the key hints
    ])
    .split(inner);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    let tick = |on: bool| if on { "x" } else { " " };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" left   {}", fit(&left.display().to_string(), 80)),
                dim,
            )),
            Line::from(Span::styled(
                format!(" right  {}", fit(&right.display().to_string(), 80)),
                dim,
            )),
            Line::from(Span::styled(
                format!(
                    " [{}] F6 subdirectories  [{}] F3 by content  [{}] F4 hidden  [{}] = show same",
                    tick(options.recursive),
                    tick(options.by_content),
                    tick(options.include_hidden),
                    tick(show.same)
                ),
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    let rows = split[1].height as usize;
    let width = split[1].width as usize;
    // A size column is `%8s %14s`: eight for the size and the panels' own
    // date format. The name gets what those two and the arrow leave, and the
    // row is built to exactly that so the right-hand side is not cut off.
    const SIDE: usize = 8 + 1 + 14;
    let name_width = width.saturating_sub(2 * SIDE + 8).max(10);

    if let Some(found) = &live {
        // While it runs the list is the worker's, and it is only worth showing
        // where it has got to - the rows cannot be turned round yet anyway.
        let mut body = vec![Line::from(Span::styled(
            format!(
                " comparing {}",
                fit(&found.current, width.saturating_sub(12))
            ),
            plain,
        ))];
        for pair in found.pairs.iter().rev().take(rows.saturating_sub(1)).rev() {
            body.push(Line::from(Span::styled(
                format!("  {}", fit(&pair.name, width.saturating_sub(3))),
                dim,
            )));
        }
        frame.render_widget(
            Paragraph::new(body).style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
        );
    } else {
        let showing: Vec<&compare::Pair> = pairs.iter().filter(|p| show.allows(p.state)).collect();
        let items: Vec<ListItem> = showing
            .iter()
            .enumerate()
            .map(|(index, pair)| {
                let style = if index == cursor {
                    Style::default()
                        .bg(theme::CURSOR_BG)
                        .fg(theme::CURSOR_FG)
                        .add_modifier(Modifier::BOLD)
                } else if pair.state.is_same() {
                    dim
                } else {
                    plain
                };
                ListItem::new(Line::from(Span::styled(
                    format!(
                        " {:<name_width$} {:>SIDE$}  {:^2}  {:<SIDE$}",
                        fit(&pair.name, name_width),
                        side_cell(pair.left.as_ref()),
                        pair.direction.mark(),
                        side_cell(pair.right.as_ref()),
                    ),
                    style,
                )))
            })
            .collect();
        // As in the duplicates list: the widget keeps its own selection on
        // screen, and a window sliced by hand does not.
        let mut state = ListState::default();
        state.select(Some(cursor.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
            &mut state,
        );
    }

    let tally = compare::tally(pairs);
    let say = if running {
        "comparing...".to_string()
    } else if pairs.is_empty() {
        "nothing to compare".to_string()
    } else {
        format!(
            " {} to the right, {} to the left, {} the same, {} left alone{}",
            tally.to_right,
            tally.to_left,
            tally.same,
            tally.skipped_differences,
            // A list that stops short in silence reads as the whole tree.
            if capped {
                format!("  (the first {} - there is more)", pairs.len())
            } else {
                String::new()
            }
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(say, plain)),
            Line::from(Span::styled(
                " Space turns one  <- -> all  - none  * reset  F5 run  F2 again  Esc close",
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[2],
    );
}

/// The account of what was done: one day, filtered, a flat list of lines.
#[allow(clippy::too_many_arguments)]
fn draw_journal(
    frame: &mut Frame,
    area: Rect,
    shown: lost_commander_core::journal::Shown,
    days: &[lost_commander_core::journal::Day],
    at: usize,
    rows: &[lost_commander_core::journal::Row],
    filter: &lost_commander_core::journal::Filter,
    cursor: usize,
    searching: bool,
) {
    use lost_commander_core::journal;

    let rect = centered(
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
        area,
    );
    frame.render_widget(Clear, rect);
    let block = dialog_block("What was done");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(2), // the day and the filter
        Constraint::Min(1),    // the list
        Constraint::Length(2), // the count and the keys
    ])
    .split(inner);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    let bad = Style::default().bg(theme::DIALOG_BG).fg(theme::ERROR_FG);

    let day = days.get(at);
    let kinds = match filter.kinds.first() {
        Some(kind) => kind.short().to_string(),
        None => "everything".to_string(),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(
                    " {}   {}   ({} of {} day(s))",
                    shown.label(),
                    day.map(|d| d.describe())
                        .unwrap_or_else(|| "nothing recorded".into()),
                    if days.is_empty() { 0 } else { at + 1 },
                    days.len()
                ),
                dim,
            )),
            Line::from(Span::styled(
                format!(
                    " showing {kinds}{}{}",
                    match (searching, filter.text.trim().is_empty()) {
                        (true, _) => format!("   find: {}_", filter.text),
                        (false, false) => format!("   find: {}", filter.text),
                        (false, true) => String::new(),
                    },
                    if filter.failures_only {
                        ", only what failed"
                    } else {
                        ""
                    }
                ),
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    let showing = filter.apply(rows.to_vec());
    let drawn = journal::lines(&showing);
    let width = split[1].width as usize;
    let items: Vec<ListItem> = drawn
        .iter()
        .map(|line| {
            let text = match line {
                journal::Line::Heading { row } => {
                    let journal::Row::Run {
                        group,
                        events,
                        took,
                    } = &showing[*row]
                    else {
                        return ListItem::new("");
                    };
                    format!(
                        " {}  {}  ({} file(s)){}",
                        journal::clock(group.at),
                        group.summary,
                        events.len(),
                        // The total for the run, where a per-file duration
                        // would be noise. No total means it never finished.
                        match took {
                            Some(ms) => format!("  {}", journal::took(*ms)),
                            None => String::new(),
                        }
                    )
                }
                other => match journal::event_at(&showing, other) {
                    None => String::new(),
                    Some(event) => {
                        let indent = if matches!(other, journal::Line::Under { .. }) {
                            "     "
                        } else {
                            " "
                        };
                        let arrow = match &event.to {
                            Some(to) => format!(" -> {to}"),
                            None => String::new(),
                        };
                        let note = match (event.note.is_empty(), &event.failed) {
                            (_, Some(why)) => format!("   [{why}]"),
                            (false, None) => format!("   {}", event.note),
                            _ => String::new(),
                        };
                        let lasted = match event.ms {
                            Some(ms) => format!("   {}", journal::took(ms)),
                            None => String::new(),
                        };
                        format!(
                            "{indent}{}  {:<11} {}{arrow}{note}{lasted}",
                            journal::clock(event.at),
                            // The shell's name where there is one - "Command"
                            // on every line says nothing.
                            event.label(),
                            event.path
                        )
                    }
                },
            };
            let failed = journal::event_at(&showing, line)
                .map(|e| e.is_failure())
                .unwrap_or(false);
            let style = if failed {
                bad
            } else if line.is_heading() {
                plain.add_modifier(Modifier::BOLD)
            } else {
                plain
            };
            ListItem::new(Line::from(Span::styled(fit(&text, width), style)))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(cursor.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .style(Style::default().bg(theme::DIALOG_BG))
            .highlight_style(
                Style::default()
                    .bg(theme::CURSOR_BG)
                    .fg(theme::CURSOR_FG)
                    .add_modifier(Modifier::BOLD),
            ),
        split[1],
        &mut state,
    );

    let counted = journal::tally(&showing);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(
                    " {} entr{}, {} file(s){}",
                    counted.rows,
                    if counted.rows == 1 { "y" } else { "ies" },
                    counted.items,
                    match counted.failures {
                        0 => String::new(),
                        n => format!(", {n} that did not work"),
                    }
                ),
                if counted.failures > 0 { bad } else { plain },
            )),
            Line::from(Span::styled(
                match searching {
                    true => " type to search anything on a line  Enter keep it  Esc clear",
                    false =>
                        " Left/Right day  Tab all/files/commands  / find  k kind  ! only failures  Esc close",
                },
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[2],
    );
}

/// One side of a comparison row: how big it is and when it changed.
///
/// Blank where that side has nothing, which is what says "only the other one
/// has this" without a word of explanation.
fn side_cell(facts: Option<&lost_commander_core::compare::Facts>) -> String {
    match facts {
        None => String::new(),
        Some(facts) if facts.is_dir => "<DIR>".to_string(),
        Some(facts) => format!(
            "{:>8} {}",
            human_size(facts.size),
            format_time(facts.modified)
        ),
    }
}

/// The multi-rename form: the rules on top, and what they would do below.
fn draw_multi_rename(
    frame: &mut Frame,
    area: Rect,
    rules: &lost_commander_core::rename::Rules,
    changes: &[lost_commander_core::rename::Change],
    field: RenameField,
    scroll: usize,
) {
    use lost_commander_core::rename;

    let rows = changes.len().clamp(1, 12);
    let rect = centered(76, rows as u16 + 12, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block("Rename files");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(5), // the four boxes and the case line
        Constraint::Length(3), // what the placeholders mean
        Constraint::Min(1),    // the preview
        Constraint::Length(2), // the tally and the key hints
    ])
    .split(inner);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    // The box with the keyboard carries the cursor, so it is visible which
    // one is being typed into.
    let box_line = |which: RenameField, value: &str| {
        let active = which == field;
        Line::from(Span::styled(
            format!(
                " {:<11}{value}{}",
                which.label(),
                if active { "_" } else { "" }
            ),
            Style::default().bg(theme::DIALOG_BG).fg(if active {
                theme::CURSOR_FG
            } else {
                theme::FILE_FG
            }),
        ))
    };
    let tick = if rules.case_sensitive { "x" } else { " " };
    frame.render_widget(
        Paragraph::new(vec![
            box_line(RenameField::Name, &rules.name),
            box_line(RenameField::Extension, &rules.extension),
            box_line(RenameField::Find, &rules.find),
            box_line(RenameField::Replace, &rules.replace),
            Line::from(vec![
                Span::styled(
                    format!(" {:<11}", RenameField::Case.label()),
                    if field == RenameField::Case {
                        Style::default().bg(theme::DIALOG_BG).fg(theme::CURSOR_FG)
                    } else {
                        plain
                    },
                ),
                Span::styled(
                    format!("< {} >", rules.case.label()),
                    if field == RenameField::Case {
                        Style::default()
                            .bg(theme::CURSOR_BG)
                            .fg(theme::CURSOR_FG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        plain
                    },
                ),
                Span::styled(format!("   [{tick}] F3 match case"), dim),
            ]),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[0],
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                " [N] name  [E] extension  [C] counter  [N2-5] part of the name",
                dim,
            )),
            Line::from(Span::styled(
                " [C001] pads to three  [C10+2] from ten by twos  [Y][M][D] the file's date",
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[1],
    );

    let width = split[2].width.saturating_sub(2) as usize;
    let was_width = (width / 2).min(34);
    let items: Vec<ListItem> = changes
        .iter()
        .skip(scroll)
        .map(|change| {
            // A name that is not changing is shown as it is, so a rule that
            // misses half the selection is visible before it runs.
            let (shown, style) = match change.trouble {
                Some(trouble) => (
                    trouble.message().to_string(),
                    Style::default().bg(theme::DIALOG_BG).fg(theme::ERROR_FG),
                ),
                None if change.is_rename() => (change.name.clone(), plain),
                None => (change.name.clone(), dim),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<was_width$} ", fit(&change.was, was_width)),
                    dim,
                ),
                Span::styled(fit(&shown, width.saturating_sub(was_width + 2)), style),
            ]))
        })
        .collect();
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
        split[2],
    );

    let (moving, troubled) = rename::tally(changes);
    let tally = if troubled > 0 {
        format!(" {moving} to rename, {troubled} that cannot be")
    } else {
        format!(" {moving} to rename")
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                tally,
                Style::default().bg(theme::DIALOG_BG).fg(if troubled > 0 {
                    theme::ERROR_FG
                } else {
                    theme::FILE_FG
                }),
            )),
            Line::from(Span::styled(
                " Tab next field  Up/Down scroll  Enter rename  Esc cancel",
                dim,
            )),
        ])
        .style(Style::default().bg(theme::DIALOG_BG)),
        split[3],
    );
}

fn draw_properties(
    frame: &mut Frame,
    area: Rect,
    now: &lost_commander_core::perms::Properties,
    cursor: usize,
) {
    use lost_commander_core::perms::{What, Who};

    let rect = centered(66, 17, area);
    frame.render_widget(Clear, rect);
    let block = dialog_block("Properties");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let plain = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let dim = Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG);
    let mut body = vec![
        Line::from(Span::styled(format!(" {}", now.name()), plain)),
        Line::from(Span::styled(
            format!(" {}", fit(&now.path.display().to_string(), 62)),
            dim,
        )),
        Line::from(""),
    ];

    let kind = if now.is_symlink {
        "symbolic link"
    } else if now.kind == lost_commander_core::entry::EntryKind::Dir {
        "directory"
    } else {
        "file"
    };
    let mut fact = |label: &str, value: String| {
        body.push(Line::from(Span::styled(
            format!(" {label:<10} {value}"),
            plain,
        )));
    };
    fact("type", kind.to_string());
    if let Some(target) = &now.link_target {
        fact("points at", fit(&target.display().to_string(), 50));
    }
    if now.kind != lost_commander_core::entry::EntryKind::Dir {
        // Both, because "4.2K" is what you read and the count is what you
        // check.
        fact(
            "size",
            format!("{}  ({} bytes)", human_size(now.size), now.size),
        );
    }
    fact("modified", format_time(now.modified));
    if let Some(owner) = &now.owner {
        fact("owner", owner.clone());
    }
    if let Some(group) = &now.group {
        fact("group", group.clone());
    }

    body.push(Line::from(""));
    match now.mode {
        Some(mode) => {
            body.push(Line::from(Span::styled(
                format!(
                    " permissions   {}{}   {}",
                    lost_commander_core::perms::kind_char(now.kind, now.is_symlink),
                    mode.symbolic(),
                    mode.octal()
                ),
                plain,
            )));
            body.push(Line::from(Span::styled(
                "                read  write  exec".to_string(),
                dim,
            )));
            for (row, who) in Who::ALL.iter().enumerate() {
                let mut spans = vec![Span::styled(format!(" {:<12}  ", who.label()), plain)];
                for (column, what) in What::ALL.iter().enumerate() {
                    let index = row * 3 + column;
                    let tick = if mode.is_set(*who, *what) { "x" } else { " " };
                    let style = if index == cursor {
                        Style::default()
                            .bg(theme::CURSOR_BG)
                            .fg(theme::CURSOR_FG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        plain
                    };
                    spans.push(Span::styled(format!("[{tick}]"), style));
                    spans.push(Span::styled("    ", plain));
                }
                body.push(Line::from(spans));
            }
            let special =
                |on: bool, label: &str| format!("[{}] {label}", if on { "x" } else { " " });
            body.push(Line::from(Span::styled(
                format!(
                    " {}  {}  {}",
                    special(mode.has(lost_commander_core::perms::SETUID), "u setuid"),
                    special(mode.has(lost_commander_core::perms::SETGID), "g setgid"),
                    special(mode.has(lost_commander_core::perms::STICKY), "t sticky"),
                ),
                plain,
            )));
        }
        None => body.push(Line::from(Span::styled(
            format!(" [{}] read-only", if now.readonly { "x" } else { " " }),
            plain,
        ))),
    }

    frame.render_widget(
        Paragraph::new(body).style(Style::default().bg(theme::DIALOG_BG)),
        inner,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            " arrows move  Space toggles  Enter applies  Esc closes",
            dim,
        )),
        Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        },
    );
}

fn draw_overwrite(
    frame: &mut Frame,
    area: Rect,
    conflict: &lost_commander_core::progress::Conflict,
) {
    let rect = centered(80, 9, area);
    frame.render_widget(Clear, rect);

    let name = conflict
        .target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let block = dialog_block("Already there");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Both sides, so the answer is a comparison rather than a guess, and the
    // newer one is marked - the fact the question turns on nine times in ten.
    let newer = conflict.source_is_newer();
    let side = |label: &str, size: u64, when: Option<std::time::SystemTime>, is_newer: bool| {
        format!(
            " {label} {:>9}  {}{}",
            human_size(size),
            format_time(when),
            if is_newer { "   (newer)" } else { "" }
        )
    };

    let body = vec![
        Line::from(Span::styled(
            format!(" {name} already exists."),
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            side(
                "there now",
                conflict.target_size,
                conflict.target_modified,
                newer == Some(false),
            ),
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
        Line::from(Span::styled(
            side(
                "arriving ",
                conflict.source_size,
                conflict.source_modified,
                newer == Some(true),
            ),
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " (s)kip  s(k)ip all  (o)verwrite  overwrite (a)ll  only (n)ewer  (c)ancel",
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )),
    ];
    frame.render_widget(
        Paragraph::new(body).style(Style::default().bg(theme::DIALOG_BG)),
        inner,
    );
}

fn draw_open_with(
    frame: &mut Frame,
    area: Rect,
    target: &std::path::Path,
    applications: &[lost_commander_core::apps::Application],
    typed: &str,
    cursor: usize,
    as_admin: bool,
) {
    let matches = lost_commander_core::apps::matching(applications, typed);
    let rows = matches.len().clamp(1, 14);
    let rect = centered(70, rows as u16 + 6, area);
    frame.render_widget(Clear, rect);

    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let block = dialog_block("Open with");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(2), // the file, and the box
        Constraint::Min(1),    // the list
        Constraint::Length(1), // key hints
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{name} with:"),
                Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
            )),
            Line::from(Span::styled(
                format!("> {typed}_"),
                Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
            )),
        ]),
        split[0],
    );

    if matches.is_empty() {
        let message = if typed.trim().is_empty() {
            "No applications found - type a command to run.".to_string()
        } else {
            format!("Run: {}", typed.trim())
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)),
            split[1],
        );
    } else {
        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .map(|(index, app)| {
                // The ones that claim this type are marked rather than merely
                // sorted first: a list is only as ordered as it looks.
                let mark = if app.handles { "*" } else { " " };
                let suffix = if app.terminal { "  (in a shell)" } else { "" };
                let text = format!(
                    " {mark} {}{suffix}",
                    fit(&app.name, split[1].width.saturating_sub(6) as usize)
                );
                let style = if index == cursor {
                    Style::default()
                        .bg(theme::CURSOR_BG)
                        .fg(theme::CURSOR_FG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)
                };
                ListItem::new(Line::from(Span::styled(text, style)))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(cursor));
        frame.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
            &mut state,
        );
    }

    let hints = if as_admin {
        "Ctrl-A as administrator: ON   Up/Down choose  Enter open  Esc cancel"
    } else {
        "type to filter  Ctrl-A as administrator  Enter open  Esc cancel"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            hints,
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )),
        split[2],
    );
}

/// One pane: its tabs, if it has more than one, and the tab on show.
fn draw_pane(frame: &mut Frame, area: Rect, tabs: &Tabs, active: bool, on_tree: bool) {
    if tabs.len() < 2 {
        // One tab is not a row of tabs, it is a pane - and drawing a strip
        // over it would cost a line of listing to say nothing.
        draw_panel(frame, area, tabs.current(), active, on_tree);
        return;
    }
    let split = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    draw_tab_strip(frame, split[0], tabs, active);
    draw_panel(frame, split[1], tabs.current(), active, on_tree);
}

fn draw_tab_strip(frame: &mut Frame, area: Rect, tabs: &Tabs, active: bool) {
    let paths: Vec<std::path::PathBuf> = tabs.all().iter().map(|p| p.cwd.clone()).collect();
    let names = lost_commander_core::tabs::titles(&paths);
    let width = area.width as usize;

    // As many as fit, and a count of what did not. Truncating the strip is
    // better than truncating every name to nothing.
    let mut spans = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (index, name) in names.iter().enumerate() {
        let label = format!(" {} ", fit(name, 18));
        if used + label.chars().count() + 4 > width && index != tabs.active() {
            break;
        }
        used += label.chars().count();
        shown += 1;
        spans.push(Span::styled(
            label,
            theme::tab_style(index == tabs.active(), active),
        ));
    }
    if shown < names.len() {
        spans.push(Span::styled(
            format!(" +{} ", names.len() - shown),
            theme::tab_style(false, active),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme::base()), area);
}

fn draw_panel(frame: &mut Frame, area: Rect, panel: &Panel, active: bool, on_tree: bool) {
    let heading = if panel.in_tree_mode() {
        format!("Tree: {}", panel.cwd.display())
    } else {
        panel.cwd.display().to_string()
    };
    let title = fit(&heading, area.width.saturating_sub(4) as usize);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style(active))
        .title(Span::styled(format!(" {title} "), theme::title_style()))
        .title_alignment(Alignment::Center);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // With a tree, the pane is two halves: directories above, the files of
    // wherever you are below. XTree's arrangement, and it was a program for
    // this screen - a tree that closed itself the moment you chose a
    // directory is a chooser, not a tree you can work in.
    if let Some(tree) = &panel.tree {
        let halves =
            Layout::vertical([Constraint::Percentage(45), Constraint::Min(3)]).split(inner);
        draw_tree(frame, halves[0], tree, active && on_tree);
        draw_listing(frame, halves[1], panel, active && !on_tree, true);
        return;
    }
    draw_listing(frame, inner, panel, active, false);
}

/// The file listing: header row, then the rows themselves.
///
/// `files_only` leaves out the directories and `..`, which is what the half
/// under a tree wants - they are the half above it, and drawing them twice
/// would be one list repeated with the cursor ambiguous between them.
fn draw_listing(frame: &mut Frame, inner: Rect, panel: &Panel, active: bool, files_only: bool) {
    // Header row, then the listing below it.
    let split = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    let width = inner.width as usize;
    let name_width = width.saturating_sub(SIZE_COL + DATE_COL + 2).max(4);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "{:<name_width$} {:>SIZE_COL$} {:<DATE_COL$}",
                fit("Name", name_width),
                "Size",
                "Modified"
            ),
            Style::default()
                .bg(theme::BG)
                .fg(theme::TITLE_FG)
                .add_modifier(Modifier::BOLD),
        )))
        .style(theme::base()),
        split[0],
    );

    if let Some(error) = &panel.error {
        frame.render_widget(
            Paragraph::new(format!("<{error}>"))
                .style(Style::default().bg(theme::BG).fg(theme::ERROR_FG))
                .wrap(Wrap { trim: true }),
            split[1],
        );
        return;
    }

    // The rows this half is showing, and where the cursor sits among them.
    // Kept as a list of indices rather than filtering in place, because the
    // cursor is an index into `entries` and the widget wants an index into
    // what it is drawing - and getting that wrong highlights the wrong file.
    let shown: Vec<usize> = panel
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| !files_only || (!entry.is_dir() && !entry.is_parent()))
        .map(|(index, _)| index)
        .collect();
    let selected = shown.iter().position(|index| *index == panel.cursor);

    let items: Vec<ListItem> = shown
        .iter()
        .map(|index| (*index, &panel.entries[*index]))
        .map(|(index, entry)| {
            let marker = if entry.marked { '*' } else { ' ' };
            // Symlinks get an ls -F style suffix so they are distinguishable.
            let label = if entry.is_symlink {
                format!("{}@", entry.name)
            } else {
                entry.name.clone()
            };
            let name = fit(&label, name_width.saturating_sub(1));
            let text = format!(
                "{marker}{:<width$} {:>SIZE_COL$} {:<DATE_COL$}",
                name,
                size_cell(entry),
                format_time(entry.modified),
                width = name_width.saturating_sub(1)
            );
            ListItem::new(Line::from(Span::styled(
                text,
                theme::entry_style(entry.is_dir(), entry.marked, index == panel.cursor, active),
            )))
        })
        .collect();

    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(List::new(items).style(theme::base()), split[1], &mut state);
}

fn draw_tree(frame: &mut Frame, area: Rect, tree: &Tree, active: bool) {
    let width = area.width as usize;

    let items: Vec<ListItem> = tree
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            // Two columns of indent per level keeps deep trees readable
            // without running out of width too quickly.
            let indent = "  ".repeat(node.depth);
            let text = format!("{indent}{} {}", tree.marker(index), node.label);
            ListItem::new(Line::from(Span::styled(
                fit(&text, width),
                theme::entry_style(true, false, index == tree.cursor, active),
            )))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(tree.cursor));
    frame.render_stateful_widget(List::new(items).style(theme::base()), area, &mut state);
}

/// The command line, under the panels and over the function keys.
///
/// Always there rather than appearing when typed into: a prompt that is only
/// present once you have started is one nobody discovers, and this is the
/// part of Norton Commander that made it a shell you could see the files
/// from rather than a file manager with a shell bolted on.
///
/// The prompt is the directory being shown, because that is where a command
/// will run.
fn draw_command_line(frame: &mut Frame, area: Rect, app: &App) {
    let cwd = app.active_panel().cwd.display().to_string();
    // The tail of a long path, not the head: the end says which directory
    // this is, and the beginning only says which disk.
    let shown = if cwd.chars().count() > 40 {
        let tail: String = cwd.chars().rev().take(38).collect();
        format!("...{}", tail.chars().rev().collect::<String>())
    } else {
        cwd
    };
    let line = Line::from(vec![
        Span::styled(format!("{shown}> "), Style::default().fg(theme::TITLE_FG)),
        Span::styled(app.command.clone(), Style::default().fg(theme::FILE_FG)),
        // A block where the next character will go, drawn rather than moved
        // there with the real cursor - that one belongs to the file panel.
        Span::styled("\u{2588}", Style::default().fg(theme::CURSOR_FG)),
    ]);
    frame.render_widget(Paragraph::new(line).style(theme::base()), area);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let panel = app.active_panel();

    let text = if app.status_is_error {
        app.status.clone()
    } else if let Some(tree) = &panel.tree {
        match tree.selected_path() {
            Some(path) => format!("{}   (tree)", path.display()),
            None => "<empty tree>".to_string(),
        }
    } else {
        let marks = panel.marked_count();
        let mark_note = if marks > 0 {
            format!("  [{} marked, {}]", marks, human_size(panel.marked_size()))
        } else {
            String::new()
        };
        match panel.selected() {
            Some(entry) => format!(
                "{}  {}  {}{}  (sort: {}{})",
                fit(&entry.name, 40),
                size_cell(entry),
                format_time(entry.modified),
                mark_note,
                panel.sort_by.label(),
                if panel.show_hidden { ", hidden" } else { "" }
            ),
            None => format!("<empty>{mark_note}"),
        }
    };

    let style = if app.status_is_error {
        Style::default().bg(theme::BG).fg(theme::ERROR_FG)
    } else {
        Style::default().bg(theme::BG).fg(theme::FILE_FG)
    };

    frame.render_widget(
        Paragraph::new(fit(&text, area.width as usize)).style(style),
        area,
    );
}

fn draw_keybar(frame: &mut Frame, area: Rect) {
    const KEYS: [(&str, &str); 10] = [
        ("1", "Help"),
        ("2", "Rename"),
        ("3", "View"),
        ("4", "Edit"),
        ("5", "Copy"),
        ("6", "Move"),
        ("7", "MkDir"),
        ("8", "Delete"),
        ("9", "Sort"),
        ("10", "Quit"),
    ];

    let mut spans = Vec::with_capacity(KEYS.len() * 2);
    for (number, label) in KEYS {
        spans.push(Span::styled(
            number.to_string(),
            Style::default().bg(theme::BG).fg(theme::KEYNUM_FG),
        ));
        spans.push(Span::styled(
            format!("{label:<7}"),
            Style::default().bg(theme::KEYBAR_BG).fg(theme::KEYBAR_FG),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
        area,
    );
}

/// A centred box, clamped to the available area.
fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

fn dialog_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style(true))
        .style(Style::default().bg(theme::DIALOG_BG))
        .title(Span::styled(format!(" {title} "), theme::title_style()))
        .title_alignment(Alignment::Center)
}

fn draw_input(frame: &mut Frame, area: Rect, dialog: &crate::app::InputDialog) {
    let rect = centered(70, 7, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block(&dialog.title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = vec![
        Line::from(Span::styled(
            dialog.prompt.clone(),
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
        // A block cursor marks the (append-only) edit position.
        Line::from(Span::styled(
            format!("{}_", dialog.value),
            Style::default()
                .bg(theme::DIALOG_BG)
                .fg(theme::DIR_FG)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Enter = confirm    Esc = cancel",
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
    ];

    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
}

fn draw_confirm(frame: &mut Frame, area: Rect, dialog: &crate::app::ConfirmDialog) {
    let rect = centered(64, 7, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block(&dialog.title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = vec![
        Line::from(Span::styled(
            dialog.message.clone(),
            Style::default().bg(theme::DIALOG_BG).fg(theme::DIR_FG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y / Enter = yes     n / Esc = no",
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
    ];

    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
}

fn draw_viewer(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[String],
    scroll: usize,
    forced: Option<lost_commander_core::encoding::Encoding>,
    detected: lost_commander_core::encoding::Detected,
) {
    frame.render_widget(Clear, area);

    // What the bytes were taken for, and how to change it. Worth the room in
    // the title bar: a file read as the wrong encoding is a screen of
    // nonsense, and nothing else on screen says why.
    let reading = match forced {
        Some(encoding) => format!("{} (forced)", encoding.label()),
        None => detected.describe(),
    };
    let heading = format!("View: {title}  [{reading}]  e/E encoding  (Esc closes)");
    let block = dialog_block(&heading);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(height)
        .map(|l| {
            Line::from(Span::styled(
                l.clone(),
                Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_progress(frame: &mut Frame, area: Rect, app: &App) {
    let Some(job) = &app.job else {
        return;
    };
    let progress = job.snapshot();

    let rect = centered(66, 9, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block(progress.verb);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::vertical([
        Constraint::Length(1), // current item
        Constraint::Length(1), // spacer
        Constraint::Length(1), // bar
        Constraint::Length(1), // counts
        Constraint::Min(0),
        Constraint::Length(1), // hint
    ])
    .split(inner);

    let width = inner.width as usize;
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit(&shorten_path(&progress.current, width), width),
            Style::default().bg(theme::DIALOG_BG).fg(theme::DIR_FG),
        )),
        rows[0],
    );

    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().bg(theme::DIALOG_BG).fg(theme::CURSOR_BG))
            .label(format!("{}%", progress.percent()))
            .ratio(progress.fraction()),
        rows[2],
    );

    let counts = if progress.bytes_total > 0 {
        format!(
            "{} / {} items    {} / {}",
            progress.items_done,
            progress.items_total,
            human_size(progress.bytes_done),
            human_size(progress.bytes_total)
        )
    } else {
        format!("{} / {} items", progress.items_done, progress.items_total)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            counts,
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG),
        )),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Esc = cancel",
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )),
        rows[5],
    );
}

/// Keep the tail of a long path, which is the informative part.
fn shorten_path(path: &str, width: usize) -> String {
    let chars = path.chars().count();
    if chars <= width || width < 4 {
        return path.to_string();
    }
    let tail: String = path.chars().skip(chars - (width - 3)).collect::<String>();
    format!("...{tail}")
}

fn draw_connections(frame: &mut Frame, area: Rect, app: &App, tab: ConnTab, cursor: usize) {
    let entries: &[lost_commander_core::netloc::Location] = match tab {
        ConnTab::Saved => &app.bookmarks.locations,
        ConnTab::Recent => &app.bookmarks.recent,
    };

    let rows = entries.len().max(1);
    let rect = centered(78, rows as u16 + 5, area);
    frame.render_widget(Clear, rect);

    let block = dialog_block("Locations");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let split = Layout::vertical([
        Constraint::Length(1), // tab strip
        Constraint::Min(1),    // list
        Constraint::Length(1), // key hints
    ])
    .split(inner);

    // Tab strip; the active list is highlighted.
    let tab_style = |selected: bool| {
        if selected {
            Style::default()
                .bg(theme::CURSOR_BG)
                .fg(theme::CURSOR_FG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" Saved ({}) ", app.bookmarks.len()),
                tab_style(tab == ConnTab::Saved),
            ),
            Span::styled("  ", Style::default().bg(theme::DIALOG_BG)),
            Span::styled(
                format!(" Recent ({}) ", app.bookmarks.recent.len()),
                tab_style(tab == ConnTab::Recent),
            ),
            Span::styled(
                "   Tab switches",
                Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
            ),
        ])),
        split[0],
    );

    if entries.is_empty() {
        let message = match tab {
            ConnTab::Saved => "No saved locations yet - press 'a' to add one.",
            ConnTab::Recent => "Nowhere visited yet.",
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)),
            split[1],
        );
    } else {
        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let text = format!(
                    " {:<18} {}",
                    fit(&location.name, 18),
                    fit(
                        &location.summary(),
                        split[1].width.saturating_sub(21) as usize
                    )
                );
                let style = if index == cursor {
                    Style::default()
                        .bg(theme::CURSOR_BG)
                        .fg(theme::CURSOR_FG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG)
                };
                ListItem::new(Line::from(Span::styled(text, style)))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(cursor));
        frame.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme::DIALOG_BG)),
            split[1],
            &mut state,
        );
    }

    let hints = match tab {
        ConnTab::Saved => "Enter go  a add  c add cwd  u unmount  d delete  Esc close",
        ConnTab::Recent => "Enter go  s save  d forget  C forget all  Esc close",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            hints,
            Style::default().bg(theme::DIALOG_BG).fg(theme::TITLE_FG),
        )),
        split[2],
    );
}

/// The key list. Public so the key handler can keep the scroll in range
/// without having to know how it is laid out.
pub const HELP: &[(&str, &str)] = &[
    ("Tab", "switch panel"),
    ("Up / Down / PgUp / PgDn", "move cursor"),
    ("Home / End", "first / last entry"),
    ("Enter / Right", "enter directory, or view a file"),
    ("Backspace / Left", "go to parent directory"),
    ("Space / Insert", "mark entry"),
    ("*", "invert the marks"),
    ("+ / -", "mark / unmark by mask"),
    ("Ctrl-A", "mark everything"),
    ("F1", "this help"),
    ("F2", "rename"),
    ("Shift-F2", "rename the whole selection at once"),
    ("F3", "view file"),
    ("F4", "edit ($EDITOR)"),
    ("Shift-F4", "edit as administrator"),
    ("F5", "copy to directory"),
    ("F6", "move to directory"),
    ("F7", "create directory"),
    ("Alt-F7 / Ctrl-F", "find files"),
    ("F8 / Delete", "move to the trash"),
    ("Shift-F8 / Shift-Del", "delete for good"),
    ("Alt-Enter", "properties and permissions"),
    ("Ctrl-P", "open with..."),
    ("Ctrl-E", "a shell as administrator"),
    ("F9", "cycle sort order"),
    ("Ctrl-O", "shell screen"),
    ("F10", "quit"),
    ("", ""),
    ("Ctrl-T", "another tab, here"),
    ("Ctrl-W", "close this tab"),
    ("Alt-W", "close the other tabs"),
    ("Ctrl-PgUp / PgDn", "walk the tabs"),
    ("Shift-F6", "send this tab to the other pane"),
    ("", ""),
    ("Alt-C", "mark what differs between the panes"),
    ("Alt-D", "compare two files, line by line"),
    ("Alt-U", "find files that are the same file twice"),
    ("Alt-S", "synchronize the two directories"),
    ("Ctrl-J", "what was done - the account"),
    ("Alt-T", "directory tree (Enter opens, +/- expand)"),
    ("F11 / Ctrl-B", "network locations & bookmarks"),
    ("Ctrl-D", "bookmark the current directory"),
    ("Ctrl-H", "toggle hidden files"),
    ("Ctrl-R", "reload both panels"),
    ("Ctrl-U", "swap panels"),
];

fn draw_help(frame: &mut Frame, area: Rect, scroll: usize) {
    // The list outgrew a short terminal, and `centered` clamps to what there
    // is - so a naive single column would quietly cut the last rows off, and
    // a key you cannot see in the help is a key that is not there. Two
    // columns where the window is wide enough, and what still does not fit
    // scrolls.
    const KEY_COL: usize = 23;
    let wide = area.width >= 94;
    let columns = if wide { 2 } else { 1 };
    let per_column = HELP.len().div_ceil(columns);
    let rect = centered(if wide { 96 } else { 64 }, per_column as u16 + 2, area);
    frame.render_widget(Clear, rect);

    let rows = rect.height.saturating_sub(2) as usize;
    let hidden = per_column.saturating_sub(rows);
    let scroll = scroll.min(hidden);
    let title = if hidden > 0 {
        "Help  (Up/Down to scroll, Esc closes)"
    } else {
        "Help  (Esc closes)"
    };

    let block = dialog_block(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let key_style = Style::default()
        .bg(theme::DIALOG_BG)
        .fg(theme::DIR_FG)
        .add_modifier(Modifier::BOLD);
    let what_style = Style::default().bg(theme::DIALOG_BG).fg(theme::FILE_FG);
    let cell = |index: usize, width: usize| -> Vec<Span<'static>> {
        match HELP.get(index) {
            Some((keys, what)) => vec![
                Span::styled(format!("{:<KEY_COL$}", fit(keys, KEY_COL - 1)), key_style),
                Span::styled(format!("{:<w$}", fit(what, width), w = width), what_style),
            ],
            None => vec![Span::styled(" ".repeat(KEY_COL + width), what_style)],
        }
    };

    // Down the first column and then down the second, which is how a
    // reference list is read.
    let text_width = (inner.width as usize / columns).saturating_sub(KEY_COL);
    let body: Vec<Line> = (scroll..per_column.min(scroll + rows))
        .map(|row| {
            let mut spans = cell(row, text_width);
            if columns == 2 {
                spans.extend(cell(row + per_column, text_width));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(body).style(theme::base()), inner);
}

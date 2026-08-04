// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Render the terminal view into text, so what the README shows is what the
//! program draws.
//!
//! A hand-drawn illustration in a README is a picture of whatever the program
//! looked like on the day somebody typed it, and the README in this
//! repository had already fallen two features behind. This one is generated
//! from the real drawing code and checked against the file, which is the only
//! way a picture of a user interface stays true.

use std::io::Write;

/// Draw the panels into a buffer and return them as lines of text.
fn render(width: u16, height: u16) -> Vec<String> {
    let root = tempfile::tempdir().expect("a temporary directory");
    let here = root.path().join("core");
    std::fs::create_dir_all(here.join("archive")).unwrap();
    for (name, size) in [
        ("entry.rs", 5_650usize),
        ("fsops.rs", 9_290),
        ("panel.rs", 28_800),
        ("tree.rs", 14_600),
    ] {
        std::fs::write(here.join(name), vec![b'x'; size]).unwrap();
    }

    let mut app = lostc::app::App::new(here.clone(), root.path().to_path_buf());
    // Two files marked and something half-typed: the empty case is the one
    // that says least about the program.
    app.active_panel_mut().cursor_to(2);
    app.active_panel_mut().toggle_mark();
    app.active_panel_mut().toggle_mark();
    app.command.push_str("cargo test");

    drawn(&app, width, height)
}

/// Draw an app that is already set up.
fn drawn(app: &lostc::app::App, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
    terminal.draw(|frame| lostc::ui::draw(frame, app)).unwrap();

    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let line: String = (0..width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect();
            line.trim_end().to_string()
        })
        .collect()
}

/// One panel, then two, then one again - and it is drawn each time.
///
/// The field says which panels are showing; only drawing says what the reader
/// sees. This is the test that would catch a fold leaving the window on a
/// directory nobody asked for.
#[test]
fn folding_the_second_panel_leaves_the_one_you_were_reading() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let ours = root.path().join("ours");
    let theirs = root.path().join("theirs");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::create_dir_all(&theirs).unwrap();

    let mut app = lostc::app::App::detached(ours.clone(), theirs.clone());
    let headings = |lines: &[String]| lines[0].matches('\u{250c}').count();

    // As it opens: one panel, the whole width, on the left directory.
    // Wide enough that a heading shows the whole path in either half:
    // a truncated one would say nothing about which directory it is.
    let one = drawn(&app, 160, 14);
    assert_eq!(headings(&one), 1, "one panel means one heading");
    assert!(one[0].contains("ours"), "{}", one[0]);
    assert!(!one[0].contains("theirs"), "{}", one[0]);

    // Tab asks for the second, and both are drawn.
    app.switch_panel();
    let two = drawn(&app, 160, 14);
    assert_eq!(headings(&two), 2);
    assert!(
        two[0].contains("ours") && two[0].contains("theirs"),
        "{}",
        two[0]
    );

    // F12 folds one away, and the one left is the one being read - not the
    // left panel by default, which would move the reader somewhere else.
    app.toggle_second_panel();
    let back = drawn(&app, 160, 14);
    assert_eq!(headings(&back), 1);
    assert!(back[0].contains("theirs"), "{}", back[0]);
}

/// Replace what changes between one run and the next.
///
/// A timestamp and a temporary directory are true and different every time,
/// so comparing them would make this test fail tomorrow for no reason.
/// Everything else - the borders, the columns, the sizes, the marks, the key
/// bar, the shape of the status line - is fixed, and is what the picture is
/// actually promising.
fn steady(line: &str) -> String {
    let bytes: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        // dd.mm.yy hh:mm
        let looks_like_a_date = i + 14 <= bytes.len()
            && bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2] == '.'
            && bytes[i + 5] == '.'
            && bytes[i + 8] == ' '
            && bytes[i + 11] == ':';
        if looks_like_a_date {
            out.push_str("DD.MM.YY HH:MM");
            i += 14;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }

    // The path in a heading or a prompt is wherever the reader happens to be.
    if let Some(cut) = out.find("> ") {
        if out.ends_with('\u{2588}') {
            return format!("PATH> {}", &out[cut + 2..]);
        }
    }
    if out.starts_with('┌') {
        return "HEADING".to_string();
    }
    out
}

#[test]
fn the_readme_shows_what_the_program_draws() {
    let drawn: Vec<String> = render(100, 14).iter().map(|l| steady(l)).collect();

    // Written out whole, so the README can be brought up to date by pasting
    // rather than by transcribing from a failure message.
    let out = std::env::temp_dir().join("lostc-screen.txt");
    if let Ok(mut file) = std::fs::File::create(&out) {
        let _ = writeln!(file, "{}", render(100, 14).join("\n"));
    }

    let readme: Vec<String> =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"))
            .expect("the README")
            .lines()
            .map(steady)
            .collect();

    let missing: Vec<&String> = drawn
        .iter()
        .filter(|line| !line.trim().is_empty() && line.as_str() != "HEADING")
        .filter(|line| !readme.iter().any(|shown| shown == *line))
        .collect();

    assert!(
        missing.is_empty(),
        "the README has fallen behind what the program draws.\n\
         These lines are drawn and are not in README.md:\n{}\n\n\
         The whole screen, as drawn, is in {}",
        missing
            .iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        out.display()
    );
}

#[test]
fn the_shell_screen_says_what_the_shell_can_do() {
    // A shell with no seam to hook runs commands perfectly well and simply
    // never says where it is, so the shared directory quietly does not
    // happen. Without a line saying so, nothing on screen explains why - and
    // on Windows the machine's own answer is one of those shells.
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut app = lostc::app::App::new(root.path().to_path_buf(), root.path().to_path_buf());

    for (program, expected) in [
        ("/bin/bash", "shares this directory"),
        ("cmd.exe", "not recorded"),
    ] {
        app.shell_program = Some(program.to_string());
        app.showing_shell = true;

        let backend = ratatui::backend::TestBackend::new(100, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
        terminal.draw(|frame| lostc::ui::draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let top: String = (0..100)
            .map(|x| buffer.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert!(
            top.contains(expected),
            "the shell screen should say what {program} can do, got: {top}"
        );
        assert!(
            top.contains(&lost_commander_core::shell::program_name(program)),
            "and which shell it is, got: {top}"
        );
    }
}

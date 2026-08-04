// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! lostc-gui - the graphical front-end.
//!
//! A thin shell: window setup, arguments, and handing control to
//! [`lost_commander_egui::GuiApp`]. Everything it does to the filesystem comes
//! from the same library the terminal front-end uses.

use std::path::PathBuf;

use lost_commander_egui::GuiApp;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if matches!(
        args.first().map(String::as_str),
        Some("-h") | Some("--help")
    ) {
        println!(
            "lostc-gui {VERSION} - graphical file manager

USAGE:
    lostc-gui [--grid] [--tree] [--preview] [--history] [LEFT_DIR] [RIGHT_DIR]
    lostc-gui --screenshot FILE.png [LEFT_DIR] [RIGHT_DIR]
    lostc-gui --help | --version

    --grid starts both panes in the icon grid instead of the detail list.
    Each pane also has its own list / grid / tree switch in its header.

    --tree starts the left pane with the directory tree above its files,
    which is XTree's arrangement: walk directories above, tag files below.

    --preview starts the second pane showing what the first one is pointing
    at, which is what F3 does, and opens that pane to do it. It pairs with
    --screenshot: a quick view cannot be checked from a picture of the window
    it is not open in.

    --history starts the second pane showing what has been done to the things
    in the first one's folder - the account read from where you are standing
    rather than by day. It opens the second pane, as --preview does.

    --screenshot renders a few frames, saves a PNG and exits. It is how the
    view is checked without a human at the screen."
        );
        return Ok(());
    }
    if matches!(
        args.first().map(String::as_str),
        Some("-V") | Some("--version")
    ) {
        println!("lostc-gui {VERSION}");
        return Ok(());
    }

    let mut screenshot = None;
    let mut grid = false;
    let mut preview = false;
    let mut tree = false;
    let mut history = false;
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.into_iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--screenshot" => screenshot = rest.next().map(PathBuf::from),
            "--grid" => grid = true,
            "--preview" => preview = true,
            "--tree" => tree = true,
            "--history" => history = true,
            _ => positional.push(arg),
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Resolved rather than canonicalised: on Windows `canonicalize` hands
    // back `\?\C:\src`, which is the right thing to give the operating
    // system and the wrong thing to put in a pane header.
    let settle = |path: PathBuf| lost_commander_core::paths::resolved(&path, &cwd);
    let left = settle(
        positional
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone()),
    );
    let right = settle(
        positional
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone()),
    );

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([720.0, 460.0])
            .with_title("lost-commander"),
        ..Default::default()
    };

    eframe::run_native(
        "lost-commander",
        options,
        Box::new(move |_cc| {
            let mut app = GuiApp::new(left, right);
            if tree {
                // Through `set_view`, which is what actually builds the tree.
                // Assigning the field only changes what is drawn, which is a
                // pane with an empty half above it.
                app.set_view(
                    lost_commander_egui::Side::Left,
                    lost_commander_egui::ViewMode::Tree,
                );
            }
            if preview {
                // The second pane is folded away to begin with, so a flag
                // that puts something *in* it has to open it as well - F3
                // does exactly this when a reader asks the same question.
                app.show_right = true;
                app.right_view = lost_commander_egui::ViewMode::Preview;
                // The cursor starts on `..`, and a quick view of the parent
                // directory is a count of what is in it - true, and not what
                // anyone runs this to look at. Land on the first real entry
                // so there is a file being previewed.
                let panel = app.left.current_mut();
                if let Some(first) = panel.entries.iter().position(|e| !e.is_parent()) {
                    panel.cursor_to(first);
                }
            }
            if history {
                app.show_right = true;
                app.right_view = lost_commander_egui::ViewMode::History;
            }
            if grid {
                // Both panes, since the view is now a per-pane choice.
                app.left_view = lost_commander_egui::ViewMode::Grid;
                app.right_view = lost_commander_egui::ViewMode::Grid;
            }
            app.screenshot_to = screenshot;
            Ok(Box::new(app))
        }),
    )
}

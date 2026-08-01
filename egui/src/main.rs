//! rcmd-gui - the graphical front-end.
//!
//! A thin shell: window setup, arguments, and handing control to
//! [`rust_commander_egui::GuiApp`]. Everything it does to the filesystem comes
//! from the same library the terminal front-end uses.

use std::path::PathBuf;

use rust_commander_egui::GuiApp;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if matches!(
        args.first().map(String::as_str),
        Some("-h") | Some("--help")
    ) {
        println!(
            "rcmd-gui {VERSION} - graphical dual-pane file manager

USAGE:
    rcmd-gui [--grid] [LEFT_DIR] [RIGHT_DIR]
    rcmd-gui --screenshot FILE.png [LEFT_DIR] [RIGHT_DIR]
    rcmd-gui --help | --version

    --grid starts both panes in the icon grid instead of the detail list.
    Each pane also has its own list / grid / tree switch in its header.

    --screenshot renders a few frames, saves a PNG and exits. It is how the
    view is checked without a human at the screen."
        );
        return Ok(());
    }
    if matches!(
        args.first().map(String::as_str),
        Some("-V") | Some("--version")
    ) {
        println!("rcmd-gui {VERSION}");
        return Ok(());
    }

    let mut screenshot = None;
    let mut grid = false;
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.into_iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--screenshot" => screenshot = rest.next().map(PathBuf::from),
            "--grid" => grid = true,
            _ => positional.push(arg),
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let left = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    let right = positional.get(1).map(PathBuf::from).unwrap_or(cwd);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([720.0, 460.0])
            .with_title("rust-commander"),
        ..Default::default()
    };

    eframe::run_native(
        "rust-commander",
        options,
        Box::new(move |_cc| {
            let mut app = GuiApp::new(left, right);
            if grid {
                // Both panes, since the view is now a per-pane choice.
                app.left_view = rust_commander_egui::ViewMode::Grid;
                app.right_view = rust_commander_egui::ViewMode::Grid;
            }
            app.screenshot_to = screenshot;
            Ok(Box::new(app))
        }),
    )
}

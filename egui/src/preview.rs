//! Loading and drawing a quick view.
//!
//! Loading happens on a worker thread. Reading a quarter of a megabyte, or
//! decoding a photograph, or waiting on a thumbnailer process, are all far too
//! slow to do between frames - and the last of those spawns a program, which
//! is not something to do on the thread painting the window.
//!
//! What the worker returns is already pixels or already lines, so the frame
//! that receives it only has to upload a texture.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, FontId, Rect, RichText, Vec2};

use rust_commander_core::entry::human_size;
use rust_commander_core::mount::Platform;
use rust_commander_core::preview::{self, Kind, Thumbnailer};
use rust_commander_core::textindex::LineIndex;

use super::theme;

/// The longest edge a system thumbnail is asked for.
const THUMBNAIL_SIZE: u32 = 512;
/// The longest edge kept in memory after decoding. A 6000px photograph would
/// otherwise cost 144 MB as RGBA for a panel a few hundred pixels wide.
const MAX_EDGE: u32 = 1600;

/// What a finished load produced.
pub enum Loaded {
    /// Where the lines are, not the lines themselves - so the file's size
    /// stops mattering. Shared so the drawing code can hold it without
    /// borrowing the whole `Ready`.
    Text(Arc<LineIndex>),
    Image {
        rgba: Vec<u8>,
        size: [usize; 2],
        /// True when the picture came from the operating system rather than
        /// from our own decoder, which the panel says out loud.
        from_system: bool,
        /// The real pixel size, before any downscale.
        original: [u32; 2],
    },
    Directory {
        entries: usize,
    },
    /// Not text and not a picture: shown as bytes.
    ///
    /// Carries where the file is and how big, not the file - the dump reads
    /// only the rows on screen, so a huge one costs no more than a small one.
    Bytes(rust_commander_core::hex::Dump),
    /// Nothing to show. The panel falls back to the facts about the file.
    Nothing(&'static str),
    Error(String),
}

/// A load in progress.
pub struct PreviewJob {
    pub path: PathBuf,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Loaded>>>,
}

impl PreviewJob {
    /// Start loading `path` in the background.
    pub fn spawn(path: PathBuf, is_dir: bool) -> PreviewJob {
        let done = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));

        let worker_done = Arc::clone(&done);
        let worker_result = Arc::clone(&result);
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let loaded = load(&worker_path, is_dir);
            *worker_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
            worker_done.store(true, Ordering::Release);
        });

        PreviewJob { path, done, result }
    }

    pub fn is_finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn take(&mut self) -> Option<Loaded> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Do the actual work. Runs on the worker thread.
fn load(path: &Path, is_dir: bool) -> Loaded {
    let loaded = classified(path, is_dir);
    // Whatever nothing can draw is still bytes, and bytes are always
    // something true to show - which beats "no preview available" for the one
    // question a binary can actually answer. One place rather than at the end
    // of each branch, because every branch that gives up gives up the same way.
    match loaded {
        Loaded::Nothing(_) if !is_dir => match rust_commander_core::hex::Dump::open(path) {
            Ok(dump) => Loaded::Bytes(dump),
            Err(e) => Loaded::Error(e.to_string()),
        },
        drawable => drawable,
    }
}

fn classified(path: &Path, is_dir: bool) -> Loaded {
    match preview::classify(path, is_dir) {
        Kind::Directory => match std::fs::read_dir(path) {
            Ok(entries) => Loaded::Directory {
                entries: entries.count(),
            },
            Err(e) => Loaded::Error(e.to_string()),
        },
        Kind::Text => load_text(path),
        Kind::Image => match std::fs::read(path) {
            // A decode failure is not an error worth shouting about - the
            // system may still know how to draw it.
            Ok(bytes) => match decode(&bytes, false) {
                Some(image) => image,
                None => system_thumbnail(path),
            },
            Err(e) => Loaded::Error(e.to_string()),
        },
        Kind::System => system_thumbnail(path),
    }
}

fn load_text(path: &Path) -> Loaded {
    use std::io::Read;

    // Sniff the head before committing to a whole-file scan. An extensionless
    // file that turns out to be a binary is common enough - every compiled
    // program is one - and indexing its "lines" would be pointless work.
    let head = match std::fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            if let Err(e) = file.take(8192).read_to_end(&mut bytes) {
                return Loaded::Error(e.to_string());
            }
            bytes
        }
        Err(e) => return Loaded::Error(e.to_string()),
    };
    if !preview::looks_like_text(&head) {
        return system_thumbnail(path);
    }

    match LineIndex::build(path) {
        Ok(index) => Loaded::Text(Arc::new(index)),
        Err(e) => Loaded::Error(e.to_string()),
    }
}

/// Decode image bytes, downscaling anything enormous.
fn decode(bytes: &[u8], from_system: bool) -> Option<Loaded> {
    let image = image::load_from_memory(bytes).ok()?;
    let original = [image.width(), image.height()];

    let longest = original[0].max(original[1]);
    let image = if longest > MAX_EDGE {
        let scale = MAX_EDGE as f32 / longest as f32;
        image.resize(
            (original[0] as f32 * scale) as u32,
            (original[1] as f32 * scale) as u32,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };

    let rgba = image.to_rgba8();
    Some(Loaded::Image {
        size: [rgba.width() as usize, rgba.height() as usize],
        rgba: rgba.into_raw(),
        from_system,
        original,
    })
}

/// Ask the operating system to draw the file, and decode what it produces.
fn system_thumbnail(path: &Path) -> Loaded {
    let platform = Platform::current();
    let thumbnailers = if platform == Platform::Linux {
        preview::load_thumbnailers(&preview::thumbnailer_dirs())
    } else {
        Vec::new()
    };
    run_thumbnailer(path, platform, &thumbnailers)
}

/// The half that can be pointed at a thumbnailer of the test's choosing.
fn run_thumbnailer(path: &Path, platform: Platform, thumbnailers: &[Thumbnailer]) -> Loaded {
    let Ok(scratch) = tempdir() else {
        return Loaded::Nothing("no preview available");
    };
    let Some(command) = preview::thumbnail_command(
        platform,
        thumbnailers,
        path,
        &scratch,
        THUMBNAIL_SIZE,
        &preview::on_disk,
    ) else {
        let _ = std::fs::remove_dir_all(&scratch);
        return Loaded::Nothing("no preview available");
    };

    let ran = std::process::Command::new(&command.program)
        .args(&command.args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let loaded = match ran {
        Ok(status) if status.success() => match std::fs::read(&command.output) {
            Ok(bytes) => decode(&bytes, true).unwrap_or(Loaded::Nothing("no preview available")),
            Err(_) => Loaded::Nothing("no preview available"),
        },
        // A missing thumbnailer binary is the ordinary case on a machine that
        // has none, not something to report as a failure.
        _ => Loaded::Nothing("no preview available"),
    };

    let _ = std::fs::remove_dir_all(&scratch);
    loaded
}

/// A scratch directory for a thumbnailer to write into.
fn tempdir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir().join("rcmd-preview");
    std::fs::create_dir_all(&base)?;
    // The process id and a counter keep two panels from colliding.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!("{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// How the preview is being looked at.
///
/// Kept by the application rather than by the preview, so a font size chosen
/// once survives moving the cursor. Zoom and pan are reset per picture, since
/// a new photograph should arrive fitted rather than halfway off the pane.
#[derive(Debug, Clone, Copy)]
pub struct PreviewView {
    /// Monospace point size for text.
    pub font: f32,
    /// Multiplier on the fitted size. 1.0 is "fits the pane".
    pub zoom: f32,
    /// How far the picture has been dragged from centred.
    pub pan: Vec2,
}

impl Default for PreviewView {
    fn default() -> Self {
        PreviewView {
            font: 11.5,
            zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

impl PreviewView {
    /// Back to a fitted, centred picture.
    pub fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }
}

pub const MIN_FONT: f32 = 7.0;
pub const MAX_FONT: f32 = 40.0;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 40.0;

/// Lines either side of the visible range to fetch, so that a small scroll
/// does not mean another seek and read.
const OVERSCAN: usize = 96;

/// How much of a zoom one notch of the wheel is worth. A notch is about 53
/// points, so this makes it a step of roughly 8% - fourteen of them triple
/// the size rather than hitting the ceiling, which is what the first attempt
/// did.
const WHEEL_ZOOM: f32 = 0.0015;

/// What a wheel movement and a pinch together ask for, as a multiplier.
///
/// Both are needed: egui takes ctrl-scroll out of the scroll delta and hands
/// it back as a zoom gesture, so reading only the scroll misses every
/// ctrl-wheel - which is precisely how the first version of the text zoom
/// managed to do nothing at all.
pub fn zoom_gesture(scroll: f32, pinch: f32) -> f32 {
    pinch * (scroll * WHEEL_ZOOM).exp()
}

/// A loaded preview, plus the texture once it has been uploaded.
pub struct Ready {
    pub path: PathBuf,
    pub loaded: Loaded,
    pub texture: Option<egui::TextureHandle>,
    /// The last window of lines read, so scrolling does not re-read the file
    /// on every frame.
    window: Option<Window>,
}

impl Ready {
    pub fn new(path: PathBuf, loaded: Loaded) -> Ready {
        Ready {
            path,
            loaded,
            texture: None,
            window: None,
        }
    }

    /// Upload the pixels, once, on the frame after they arrive.
    pub fn ensure_texture(&mut self, ctx: &egui::Context) {
        if self.texture.is_some() {
            return;
        }
        let Loaded::Image { rgba, size, .. } = &self.loaded else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied(*size, rgba);
        self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
    }
}

/// How big a picture should be drawn to fit `area` without distorting it.
///
/// Never enlarged past its own size: a 16-pixel icon blown up to fill a panel
/// is a worse answer than a small, sharp one.
pub fn fitted(image: Vec2, area: Vec2) -> Vec2 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return Vec2::ZERO;
    }
    let scale = (area.x / image.x).min(area.y / image.y).min(1.0);
    Vec2::new(image.x * scale, image.y * scale)
}

/// Draw a checkerboard, so transparency reads as transparency rather than as
/// whatever colour the panel happens to be.
fn checkerboard(painter: &egui::Painter, rect: Rect) {
    const SQUARE: f32 = 8.0;
    painter.rect_filled(rect, 0, Color32::from_rgb(0x20, 0x24, 0x2B));
    let mut y = rect.min.y;
    let mut row = 0;
    while y < rect.max.y {
        let mut x = rect.min.x + if row % 2 == 0 { 0.0 } else { SQUARE };
        while x < rect.max.x {
            let square = Rect::from_min_size(egui::pos2(x, y), Vec2::splat(SQUARE)).intersect(rect);
            painter.rect_filled(square, 0, Color32::from_rgb(0x28, 0x2D, 0x36));
            x += SQUARE * 2.0;
        }
        y += SQUARE;
        row += 1;
    }
}

/// A window of lines and the range they cover.
pub type Window = (std::ops::Range<usize>, Vec<String>);

/// Lines for `range`: from the cache when it covers them, otherwise read.
///
/// Returns the lines, and a new cache when a read happened. Separated out
/// because the cache-hit arithmetic is exactly the sort of off-by-one that
/// looks right and shows the wrong part of the file.
pub fn lines_for(
    index: &LineIndex,
    cache: &Option<Window>,
    range: std::ops::Range<usize>,
) -> (Vec<String>, Option<Window>) {
    if let Some((have, lines)) = cache {
        if have.start <= range.start && have.end >= range.end {
            let from = range.start - have.start;
            let to = range.end - have.start;
            return (lines[from..to].to_vec(), None);
        }
    }

    let start = range.start.saturating_sub(OVERSCAN);
    let count = (range.end - range.start) + OVERSCAN * 2;
    let lines = index.read(start, count).unwrap_or_default();
    let have = start..start + lines.len();

    let from = (range.start - start).min(lines.len());
    let to = (range.end - start).min(lines.len());
    (lines[from..to].to_vec(), Some((have, lines)))
}

/// Keep a dragged picture from being flung out of sight.
///
/// A third of it must stay inside the pane, which is enough to grab hold of
/// and drag back.
pub fn clamp_pan(pan: Vec2, drawn: Vec2, canvas: Vec2) -> Vec2 {
    let limit = |drawn: f32, canvas: f32| ((drawn + canvas) * 0.5 - canvas / 3.0).max(0.0);
    Vec2::new(
        pan.x
            .clamp(-limit(drawn.x, canvas.x), limit(drawn.x, canvas.x)),
        pan.y
            .clamp(-limit(drawn.y, canvas.y), limit(drawn.y, canvas.y)),
    )
}

/// The zoom that draws a picture at one screen pixel per image pixel.
pub fn one_to_one(original: [u32; 2], fitted: Vec2) -> f32 {
    if fitted.x <= 0.0 {
        return 1.0;
    }
    (original[0] as f32 / fitted.x).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Something the preview wants the application to do, which it cannot do
/// itself because it has no access to the panes or the dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Open the picture being previewed for turning, cropping and resizing.
    EditImage,
}

/// Draw the loaded preview into the pane.
pub fn draw(
    ui: &mut egui::Ui,
    ready: &mut Ready,
    view: &mut PreviewView,
    entry: Option<&rust_commander_core::entry::Entry>,
) -> Option<Request> {
    let mut asked = None;
    match &ready.loaded {
        Loaded::Text(index) => {
            let index = Arc::clone(index);
            let total = index.lines();

            // Ctrl-wheel resizes the text, as it does in every editor and
            // browser. Plain wheel is left to scrolling.
            let (hovered, pinch) = ui.input(|i| {
                (
                    ui.max_rect()
                        .contains(i.pointer.hover_pos().unwrap_or_default()),
                    i.zoom_delta(),
                )
            });
            if hovered && (pinch - 1.0).abs() > 0.0001 {
                view.font = (view.font * pinch).clamp(MIN_FONT, MAX_FONT);
            }

            let font = FontId::monospace(view.font);
            let row_height = ui.fonts(|f| f.row_height(&font)) + 1.0;

            // The cache is taken out so the closure can use it without
            // borrowing `ready`, and put back with whatever the read produced.
            let cache = ready.window.take();
            let mut fetched = None;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, row_height, total, |ui, range| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let (lines, new_cache) = lines_for(&index, &cache, range);
                    fetched = new_cache;
                    for line in lines {
                        // Lines run rather than wrap, and the view scrolls
                        // sideways to follow. Wrapping would fold a log or a
                        // table back on itself and lose the shape that made
                        // it worth looking at.
                        ui.add(
                            egui::Label::new(
                                RichText::new(line).font(font.clone()).color(theme::text()),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    }
                });
            ready.window = fetched.or(cache);
        }
        Loaded::Bytes(dump) => {
            let dump = dump.clone();
            let font = FontId::monospace(view.font);
            let row_height = ui.fonts(|f| f.row_height(&font)) + 1.0;
            let width = dump.offset_width();
            let total = dump.rows() as usize;

            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, row_height, total.max(1), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if total == 0 {
                        ui.label(
                            RichText::new("(empty)")
                                .font(font.clone())
                                .color(theme::text_faint()),
                        );
                        return;
                    }
                    // Only the rows on screen are read, which is what makes
                    // this cost the same on a file of any size.
                    let rows = dump
                        .read(range.start as u64, range.len())
                        .unwrap_or_default();
                    for row in &rows {
                        ui.add(
                            egui::Label::new(
                                RichText::new(rust_commander_core::hex::line(row, width))
                                    .font(font.clone())
                                    .color(theme::text()),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    }
                });
        }
        Loaded::Image {
            size,
            from_system,
            original,
            ..
        } => {
            let area = ui.available_rect_before_wrap();
            let caption = Rect::from_min_max(egui::pos2(area.min.x, area.max.y - 20.0), area.max);
            let canvas = Rect::from_min_max(area.min, egui::pos2(area.max.x, caption.min.y));
            let response = ui.allocate_rect(canvas, egui::Sense::click_and_drag());

            let base = fitted(Vec2::new(size[0] as f32, size[1] as f32), canvas.size());

            // The wheel zooms about the pointer, so the thing being looked at
            // stays under it rather than sliding away. There is nothing to
            // scroll in a picture, so the plain wheel zooms too.
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let gesture = zoom_gesture(scroll, pinch);
            if response.hovered() && (gesture - 1.0).abs() > 0.0001 {
                let zoomed = (view.zoom * gesture).clamp(MIN_ZOOM, MAX_ZOOM);
                let ratio = zoomed / view.zoom;
                if let Some(pointer) = response.hover_pos() {
                    let from_centre = pointer - (canvas.center() + view.pan);
                    view.pan += from_centre * (1.0 - ratio);
                }
                view.zoom = zoomed;
            }
            if response.dragged() {
                view.pan += response.drag_delta();
            }
            // Double-click switches between fitted and actual size, which is
            // the one zoom level anybody asks for by name.
            if response.double_clicked() {
                if (view.zoom - 1.0).abs() < 0.01 {
                    view.zoom = one_to_one(*original, base);
                } else {
                    view.reset_zoom();
                }
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(if view.zoom > 1.0 {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::Default
                });
            }

            let drawn = base * view.zoom;
            view.pan = clamp_pan(view.pan, drawn, canvas.size());
            let frame = Rect::from_center_size(canvas.center() + view.pan, drawn);

            if let Some(texture) = &ready.texture {
                let painter = ui.painter_at(canvas);
                checkerboard(&painter, frame.intersect(canvas));
                painter.image(
                    texture.id(),
                    frame,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );

                let scale = drawn.x / original[0] as f32;
                let label = format!(
                    "{} x {}  -  {:.0}%{}",
                    original[0],
                    original[1],
                    scale * 100.0,
                    if *from_system {
                        "  -  drawn by the system"
                    } else {
                        ""
                    }
                );
                ui.painter().text(
                    caption.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(11.0),
                    theme::text_faint(),
                );

                // Offered only where we decoded it ourselves. A RAW or a HEIC
                // arrives here as a thumbnail the system drew, and editing a
                // thumbnail and saving it over the original would swap a
                // photograph for a postage stamp.
                if !*from_system {
                    let button = Rect::from_min_max(
                        egui::pos2(caption.max.x - 52.0, caption.min.y + 1.0),
                        caption.max,
                    );
                    if ui
                        .put(button, egui::Button::new(RichText::new("Edit").size(10.5)))
                        .on_hover_text("Turn, crop or resize this picture (Alt-I)")
                        .clicked()
                    {
                        asked = Some(Request::EditImage);
                    }
                }
            }
        }
        Loaded::Directory { entries } => {
            facts(ui, entry, &format!("{entries} items"));
        }
        Loaded::Nothing(why) => {
            facts(ui, entry, why);
        }
        Loaded::Error(message) => {
            ui.add_space(12.0);
            ui.label(RichText::new(message).size(11.5).color(theme::danger()));
        }
    }
    asked
}

/// The fallback: what we know about the file, centred, with its icon.
fn facts(ui: &mut egui::Ui, entry: Option<&rust_commander_core::entry::Entry>, note: &str) {
    let Some(entry) = entry else { return };
    let area = ui.available_rect_before_wrap();
    let centre = area.center();

    let icon = Rect::from_center_size(egui::pos2(centre.x, centre.y - 48.0), Vec2::splat(56.0));
    super::icons::draw(ui.painter(), icon, super::icons::classify(entry), false);

    let painter = ui.painter();
    painter.text(
        egui::pos2(centre.x, centre.y + 4.0),
        egui::Align2::CENTER_CENTER,
        &entry.name,
        FontId::proportional(13.0),
        theme::text(),
    );
    if !entry.is_dir() {
        painter.text(
            egui::pos2(centre.x, centre.y + 26.0),
            egui::Align2::CENTER_CENTER,
            human_size(entry.size),
            FontId::proportional(11.5),
            theme::text_dim(),
        );
    }
    painter.text(
        egui::pos2(centre.x, centre.y + 46.0),
        egui::Align2::CENTER_CENTER,
        note,
        FontId::proportional(11.0),
        theme::text_faint(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(job: &PreviewJob) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !job.is_finished() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(job.is_finished(), "the load never finished");
    }

    #[test]
    fn a_picture_is_scaled_down_to_fit_but_never_up() {
        // Taller than the space: height decides.
        let drawn = fitted(Vec2::new(100.0, 200.0), Vec2::new(400.0, 100.0));
        assert_eq!(drawn, Vec2::new(50.0, 100.0));
        // Wider than the space: width decides.
        let drawn = fitted(Vec2::new(400.0, 100.0), Vec2::new(200.0, 400.0));
        assert_eq!(drawn, Vec2::new(200.0, 50.0));
        // A small icon is left alone rather than blown up into mush.
        assert_eq!(
            fitted(Vec2::new(16.0, 16.0), Vec2::new(800.0, 600.0)),
            Vec2::new(16.0, 16.0)
        );
        // Degenerate input does not divide by zero.
        assert_eq!(fitted(Vec2::ZERO, Vec2::new(10.0, 10.0)), Vec2::ZERO);
    }

    #[test]
    fn a_text_file_comes_back_as_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "alpha\nbeta\tindented\ngamma\n").unwrap();

        let mut job = PreviewJob::spawn(file, false);
        wait(&job);
        match job.take().expect("a result") {
            Loaded::Text(index) => {
                assert_eq!(index.lines(), 3);
                let lines = index.read(0, 3).unwrap();
                assert_eq!(lines[0], "alpha");
                assert_eq!(lines[1], "beta    indented", "tabs are expanded");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn a_file_far_bigger_than_memory_would_like_is_still_readable_to_the_end() {
        // There is no cap any more: the index knows where every line is, and
        // the view reads the window it needs.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.log");
        let body: String = (0..40_000)
            .map(|i| format!("row {i:06} of the file\n"))
            .collect();
        assert!(body.len() > 512 * 1024, "want a file past the old cap");
        std::fs::write(&file, &body).unwrap();

        let mut job = PreviewJob::spawn(file, false);
        wait(&job);
        match job.take().expect("a result") {
            Loaded::Text(index) => {
                assert_eq!(index.lines(), 40_000);
                assert!(!index.partial());
                // The last line, which the old capped reader never reached.
                assert_eq!(index.read(39_999, 1).unwrap(), ["row 039999 of the file"]);
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn a_window_is_cached_so_scrolling_does_not_re_read_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rows.txt");
        let body: String = (0..2000).map(|i| format!("row {i:04}\n")).collect();
        std::fs::write(&file, &body).unwrap();
        let index = LineIndex::build(&file).unwrap();

        // First look: nothing cached, so it reads and offers a cache back.
        let (lines, cache) = lines_for(&index, &None, 500..510);
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "row 0500");
        let cache = cache.expect("a read should hand back a cache");
        // The overscan means the cache covers more than was asked for.
        assert!(cache.0.start < 500 && cache.0.end > 510);

        // A small scroll inside the cached window is served without a read.
        let cache = Some(cache);
        let (lines, fresh) = lines_for(&index, &cache, 505..515);
        assert_eq!(
            lines[0], "row 0505",
            "the cache must be sliced at the right offset"
        );
        assert!(fresh.is_none(), "no read was needed");

        // A jump outside it reads again.
        let (lines, fresh) = lines_for(&index, &cache, 1500..1510);
        assert_eq!(lines[0], "row 1500");
        assert!(fresh.is_some());

        // Near the end, where the window runs out before the request does.
        let (lines, _) = lines_for(&index, &None, 1995..2005);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[4], "row 1999");
    }

    #[test]
    fn a_zoom_gesture_reads_both_the_wheel_and_the_pinch() {
        // egui takes ctrl-scroll out of the scroll delta and hands it back as
        // a zoom gesture, so reading only the scroll misses every ctrl-wheel.
        // That is exactly what the first text zoom did: nothing.
        assert!((zoom_gesture(0.0, 1.25) - 1.25).abs() < 0.001);

        // A wheel notch is about 53 points, and should be a modest step.
        let notch = zoom_gesture(53.0, 1.0);
        assert!(
            (1.05..1.15).contains(&notch),
            "one notch was {notch}x, which is not a modest step"
        );
        // Fourteen of them should roughly triple, not hit the ceiling - the
        // first attempt reached 4000% and clamped there.
        let fourteen = notch.powi(14);
        assert!(
            (2.0..6.0).contains(&fourteen),
            "fourteen notches came to {fourteen}x"
        );

        // Nothing happening is exactly 1.0, so it can be tested for.
        assert_eq!(zoom_gesture(0.0, 1.0), 1.0);
        // Scrolling the other way shrinks.
        assert!(zoom_gesture(-53.0, 1.0) < 1.0);
    }

    #[test]
    fn zoom_and_pan_stay_within_reach() {
        // A picture dragged off the pane could never be dragged back.
        let canvas = Vec2::new(400.0, 300.0);
        let drawn = Vec2::new(1200.0, 900.0);
        let far = clamp_pan(Vec2::new(100_000.0, -100_000.0), drawn, canvas);
        assert!(far.x < (drawn.x + canvas.x) * 0.5);
        assert!(far.y.abs() < (drawn.y + canvas.y) * 0.5);
        // A modest pan is left alone.
        let small = Vec2::new(20.0, -15.0);
        assert_eq!(clamp_pan(small, drawn, canvas), small);

        // One-to-one is the ratio between real pixels and the fitted size.
        assert!((one_to_one([800, 600], Vec2::new(400.0, 300.0)) - 2.0).abs() < 0.001);
        // A picture already smaller than the pane is drawn at 1.0 either way.
        assert!((one_to_one([64, 64], Vec2::new(64.0, 64.0)) - 1.0).abs() < 0.001);
        // Degenerate input does not divide by zero.
        assert_eq!(one_to_one([100, 100], Vec2::ZERO), 1.0);
    }

    #[test]
    fn an_image_is_decoded_to_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("red.png");
        // A 3x2 image, written with the same crate that will read it.
        let mut buffer = image::RgbaImage::new(3, 2);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([255, 0, 0, 255]);
        }
        buffer.save(&file).unwrap();

        let mut job = PreviewJob::spawn(file, false);
        wait(&job);
        match job.take().expect("a result") {
            Loaded::Image {
                size,
                original,
                from_system,
                rgba,
            } => {
                assert_eq!(size, [3, 2]);
                assert_eq!(original, [3, 2]);
                assert!(!from_system, "we decoded this one ourselves");
                assert_eq!(rgba.len(), 3 * 2 * 4);
                assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
            }
            _ => panic!("expected an image"),
        }
    }

    #[test]
    fn an_extensionless_binary_is_shown_as_bytes_rather_than_as_text() {
        // Every compiled program is one of these, and pouring an ELF into a
        // text view is the classic file-manager embarrassment. Nothing can
        // draw it, so it falls through to the dump - which can always show
        // something, and something true.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.out");
        let mut bytes = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0".to_vec();
        bytes.extend((0..200u32).map(|n| (n % 251) as u8));
        std::fs::write(&file, &bytes).unwrap();

        let mut job = PreviewJob::spawn(file.clone(), false);
        wait(&job);
        match job.take() {
            Some(Loaded::Bytes(dump)) => {
                assert_eq!(dump.size, bytes.len() as u64);
                assert_eq!(dump.path, file);
                let first = dump.read(0, 1).unwrap();
                assert_eq!(&first[0].bytes[..4], b"\x7fELF");
            }
            _ => panic!("a binary must not be poured into the text view"),
        }
    }

    #[test]
    fn an_empty_file_that_is_not_text_still_has_a_view() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nothing.bin");
        std::fs::write(&file, b"\0\0\0\0").unwrap();

        let mut job = PreviewJob::spawn(file, false);
        wait(&job);
        assert!(matches!(job.take(), Some(Loaded::Bytes(_))));
    }

    #[test]
    fn a_directory_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("one")).unwrap();
        std::fs::write(dir.path().join("two.txt"), "x").unwrap();

        let mut job = PreviewJob::spawn(dir.path().to_path_buf(), true);
        wait(&job);
        match job.take().expect("a result") {
            Loaded::Directory { entries } => assert_eq!(entries, 2),
            _ => panic!("expected a directory"),
        }
    }

    #[test]
    fn a_file_that_is_not_there_reports_rather_than_hangs() {
        let mut job = PreviewJob::spawn(PathBuf::from("/nowhere/at/all.txt"), false);
        wait(&job);
        assert!(matches!(job.take(), Some(Loaded::Error(_))));
    }

    /// A thumbnailer that really runs, so the whole path can be exercised on
    /// a machine that ships none. Writes a 4x4 PNG wherever it is told.
    fn fake_thumbnailer(dir: &Path) -> Thumbnailer {
        let script = dir.join("fake-thumbnailer");
        std::fs::write(
            &script,
            // $1 is the size, $2 the URI, $3 where the picture goes.
            "#!/bin/sh\ncp \"$FAKE_SOURCE\" \"$3\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // The picture it will produce, prepared once.
        let source = dir.join("source.png");
        let mut buffer = image::RgbaImage::new(4, 4);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([0, 128, 255, 255]);
        }
        buffer.save(&source).unwrap();
        std::env::set_var("FAKE_SOURCE", &source);

        Thumbnailer {
            exec: format!("{} %s %u %o", script.display()),
            try_exec: Some(script.display().to_string()),
            mime_types: vec!["application/pdf".to_string()],
        }
    }

    #[test]
    fn the_system_is_asked_to_draw_what_we_cannot() {
        // A PDF: no decoder of ours touches it. With a thumbnailer registered
        // for it, the whole path runs - expand the Exec line, spawn it, read
        // the PNG back, decode it - and the panel gets a picture.
        if Platform::current() != Platform::Linux {
            eprintln!("freedesktop thumbnailers are a Linux thing - skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let thumbnailer = fake_thumbnailer(dir.path());

        let file = dir.path().join("paper.pdf");
        std::fs::write(&file, b"%PDF-1.4 not really").unwrap();

        match run_thumbnailer(&file, Platform::Linux, std::slice::from_ref(&thumbnailer)) {
            Loaded::Image {
                from_system, size, ..
            } => {
                assert!(from_system, "this could only have come from the system");
                assert_eq!(size, [4, 4]);
            }
            Loaded::Nothing(why) => panic!("the thumbnailer produced nothing: {why}"),
            Loaded::Error(e) => panic!("{e}"),
            _ => panic!("expected an image"),
        }
    }

    #[test]
    fn nothing_is_shown_when_the_system_has_no_answer() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mystery.qqq");
        std::fs::write(&file, b"\0\0\0\0").unwrap();

        // No thumbnailer claims it, so the panel falls back to the facts
        // rather than showing an error the user can do nothing about.
        assert!(matches!(
            run_thumbnailer(&file, Platform::Linux, &[]),
            Loaded::Nothing(_)
        ));
    }

    #[test]
    fn this_machines_own_thumbnailers_are_used_when_they_work() {
        // The end-to-end version, against whatever is really installed. It
        // skips rather than fails where nothing usable is registered - which
        // is this container's situation: librsvg's entry is present but
        // gdk-pixbuf-thumbnailer is not installed.
        let thumbnailers = preview::load_thumbnailers(&preview::thumbnailer_dirs());
        let usable: Vec<&Thumbnailer> = thumbnailers
            .iter()
            .filter(|t| t.handles("image/svg+xml") && t.installed(&preview::on_disk))
            .collect();
        if usable.is_empty() || Platform::current() != Platform::Linux {
            eprintln!(
                "no usable SVG thumbnailer here ({} registered) - skipping",
                thumbnailers.len()
            );
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("circle.svg");
        std::fs::write(
            &file,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
                 <circle cx="32" cy="32" r="30" fill="#4c8dff"/>
               </svg>"##,
        )
        .unwrap();

        let mut job = PreviewJob::spawn(file, false);
        wait(&job);
        match job.take().expect("a result") {
            Loaded::Image { from_system, .. } => assert!(from_system),
            other => {
                let what = match other {
                    Loaded::Nothing(why) => why.to_string(),
                    Loaded::Error(e) => e,
                    _ => "something else".to_string(),
                };
                panic!("a usable thumbnailer produced nothing: {what}");
            }
        }
    }
}

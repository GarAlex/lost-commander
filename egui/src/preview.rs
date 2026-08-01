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

use lost_commander_core::entry::human_size;
use lost_commander_core::markdown;
use lost_commander_core::mount::Platform;
use lost_commander_core::preview::{self, Kind, Thumbnailer};
use lost_commander_core::textindex::LineIndex;

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
    /// A document, parsed but not drawn.
    ///
    /// The parse is the engine's - what counts as a heading, where a lazy
    /// continuation joins a quote, what a reference link resolves to is
    /// CommonMark and has a thousand edge cases. What a heading *looks* like
    /// is this crate's, and it looks different here than it does in the
    /// native Windows window on purpose.
    Markdown(Vec<markdown::Block>),
    /// Not text and not a picture: shown as bytes.
    ///
    /// Carries where the file is and how big, not the file - the dump reads
    /// only the rows on screen, so a huge one costs no more than a small one.
    Bytes(lost_commander_core::hex::Dump),
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
        Loaded::Nothing(_) if !is_dir => match lost_commander_core::hex::Dump::open(path) {
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
        Kind::Text => {
            // Asked by name, which is how a document announces itself. The
            // bytes cannot tell you: markdown is text, and text that happens
            // to contain a `#` is not a heading unless the file claims to be
            // markdown in the first place.
            let named = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if markdown::looks_like_markdown(&named) {
                match std::fs::read_to_string(path) {
                    Ok(source) => return Loaded::Markdown(markdown::parse(&source)),
                    // Unreadable as text - fall through and let the text path
                    // decide what it is instead of refusing outright.
                    Err(_) => return load_text(path),
                }
            }
            load_text(path)
        }
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
    let base = std::env::temp_dir().join("lostc-preview");
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
    entry: Option<&lost_commander_core::entry::Entry>,
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
        Loaded::Markdown(blocks) => {
            // Ctrl-wheel resizes, as in the text view - the same gesture for
            // the same thing, since a reader does not care which of the two
            // they happen to be looking at.
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
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| draw_markdown(ui, blocks, view.font));
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
                                RichText::new(lost_commander_core::hex::line(row, width))
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
fn facts(ui: &mut egui::Ui, entry: Option<&lost_commander_core::entry::Entry>, note: &str) {
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

// ---- drawing a document --------------------------------------------------

/// How much wider than the body text a heading of this level is drawn.
///
/// Six levels compressed into four sizes: past the third, a heading is doing
/// structural work the eye no longer needs a size for, and a document that
/// used all six would otherwise end with headings smaller than its own text.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.9,
        2 => 1.5,
        3 => 1.25,
        _ => 1.1,
    }
}

/// One indent step, in points, for a nested list or a quote.
const INDENT: f32 = 18.0;

/// Draw a parsed document.
///
/// Nothing here decides what the markup *means* - that arrived already
/// decided. What it decides is what a heading looks like, and that is on
/// purpose: this window and the native Windows one draw the same document
/// differently because they are different windows.
fn draw_markdown(ui: &mut egui::Ui, blocks: &[markdown::Block], base: f32) {
    use markdown::Kind;

    ui.spacing_mut().item_spacing.y = base * 0.35;
    for block in blocks {
        let indent = INDENT * block.depth as f32;
        match &block.kind {
            Kind::Heading { level } => {
                // Air above a heading and not below it, so a heading sits
                // with the text it introduces rather than floating between.
                ui.add_space(base * if *level <= 2 { 0.9 } else { 0.5 });
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    draw_runs(ui, block, base * heading_scale(*level), true);
                });
                if *level <= 2 {
                    // A rule under the top two levels, as the rendered
                    // markdown everyone has read does it.
                    ui.add_space(base * 0.15);
                    rule(ui);
                }
            }
            Kind::Paragraph => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    draw_runs(ui, block, base, false);
                });
            }
            Kind::ListItem { .. } => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    // The marker comes from the engine: a front-end that
                    // chose its own bullet, or counted the numbers itself,
                    // would render the same document differently from the
                    // other one - and CommonMark lets a list start at seven.
                    ui.label(
                        RichText::new(format!("{}  ", block.marker()))
                            .size(base)
                            .color(theme::text_dim()),
                    );
                    draw_runs(ui, block, base, false);
                });
            }
            Kind::Code { language } => {
                let frame = egui::Frame::new()
                    .fill(theme::surface_hi())
                    .inner_margin(egui::Margin::same((base * 0.5) as i8))
                    .corner_radius(egui::CornerRadius::same(3));
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    frame.show(ui, |ui| {
                        ui.vertical(|ui| {
                            if let Some(language) = language {
                                ui.label(
                                    RichText::new(language)
                                        .size(base * 0.8)
                                        .color(theme::text_faint()),
                                );
                            }
                            // Kept exactly as written, and not wrapped: code
                            // folded back on itself stops being readable as
                            // code, which is the only reason to show it.
                            for line in block.text().lines() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(line)
                                            .monospace()
                                            .size(base)
                                            .color(theme::text()),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            }
                        });
                    });
                });
            }
            Kind::Quote => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    let (bar, _) =
                        ui.allocate_exact_size(Vec2::new(3.0, base * 1.2), egui::Sense::hover());
                    ui.painter().rect_filled(bar, 1.0, theme::accent_dim());
                    ui.add_space(base * 0.4);
                    draw_runs(ui, block, base, false);
                });
            }
            Kind::Rule => {
                ui.add_space(base * 0.4);
                rule(ui);
                ui.add_space(base * 0.4);
            }
            Kind::TableRow { header } => {
                // The engine says where each cell starts; how wide they are
                // is this view's problem, and equal shares is the honest
                // answer without measuring the whole table first.
                let cells = cells_of(block);
                let count = cells.len().max(1);
                let width = ((ui.available_width() - indent) / count as f32).max(24.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(indent);
                    for cell in cells {
                        // Padded out to the column width rather than merely
                        // offered it: a cell shrinks to its content, so
                        // without this every row starts its columns wherever
                        // the row above happened to end and nothing lines up.
                        let from = ui.cursor().min.x;
                        ui.horizontal_wrapped(|ui| {
                            draw_range(ui, block, cell, base, *header);
                        });
                        let used = ui.cursor().min.x - from;
                        if used < width {
                            ui.add_space(width - used);
                        }
                    }
                });
                if *header {
                    rule(ui);
                }
            }
            Kind::Html => {
                // Said rather than silently dropped. Rendering it would make
                // this a browser, but a reader who cannot see that something
                // was skipped has been told the document is shorter than it
                // is - which is the failure this program does not commit.
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(
                        RichText::new("[HTML, not rendered]")
                            .size(base * 0.85)
                            .italics()
                            .color(theme::text_faint()),
                    )
                    .on_hover_text(block.text());
                });
            }
        }
    }
}

/// The same colour, pushed away from the background.
///
/// Emphasis without a bold face has to come from contrast, and which
/// direction is "more" depends on the scheme: on a dark ground bold text is
/// lighter, on Paper it is darker. Halfway to the limit is enough to read as
/// emphasis without looking like a different colour.
fn stronger(base: Color32) -> Color32 {
    let toward_white = theme::is_dark(&theme::palette());
    let push = |c: u8| {
        if toward_white {
            c.saturating_add((255 - c) / 2)
        } else {
            c / 2
        }
    };
    Color32::from_rgb(push(base.r()), push(base.g()), push(base.b()))
}

/// A hairline across the available width.
fn rule(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::border());
}

/// Where each cell of a table row starts and ends, as ranges into `runs`.
///
/// A row with no offsets at all is one cell: that is what an empty `cells`
/// means, and treating it as no cells would drop the row.
fn cells_of(block: &markdown::Block) -> Vec<std::ops::Range<usize>> {
    if block.cells.is_empty() {
        // Built from an iterator rather than written as `vec![0..n]`, which
        // reads to clippy as a botched `vec![0; n]` - a fair thing to ask
        // about, since the two differ by one character and mean quite
        // different things.
        return std::iter::once(0..block.runs.len()).collect();
    }
    let mut ranges = Vec::with_capacity(block.cells.len());
    for (i, start) in block.cells.iter().enumerate() {
        let end = block.cells.get(i + 1).copied().unwrap_or(block.runs.len());
        let start = (*start).min(block.runs.len());
        ranges.push(start..end.min(block.runs.len()).max(start));
    }
    ranges
}

fn draw_runs(ui: &mut egui::Ui, block: &markdown::Block, size: f32, heading: bool) {
    draw_range(ui, block, 0..block.runs.len(), size, heading);
}

fn draw_range(
    ui: &mut egui::Ui,
    block: &markdown::Block,
    range: std::ops::Range<usize>,
    size: f32,
    heading: bool,
) {
    use markdown::Style;

    // Runs butt against each other - the parser already put the spaces where
    // they belong, and egui's own spacing between widgets would insert more.
    ui.spacing_mut().item_spacing.x = 0.0;
    for run in &block.runs[range] {
        if let Some(image) = &run.image {
            draw_image_placeholder(ui, run, image, size);
            continue;
        }
        // Bold is a colour here, not a weight, and it has to be said
        // explicitly. egui's own `strong()` defers to the palette, and this
        // theme sets `override_text_color` - which wins - so `**bold**` came
        // out looking exactly like the text around it. The default fonts
        // carry no bold face to fall back on either, so contrast is the
        // emphasis: away from the background, whichever way that is.
        let bold = run.style == Style::Strong || run.style == Style::StrongEmphasis;
        // A heading and a link are both the accent: one is the structure of
        // the document and the other is a way out of it, and in a panel this
        // size two accents would be noise rather than information.
        let colour = if heading || run.href.is_some() {
            theme::accent()
        } else if bold {
            stronger(theme::text())
        } else {
            theme::text()
        };

        let mut text = RichText::new(&run.text).size(size).color(colour);
        text = match run.style {
            Style::Plain | Style::Strong => text,
            Style::Emphasis | Style::StrongEmphasis => text.italics(),
            Style::Code => text.monospace().background_color(theme::surface_hi()),
            Style::Strike => text.strikethrough(),
        };
        if run.href.is_some() {
            text = text.underline();
        }

        let label = ui.label(text);
        if let Some(href) = run.href.as_deref() {
            // Shown, not followed. A quick view that opened a browser because
            // the cursor moved onto a file would be a file manager taking an
            // action nobody asked for; the target is on hover instead.
            label.on_hover_text(href);
        }
    }
}

/// An image is named, not fetched.
fn draw_image_placeholder(
    ui: &mut egui::Ui,
    run: &markdown::Run,
    image: &markdown::Image,
    size: f32,
) {
    let alt = if run.text.is_empty() {
        "image"
    } else {
        run.text.as_str()
    };
    let label = ui.label(RichText::new(format!("\u{1F5BC} {alt}")).size(size).color(
        if image.remote {
            theme::text_faint()
        } else {
            theme::text_dim()
        },
    ));
    if image.remote {
        // Not squeamishness: a preview that loaded this would tell whoever
        // hosts it the moment the cursor landed on the file, which is what a
        // tracking pixel in an email is.
        label.on_hover_text(format!(
            "{}\n\nNot loaded - it is somewhere else, and fetching it would say you opened this file.",
            image.src
        ));
    } else {
        label.on_hover_text(&image.src);
    }
}

#[cfg(test)]
mod document_tests {
    use super::*;

    fn written(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_markdown_file_arrives_parsed_and_a_text_file_does_not() {
        let dir = tempfile::tempdir().unwrap();

        let doc = written(dir.path(), "README.md", "# Title\n\nA paragraph.\n");
        match load(&doc, false) {
            Loaded::Markdown(blocks) => {
                assert_eq!(blocks.len(), 2, "a heading and a paragraph");
                assert_eq!(blocks[0].text(), "Title");
            }
            _ => panic!("a .md file should arrive as a parsed document"),
        }

        // The same bytes under another name are text, and stay text. What
        // makes a document is the name claiming it - a `#` in a source file
        // is a comment or a preprocessor line, not a heading.
        let plain = written(dir.path(), "notes.txt", "# Title\n\nA paragraph.\n");
        assert!(
            matches!(load(&plain, false), Loaded::Text(_)),
            "only files that say they are markdown are parsed as markdown"
        );
    }

    #[test]
    fn a_document_that_cannot_be_read_as_text_still_shows_as_something() {
        // Invalid UTF-8 under a .md name. The parse needs a `String`, so this
        // is the branch that falls back rather than giving up - a file that
        // showed nothing at all would be the viewer refusing to answer a
        // question it can answer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.md");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01, 0x02]).unwrap();
        assert!(
            !matches!(load(&path, false), Loaded::Nothing(_)),
            "something is always shown, even when it is only bytes"
        );
    }

    #[test]
    fn table_cells_are_the_ranges_the_engine_marked() {
        let blocks = markdown::parse("| a | b |\n| --- | --- |\n| c | d |\n");
        let row = blocks
            .iter()
            .find(|b| matches!(b.kind, markdown::Kind::TableRow { header: true }))
            .expect("a header row");

        let cells = cells_of(row);
        assert_eq!(cells.len(), 2, "two columns");
        // Every run is accounted for exactly once: a cell range that
        // overlapped or skipped would draw a word twice or lose it.
        assert_eq!(cells[0].start, 0);
        assert_eq!(cells[1].end, row.runs.len());
        assert_eq!(cells[0].end, cells[1].start);
    }

    #[test]
    fn a_row_with_no_marked_cells_is_still_one_cell() {
        // Not a hypothetical: `cells` is skipped when empty, so anything that
        // is not a table arrives this way, and a `Vec` of no ranges would
        // draw none of the runs.
        let block = markdown::Block {
            kind: markdown::Kind::Paragraph,
            depth: 0,
            runs: vec![markdown::Run {
                text: "alone".into(),
                style: markdown::Style::Plain,
                href: None,
                image: None,
            }],
            cells: Vec::new(),
        };
        assert_eq!(cells_of(&block), vec![0..1]);
    }

    #[test]
    fn emphasis_moves_away_from_the_background_whichever_way_that_is() {
        let sum = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;

        theme::set_palette(theme::Palette::midnight());
        let dim = Color32::from_rgb(0x80, 0x80, 0x80);
        assert!(
            sum(stronger(dim)) > sum(dim),
            "on a dark ground, bold is lighter"
        );

        theme::set_palette(theme::Palette::paper());
        assert!(
            sum(stronger(dim)) < sum(dim),
            "on a light ground, bold is darker - the same rule, not the same direction"
        );
        theme::set_palette(theme::Palette::midnight());
    }
}

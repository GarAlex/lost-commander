// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The picture editor: turn it, mirror it, crop it, resize it, write it back.
//!
//! The arithmetic is all in [`lost_commander_core::imageops`] and tested without pixels.
//! What is here is the three things that need real pixels: reading the file at
//! its **own** size, applying the operations, and encoding the result.
//!
//! Reading at the file's own size is the point. The preview panel downscales
//! anything enormous, because a panel a few hundred points wide has no use for
//! six thousand pixels - but saving that back would quietly throw away most of
//! a photograph. The editor re-reads the file rather than borrowing what the
//! preview already has.
//!
//! Operations are held, not applied. The session keeps an [`Edit`] and the
//! untouched source, so five presses of rotate are one rotation of the
//! original rather than five rounds of resampling, and Reset is dropping a
//! value rather than trying to invert one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use image::DynamicImage;

use lost_commander_core::imageops::{self, Crop, Drawn, Edit, Turn};

use super::theme;

/// The most pixels the editor will take on.
///
/// Held as RGBA, a hundred megapixels is 400 MB before anything is done to it,
/// and the result and its texture are two more copies. Refusing is a better
/// answer than being killed by the kernel halfway through a rotation.
pub const MAX_PIXELS: u64 = 80_000_000;

/// The longest edge the on-screen copy is kept at.
///
/// The result is saved from the full-size image; this is only what gets
/// uploaded to draw, and no screen has forty thousand pixels across.
const SHOWN_EDGE: u32 = 2048;

/// Reading and decoding a picture, on a thread.
///
/// Decoding a large photograph takes long enough to drop frames, and the
/// window has to stay alive while it happens - so it happens elsewhere and
/// the frame that finds it finished picks it up.
pub struct Job {
    pub path: PathBuf,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Loaded, String>>>>,
}

impl Job {
    pub fn spawn(path: PathBuf) -> Job {
        let done = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));

        let worker_done = Arc::clone(&done);
        let worker_result = Arc::clone(&result);
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let loaded = read(&worker_path);
            *worker_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
            worker_done.store(true, Ordering::Release);
        });

        Job { path, done, result }
    }

    pub fn is_finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn take(&mut self) -> Option<Result<Loaded, String>> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// A picture, and what its file held that the pixels do not.
pub struct Loaded {
    pub image: DynamicImage,
    pub carries: imageops::Carries,
}

/// Read one picture at its own size, or say why not.
fn read(path: &Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let format = reader.format();

    // Asked before decoding, not after: the whole point is not to allocate
    // the four hundred megabytes in the first place.
    if let Ok((width, height)) = reader.into_dimensions() {
        let pixels = width as u64 * height as u64;
        if pixels > MAX_PIXELS {
            return Err(format!(
                "{width}x{height} is too big to edit here - {} megapixels, and the limit is {}",
                pixels / 1_000_000,
                MAX_PIXELS / 1_000_000
            ));
        }
    }

    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    Ok(Loaded {
        carries: imageops::Carries {
            animated: is_animated(&bytes, format),
            metadata: has_metadata(&bytes),
        },
        image,
    })
}

/// Whether a file holds more than one frame.
///
/// Read off the bytes rather than the decoded picture, because by then it is
/// already gone: a decoder hands over one grid of colours and nothing else.
///
/// Decodes at most two frames and stops. The question is only ever "is this an
/// animation" - counting a hundred-frame GIF would be a hundred decodes to put
/// a number in a warning, and a warning printing the number it stopped at would
/// say two whatever the file actually held.
fn is_animated(bytes: &[u8], format: Option<image::ImageFormat>) -> bool {
    use image::AnimationDecoder;
    let reader = || std::io::BufReader::new(std::io::Cursor::new(bytes));
    match format {
        Some(image::ImageFormat::Gif) => image::codecs::gif::GifDecoder::new(reader())
            .map(|decoder| decoder.into_frames().take(2).count() > 1)
            .unwrap_or(false),
        Some(image::ImageFormat::WebP) => image::codecs::webp::WebPDecoder::new(reader())
            .map(|decoder| decoder.has_animation())
            .unwrap_or(false),
        _ => false,
    }
}

/// Whether the file carries a metadata block.
///
/// The marker, not the contents: this only decides whether to say the block
/// will not be carried over, and for that "there is one" is the whole
/// question. `Exif\0\0` opens the segment in JPEG, WebP and PNG alike.
fn has_metadata(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(128 * 1024)];
    head.windows(6).any(|window| window == b"Exif\0\0")
}

/// Everything the editor asks for, applied in the order [`Edit`] documents:
/// crop, then mirror, then turn, then resize.
pub fn apply(source: &DynamicImage, edit: &Edit) -> DynamicImage {
    let mut out = match edit.crop {
        Some(crop) => source.crop_imm(crop.x, crop.y, crop.width, crop.height),
        None => source.clone(),
    };
    if edit.transform.flip_h {
        out = out.fliph();
    }
    if edit.transform.flip_v {
        out = out.flipv();
    }
    out = match edit.transform.turn {
        Turn::None => out,
        Turn::Right => out.rotate90(),
        Turn::Half => out.rotate180(),
        Turn::Left => out.rotate270(),
    };
    if let Some((width, height)) = edit.resize {
        // Lanczos for the saved result: a resize is a thing you look at
        // afterwards, and this is the one operation where the filter shows.
        out = out.resize_exact(
            width.max(1),
            height.max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }
    out
}

/// Write a picture where it is going. The format comes from the extension,
/// which is what makes "save as .png" mean something.
pub fn save(image: &DynamicImage, path: &Path) -> Result<(), String> {
    image.save(path).map_err(|e| e.to_string())
}

/// What the editor is being asked to do, once the frame is over.
pub enum Outcome {
    /// Still open, nothing to do.
    Nothing,
    Close,
    /// Write the result to this path. The caller does it, so the one place
    /// that reports success or failure stays the one place.
    Write(PathBuf),
}

/// One picture open for editing.
pub struct Session {
    pub path: PathBuf,
    /// Untouched, for the whole session. Every operation is applied to this.
    source: DynamicImage,
    pub size: (u32, u32),
    pub edit: Edit,
    /// The edit the texture was built from, so it is only rebuilt when
    /// something has actually changed.
    shown: Option<Edit>,
    texture: Option<egui::TextureHandle>,
    /// The corners of a crop being dragged out, in screen points.
    dragging: Option<((f32, f32), (f32, f32))>,
    /// Both boxes, as typed. Kept as text so a half-typed number is not
    /// snapped to something else while it is being typed.
    width_box: String,
    height_box: String,
    keep_aspect: bool,
    /// Where "save as" would write, shown only once it has been asked for.
    save_as: Option<String>,
    /// Overwriting the original is asked about once, then done.
    confirming: bool,
    /// Set once something has been written, so the title says so.
    pub written: bool,
    /// What the file held besides its first frame's pixels, so the window can
    /// say what a save will leave behind.
    carries: imageops::Carries,
}

impl Session {
    pub fn new(path: PathBuf, loaded: Loaded) -> Session {
        let source = loaded.image;
        let size = (source.width(), source.height());
        Session {
            path,
            source,
            size,
            edit: Edit::default(),
            shown: None,
            texture: None,
            dragging: None,
            width_box: size.0.to_string(),
            height_box: size.1.to_string(),
            keep_aspect: true,
            save_as: None,
            confirming: false,
            written: false,
            carries: loaded.carries,
        }
    }

    /// The extension a save would write through, which decides the format.
    fn extension(&self) -> String {
        self.path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }

    /// The picture as the operations leave it, at full size.
    pub fn result(&self) -> DynamicImage {
        apply(&self.source, &self.edit)
    }

    /// The size the resize boxes count from - everything but the resize.
    fn natural(&self) -> (u32, u32) {
        self.edit.size_before_resize(self.size)
    }

    /// Put the resize boxes back to what the rest of the operations come to.
    ///
    /// Called whenever a crop or a turn changes the shape underneath them,
    /// because boxes still showing the old dimensions are boxes that lie.
    fn refresh_boxes(&mut self) {
        let natural = self.natural();
        self.width_box = natural.0.to_string();
        self.height_box = natural.1.to_string();
    }

    fn changed(&mut self) {
        self.confirming = false;
    }
}

/// Draw the editor. Returns what to do about it once the frame is done.
pub fn draw(ctx: &egui::Context, session: &mut Session) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let mut closed = false;
    let source_size = session.size;

    // Rebuilding is a full-size crop, rotate and resize; only worth doing when
    // the operations have actually changed, not sixty times a second.
    if session.shown != Some(session.edit) {
        let shown = downscaled(&session.result());
        let rgba = shown.to_rgba8();
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            &rgba,
        );
        session.texture = Some(ctx.load_texture("image_edit", image, egui::TextureOptions::LINEAR));
        session.shown = Some(session.edit);
    }

    let escaped = super::modal(ctx, "Edit picture", |ui| {
        ui.set_min_width(720.0);
        ui.label(
            RichText::new(session.path.display().to_string())
                .size(11.0)
                .monospace()
                .color(theme::text_dim()),
        );
        ui.add_space(6.0);

        toolbar(ui, session);
        ui.add_space(6.0);
        canvas(ui, session);
        ui.add_space(6.0);
        resize_row(ui, session);

        // What has been done, and what it will come out as.
        let result_size = session.edit.size_of(source_size);
        let done = session.edit.describe(source_size);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{}x{} \u{2192} {}x{}",
                    source_size.0, source_size.1, result_size.0, result_size.1
                ))
                .size(11.0)
                .monospace()
                .color(theme::text_dim()),
            );
            if !done.is_empty() {
                ui.label(RichText::new(done).size(11.0).color(theme::text_faint()));
            }
        });

        ui.add_space(6.0);
        outcome = buttons(ui, session, &mut closed);
    });

    if escaped || closed {
        return Outcome::Close;
    }
    outcome
}

/// Keep the on-screen copy to something a screen can use.
fn downscaled(image: &DynamicImage) -> DynamicImage {
    let longest = image.width().max(image.height());
    if longest <= SHOWN_EDGE {
        return image.clone();
    }
    let scale = SHOWN_EDGE as f32 / longest as f32;
    image.resize(
        ((image.width() as f32 * scale) as u32).max(1),
        ((image.height() as f32 * scale) as u32).max(1),
        image::imageops::FilterType::Triangle,
    )
}

fn toolbar(ui: &mut egui::Ui, session: &mut Session) {
    ui.horizontal(|ui| {
        if ui
            .button("\u{21BA}")
            .on_hover_text("Turn a quarter to the left")
            .clicked()
        {
            session.edit.transform.turn_left();
            session.refresh_boxes();
            session.changed();
        }
        if ui
            .button("\u{21BB}")
            .on_hover_text("Turn a quarter to the right")
            .clicked()
        {
            session.edit.transform.turn_right();
            session.refresh_boxes();
            session.changed();
        }
        if ui.button("Mirror").on_hover_text("Left to right").clicked() {
            session.edit.transform.flip_horizontal();
            session.changed();
        }
        if ui.button("Flip").on_hover_text("Top to bottom").clicked() {
            session.edit.transform.flip_vertical();
            session.changed();
        }

        ui.separator();
        let cropped = session.edit.crop.is_some();
        ui.label(
            RichText::new(if cropped {
                "drag to crop again"
            } else {
                "drag on the picture to crop"
            })
            .size(11.0)
            .color(theme::text_faint()),
        );
        if ui
            .add_enabled(cropped, egui::Button::new("Whole picture"))
            .on_hover_text("Undo the crop")
            .clicked()
        {
            session.edit.crop = None;
            session.refresh_boxes();
            session.changed();
        }

        ui.separator();
        if ui
            .add_enabled(!session.edit.is_identity(), egui::Button::new("Reset"))
            .on_hover_text("Back to the picture as it is on disk")
            .clicked()
        {
            session.edit = Edit::default();
            session.refresh_boxes();
            session.changed();
        }
    });
}

/// The picture, and the drag that crops it.
fn canvas(ui: &mut egui::Ui, session: &mut Session) {
    let Some(texture) = session.texture.clone() else {
        return;
    };
    let shown = texture.size_vec2();
    let area = egui::vec2(ui.available_width(), 380.0);
    let drawn_size = super::preview::fitted(shown, area);

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 380.0),
        egui::Sense::click_and_drag(),
    );
    // Centred in whatever room there is, which is where the picture is drawn
    // and therefore what a drag has to be measured against.
    let where_ = egui::Rect::from_center_size(rect.center(), drawn_size);
    ui.painter().image(
        texture.id(),
        where_,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    // The crop is dragged against the picture *as it is now* - after the turn
    // and any earlier crop - because that is what is under the pointer. It is
    // then folded back into the source's own pixels below.
    let showing = session.edit.size_before_resize(session.size);
    let drawn = Drawn {
        x: where_.min.x,
        y: where_.min.y,
        width: where_.width(),
        height: where_.height(),
    };

    if response.drag_started() {
        // Where the button actually went down, not where the pointer had got
        // to by the time egui called it a drag. A drag is only recognised
        // after a few points of movement, so taking the position on that
        // frame swallows the start of every crop - and the faster the drag,
        // the more of it goes missing.
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        if let Some(at) = origin {
            session.dragging = Some(((at.x, at.y), (at.x, at.y)));
        }
    }
    if response.dragged() {
        if let (Some(at), Some(drag)) = (response.interact_pointer_pos(), session.dragging.as_mut())
        {
            drag.1 = (at.x, at.y);
        }
    }

    // While the drag is on, show what it would take.
    if let Some((from, to)) = session.dragging {
        if let Some(crop) = imageops::crop_from_drag(from, to, drawn, showing) {
            let on_screen = imageops::crop_on_screen(crop, drawn, showing);
            let outline = egui::Rect::from_min_size(
                egui::pos2(on_screen.x, on_screen.y),
                egui::vec2(on_screen.width, on_screen.height),
            );
            ui.painter().rect_stroke(
                outline,
                0.0,
                egui::Stroke::new(1.5, theme::accent()),
                egui::StrokeKind::Middle,
            );
            ui.painter().text(
                outline.left_top() + egui::vec2(4.0, -14.0),
                egui::Align2::LEFT_TOP,
                format!("{}x{}", crop.width, crop.height),
                egui::FontId::monospace(11.0),
                theme::accent(),
            );
        }
    }

    if response.drag_stopped() {
        if let Some((from, to)) = session.dragging.take() {
            if let Some(dragged) = imageops::crop_from_drag(from, to, drawn, showing) {
                let base = session.edit.crop.unwrap_or(Crop::whole(session.size));
                if let Some(folded) =
                    imageops::fold_crop(dragged, base, session.edit.transform, session.size)
                {
                    session.edit.crop = Some(folded);
                    session.refresh_boxes();
                    session.changed();
                }
            }
        }
    }
}

fn resize_row(ui: &mut egui::Ui, session: &mut Session) {
    let natural = session.natural();
    ui.horizontal(|ui| {
        ui.label(RichText::new("size").size(11.0).color(theme::text_faint()));

        let width = ui.add(
            egui::TextEdit::singleline(&mut session.width_box)
                .desired_width(64.0)
                .font(egui::TextStyle::Monospace),
        );
        ui.label(RichText::new("x").size(11.0).color(theme::text_faint()));
        let height = ui.add(
            egui::TextEdit::singleline(&mut session.height_box)
                .desired_width(64.0)
                .font(egui::TextStyle::Monospace),
        );
        ui.checkbox(&mut session.keep_aspect, "keep the shape");

        if width.changed() || height.changed() {
            let typed = (
                session.width_box.trim().parse::<u32>().unwrap_or(0),
                session.height_box.trim().parse::<u32>().unwrap_or(0),
            );
            // An empty or half-typed box is not a resize yet; leave it alone
            // rather than snapping it to 1 under the cursor.
            if typed.0 > 0 && typed.1 > 0 {
                let want = if session.keep_aspect {
                    imageops::keep_aspect(natural, typed, width.changed())
                } else {
                    typed
                };
                session.width_box = want.0.to_string();
                session.height_box = want.1.to_string();
                session.edit.resize = (want != natural).then_some(want);
                session.confirming = false;
            }
        }

        for percent in [25.0f32, 50.0, 200.0] {
            if ui.small_button(format!("{percent:.0}%")).clicked() {
                let want = imageops::scaled(natural, percent);
                session.width_box = want.0.to_string();
                session.height_box = want.1.to_string();
                session.edit.resize = Some(want);
                session.confirming = false;
            }
        }
        if ui
            .add_enabled(session.edit.resize.is_some(), egui::Button::new("Own size"))
            .clicked()
        {
            session.edit.resize = None;
            session.refresh_boxes();
            session.confirming = false;
        }
    });
}

fn buttons(ui: &mut egui::Ui, session: &mut Session, closed: &mut bool) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let name = session
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let extension = session.extension();
    // Both worked out before the buttons, so what a save would cost is on
    // screen while there is still the option of Save as - rather than
    // reported afterwards, when the frames are already gone.
    let refused = imageops::refuses(&extension, session.edit.size_of(session.size));
    let losses = imageops::losses(&extension, session.carries);

    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            *closed = true;
        }

        if session.confirming {
            ui.label(
                RichText::new(format!("Write over {name}?"))
                    .size(11.5)
                    .color(theme::danger()),
            );
            if ui.button("Overwrite").clicked() {
                session.confirming = false;
                outcome = Outcome::Write(session.path.clone());
            }
            if ui.button("Keep it").clicked() {
                session.confirming = false;
            }
        } else {
            // Nothing changed is nothing to save: a Save that re-encodes a
            // JPEG for no reason is a Save that costs quality for nothing.
            // And a format that cannot hold the result is not offered at all,
            // rather than offered and then failed.
            let can = !session.edit.is_identity() && refused.is_none();
            let save = ui
                .add_enabled(can, egui::Button::new("Save"))
                .on_hover_text("Write over the original");
            let save = match &refused {
                Some(why) => save.on_disabled_hover_text(why.clone()),
                None => save,
            };
            if save.clicked() {
                session.confirming = true;
            }
            if ui.button("Save as...").clicked() {
                session.save_as = Some(match &session.save_as {
                    Some(existing) => existing.clone(),
                    None => suggested_name(&session.path),
                });
            }
        }
    });

    // Taken out and put back, so the box can be edited without the closure
    // holding a borrow of the session it also has to write to.
    if let Some(mut typed) = session.save_as.take() {
        let (mut cancelled, mut go) = (false, false);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("save as")
                    .size(11.0)
                    .color(theme::text_faint()),
            );
            let box_ = ui.add(
                egui::TextEdit::singleline(&mut typed)
                    .desired_width(280.0)
                    .font(egui::TextStyle::Monospace),
            );
            let entered = box_.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            go = ui.button("Write").clicked() || entered;
            cancelled = ui.button("Cancel").clicked();
        });

        let name = typed.trim().to_string();
        if go && !name.is_empty() {
            // Alongside the original unless an absolute path was typed, which
            // is the rule the rename box follows too.
            let target = Path::new(&name);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                session.path.parent().unwrap_or(Path::new(".")).join(target)
            };
            outcome = Outcome::Write(target);
        }
        if !cancelled {
            session.save_as = Some(typed);
        }
    }

    // A format that cannot take the result at all is said in the danger
    // colour; what a save merely leaves behind is quieter, but still said.
    if let Some(why) = &refused {
        ui.label(
            RichText::new(format!("Cannot save: {why}."))
                .size(10.5)
                .color(theme::danger()),
        );
    }
    for loss in &losses {
        ui.label(
            RichText::new(format!("Saving keeps the pixels only - {loss}."))
                .size(10.5)
                .color(if session.carries.animated {
                    theme::danger()
                } else {
                    theme::text_faint()
                }),
        );
    }

    outcome
}

/// A name beside the original rather than over it: `photo.jpg` suggests
/// `photo-edited.jpg`, which is the thing anyone typing into that box wants
/// and would otherwise type by hand.
fn suggested_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    match path.extension() {
        Some(extension) => format!("{stem}-edited.{}", extension.to_string_lossy()),
        None => format!("{stem}-edited"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_suggested_beside_the_original_rather_than_over_it() {
        assert_eq!(
            suggested_name(Path::new("/photos/beach.jpg")),
            "beach-edited.jpg"
        );
        assert_eq!(suggested_name(Path::new("/photos/scan")), "scan-edited");
        assert_eq!(
            suggested_name(Path::new("/photos/a.b.png")),
            "a.b-edited.png"
        );
    }

    /// A tiny picture whose four quadrants are four colours, so any turn or
    /// mirror can be checked by looking at one pixel.
    fn quadrants() -> DynamicImage {
        let mut buffer = image::RgbaImage::new(2, 2);
        buffer.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // red top left
        buffer.put_pixel(1, 0, image::Rgba([0, 255, 0, 255])); // green top right
        buffer.put_pixel(0, 1, image::Rgba([0, 0, 255, 255])); // blue bottom left
        buffer.put_pixel(1, 1, image::Rgba([255, 255, 0, 255])); // yellow bottom right
        DynamicImage::ImageRgba8(buffer)
    }

    fn corner(image: &DynamicImage, x: u32, y: u32) -> [u8; 4] {
        image.to_rgba8().get_pixel(x, y).0
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn a_quarter_turn_moves_the_pixels_the_way_the_arithmetic_says() {
        let source = quadrants();
        let mut edit = Edit::default();
        edit.transform.turn_right();

        let turned = apply(&source, &edit);
        assert_eq!(
            (turned.width(), turned.height()),
            edit.size_of((2, 2)),
            "the size agrees with imageops"
        );
        // Clockwise: the top-left goes to the top-right.
        assert_eq!(corner(&turned, 1, 0), RED);
        assert_eq!(corner(&turned, 1, 1), GREEN);
    }

    #[test]
    fn mirroring_moves_the_pixels_across() {
        let source = quadrants();
        let mut edit = Edit::default();
        edit.transform.flip_horizontal();
        let mirrored = apply(&source, &edit);
        assert_eq!(corner(&mirrored, 0, 0), GREEN);
        assert_eq!(corner(&mirrored, 1, 0), RED);
    }

    #[test]
    fn a_crop_takes_the_rectangle_it_was_given() {
        let source = quadrants();
        let edit = Edit {
            crop: Some(Crop {
                x: 0,
                y: 1,
                width: 1,
                height: 1,
            }),
            ..Default::default()
        };
        let cropped = apply(&source, &edit);
        assert_eq!((cropped.width(), cropped.height()), (1, 1));
        assert_eq!(corner(&cropped, 0, 0), BLUE);
    }

    #[test]
    fn the_operations_happen_in_the_order_the_edit_documents() {
        // Crop first, then turn, then resize - so the result is the size the
        // pure arithmetic predicted, whatever combination is asked for.
        let source = {
            let mut buffer = image::RgbaImage::new(40, 20);
            for (_, _, pixel) in buffer.enumerate_pixels_mut() {
                *pixel = image::Rgba([1, 2, 3, 255]);
            }
            DynamicImage::ImageRgba8(buffer)
        };
        let mut edit = Edit {
            crop: Some(Crop {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            }),
            ..Default::default()
        };
        edit.transform.turn_right();
        edit.resize = Some((5, 7));

        let result = apply(&source, &edit);
        assert_eq!(
            (result.width(), result.height()),
            edit.size_of((40, 20)),
            "pixels and arithmetic agree"
        );
        assert_eq!((result.width(), result.height()), (5, 7));
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.png");
        let mut edit = Edit::default();
        edit.transform.turn_right();
        let result = apply(&quadrants(), &edit);

        save(&result, &path).unwrap();
        let read_back = image::open(&path).unwrap();
        assert_eq!((read_back.width(), read_back.height()), (2, 2));
        assert_eq!(corner(&read_back, 1, 0), RED);
    }

    #[test]
    fn a_picture_too_big_to_hold_is_refused_rather_than_attempted() {
        // Not by decoding one - by asking the header. A file claiming more
        // pixels than the limit never reaches the allocator.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.png");
        save(&quadrants(), &path).unwrap();
        assert!(read(&path).is_ok(), "a small one is fine");

        let missing = dir.path().join("nothing.png");
        assert!(read(&missing).is_err());
    }
}

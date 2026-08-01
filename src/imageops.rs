//! The geometry of the picture operations: turning, flipping, cropping and
//! resizing.
//!
//! Only the arithmetic is here. Moving pixels around is three lines of the
//! `image` crate and needs no help; what is worth writing down and testing is
//! everything around it - how two quarter-turns compose, which way a flip goes
//! once a turn is in effect, where on the picture a rectangle dragged across
//! the screen actually lands, and what a resize comes to when the shape is
//! being kept.
//!
//! That arithmetic is where the mistakes live, and none of it needs a
//! decoded image to check.
//!
//! One rule runs through all of it: **the source is never touched**. A session
//! holds the operations, not the result, so pressing rotate five times is
//! still one rotation of the original rather than five rounds of resampling,
//! and undoing is dropping an operation rather than trying to reverse one.

/// A quarter-turn clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Turn {
    #[default]
    None,
    Right,
    Half,
    Left,
}

impl Turn {
    /// How many quarter-turns clockwise, which is what makes them compose.
    pub fn quarters(self) -> u8 {
        match self {
            Turn::None => 0,
            Turn::Right => 1,
            Turn::Half => 2,
            Turn::Left => 3,
        }
    }

    pub fn from_quarters(quarters: u8) -> Turn {
        match quarters % 4 {
            0 => Turn::None,
            1 => Turn::Right,
            2 => Turn::Half,
            _ => Turn::Left,
        }
    }

    pub fn right(self) -> Turn {
        Turn::from_quarters(self.quarters() + 1)
    }

    pub fn left(self) -> Turn {
        Turn::from_quarters(self.quarters() + 3)
    }

    /// Whether this turn puts the picture's width where its height was.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Turn::Right | Turn::Left)
    }

    pub fn label(self) -> &'static str {
        match self {
            Turn::None => "0°",
            Turn::Right => "90°",
            Turn::Half => "180°",
            Turn::Left => "270°",
        }
    }
}

/// Turning and flipping together, applied in one fixed order.
///
/// Flip first, then turn. Fixing the order is what stops "rotate, flip,
/// rotate back" from landing somewhere different than "flip" - the operations
/// do not commute, so the only way to keep them predictable is to record what
/// was asked for and always compose it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transform {
    pub turn: Turn,
    /// Mirror the source left to right, before the turn.
    pub flip_h: bool,
    /// Mirror the source top to bottom, before the turn.
    pub flip_v: bool,
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }

    pub fn turn_right(&mut self) {
        self.turn = self.turn.right();
    }

    pub fn turn_left(&mut self) {
        self.turn = self.turn.left();
    }

    /// Mirror what is *on screen* left to right.
    ///
    /// Which is not always a mirror of the source: with a quarter-turn in
    /// effect the screen's horizontal is the source's vertical, so the flip
    /// has to be recorded as the other one. Getting this wrong gives a button
    /// that flips the right way until you rotate, and the wrong way after.
    pub fn flip_horizontal(&mut self) {
        if self.turn.swaps_axes() {
            self.flip_v = !self.flip_v;
        } else {
            self.flip_h = !self.flip_h;
        }
    }

    /// Mirror what is on screen top to bottom. As above, the other way round.
    pub fn flip_vertical(&mut self) {
        if self.turn.swaps_axes() {
            self.flip_h = !self.flip_h;
        } else {
            self.flip_v = !self.flip_v;
        }
    }

    /// Whether the result reads as mirrored left to right *on screen*.
    ///
    /// The flips are stored against the source and applied before the turn,
    /// so with a quarter turn in effect the stored vertical flip is the one
    /// that shows as a mirror. Anything describing the picture to a person has
    /// to ask this rather than read the field, or pressing Mirror produces the
    /// word "flipped".
    pub fn mirrored_on_screen(&self) -> bool {
        if self.turn.swaps_axes() {
            self.flip_v
        } else {
            self.flip_h
        }
    }

    /// Whether the result reads as flipped top to bottom on screen. As above.
    pub fn flipped_on_screen(&self) -> bool {
        if self.turn.swaps_axes() {
            self.flip_h
        } else {
            self.flip_v
        }
    }

    /// How big the picture comes out.
    pub fn size_of(&self, size: (u32, u32)) -> (u32, u32) {
        if self.turn.swaps_axes() {
            (size.1, size.0)
        } else {
            size
        }
    }
}

/// A rectangle of the picture, in the picture's own pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Crop {
    pub fn whole(size: (u32, u32)) -> Crop {
        Crop {
            x: 0,
            y: 0,
            width: size.0,
            height: size.1,
        }
    }

    /// Trim to what is actually inside the picture.
    ///
    /// `None` where nothing is left, so a stray click that drags one pixel -
    /// or off the edge entirely - cannot become a crop to nothing.
    pub fn clamped(self, size: (u32, u32)) -> Option<Crop> {
        let x = self.x.min(size.0);
        let y = self.y.min(size.1);
        let width = self.width.min(size.0.saturating_sub(x));
        let height = self.height.min(size.1.saturating_sub(y));
        if width == 0 || height == 0 {
            return None;
        }
        Some(Crop {
            x,
            y,
            width,
            height,
        })
    }

    pub fn is_whole(&self, size: (u32, u32)) -> bool {
        self.x == 0 && self.y == 0 && (self.width, self.height) == size
    }
}

/// Where a picture is drawn on screen: top-left corner, and the size it is
/// drawn at. Floats, because that is what a fitted-and-zoomed picture is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Which pixel of the picture a point on screen is over.
///
/// Clamped to the picture, so dragging past the edge selects to the edge -
/// which is what every crop tool does and what anyone dragging expects.
pub fn at_screen(point: (f32, f32), drawn: Drawn, size: (u32, u32)) -> (u32, u32) {
    if drawn.width <= 0.0 || drawn.height <= 0.0 {
        return (0, 0);
    }
    let across = ((point.0 - drawn.x) / drawn.width).clamp(0.0, 1.0);
    let down = ((point.1 - drawn.y) / drawn.height).clamp(0.0, 1.0);
    (
        ((across * size.0 as f32).round() as u32).min(size.0),
        ((down * size.1 as f32).round() as u32).min(size.1),
    )
}

/// The crop a drag from one screen point to another asks for.
///
/// Either corner may be dragged to, so the rectangle is worked out from the
/// two points rather than assuming which one came first.
pub fn crop_from_drag(
    from: (f32, f32),
    to: (f32, f32),
    drawn: Drawn,
    size: (u32, u32),
) -> Option<Crop> {
    let (x1, y1) = at_screen(from, drawn, size);
    let (x2, y2) = at_screen(to, drawn, size);
    Crop {
        x: x1.min(x2),
        y: y1.min(y2),
        width: x1.abs_diff(x2),
        height: y1.abs_diff(y2),
    }
    .clamped(size)
}

/// Where a crop of the picture sits on screen, for drawing the marching
/// rectangle over it.
pub fn crop_on_screen(crop: Crop, drawn: Drawn, size: (u32, u32)) -> Drawn {
    if size.0 == 0 || size.1 == 0 {
        return drawn;
    }
    let across = drawn.width / size.0 as f32;
    let down = drawn.height / size.1 as f32;
    Drawn {
        x: drawn.x + crop.x as f32 * across,
        y: drawn.y + crop.y as f32 * down,
        width: crop.width as f32 * across,
        height: crop.height as f32 * down,
    }
}

/// Turn a crop of what is *on screen* into a crop of the source's own pixels.
///
/// The rectangle was dragged over a picture that may already be cropped,
/// mirrored and turned, while an [`Edit`] stores its crop against the
/// untouched source. So the turn has to be undone, the mirrors undone, and the
/// earlier crop's corner added back on.
///
/// Storing it the other way round - against the picture as displayed - is the
/// tempting shortcut and the wrong one: the stored rectangle would then move
/// every time anything rotated.
///
/// `base` is the crop already in effect, `transform` what is applied after it,
/// and `source` the size of the untouched picture.
pub fn fold_crop(
    dragged: Crop,
    base: Crop,
    transform: Transform,
    source: (u32, u32),
) -> Option<Crop> {
    let (width, height) = (base.width, base.height);

    // Undo the turn: where the rectangle sits on the un-turned picture. The
    // turned picture's axes are swapped for a quarter turn, so the dragged
    // width becomes a height and the corner is measured from the far side.
    let (mut x, mut y, w, h) = match transform.turn {
        Turn::None => (dragged.x, dragged.y, dragged.width, dragged.height),
        Turn::Right => (
            dragged.y,
            height.saturating_sub(dragged.x + dragged.width),
            dragged.height,
            dragged.width,
        ),
        Turn::Half => (
            width.saturating_sub(dragged.x + dragged.width),
            height.saturating_sub(dragged.y + dragged.height),
            dragged.width,
            dragged.height,
        ),
        Turn::Left => (
            width.saturating_sub(dragged.y + dragged.height),
            dragged.x,
            dragged.height,
            dragged.width,
        ),
    };

    // And the mirrors, which came before the turn and so are undone after it.
    if transform.flip_h {
        x = width.saturating_sub(x + w);
    }
    if transform.flip_v {
        y = height.saturating_sub(y + h);
    }

    Crop {
        x: base.x + x,
        y: base.y + y,
        width: w,
        height: h,
    }
    .clamped(source)
}

/// A size that is never zero: a picture zero pixels wide is not a picture, and
/// the encoders refuse it anyway.
fn at_least_one(value: u32) -> u32 {
    value.max(1)
}

/// The other side of a resize when the shape is being kept.
///
/// `changed` says which side was typed into; the other one follows from it.
/// Rounding is to the nearest pixel and never to nothing, so a thumbnail of a
/// very wide panorama is one pixel tall rather than an error.
pub fn keep_aspect(size: (u32, u32), want: (u32, u32), changed_width: bool) -> (u32, u32) {
    if size.0 == 0 || size.1 == 0 {
        return (at_least_one(want.0), at_least_one(want.1));
    }
    if changed_width {
        let width = at_least_one(want.0);
        let height = (width as f64 * size.1 as f64 / size.0 as f64).round() as u32;
        (width, at_least_one(height))
    } else {
        let height = at_least_one(want.1);
        let width = (height as f64 * size.0 as f64 / size.1 as f64).round() as u32;
        (at_least_one(width), height)
    }
}

/// A size as a percentage of another. 50% of an odd number rounds up rather
/// than losing a row.
pub fn scaled(size: (u32, u32), percent: f32) -> (u32, u32) {
    let factor = (percent as f64 / 100.0).max(0.0);
    (
        at_least_one((size.0 as f64 * factor).round() as u32),
        at_least_one((size.1 as f64 * factor).round() as u32),
    )
}

/// Everything asked of one picture, in the order it is applied.
///
/// Crop first, in the source's own pixels, then the turn and flips, then the
/// resize. Cropping first is what makes the rectangle mean what was dragged:
/// a crop recorded against a rotated picture would move when the rotation
/// changed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edit {
    pub crop: Option<Crop>,
    pub transform: Transform,
    /// The size to come out at, after cropping and turning.
    pub resize: Option<(u32, u32)>,
}

impl Edit {
    pub fn is_identity(&self) -> bool {
        self.crop.is_none() && self.transform.is_identity() && self.resize.is_none()
    }

    /// How big the result comes out, given the source size.
    pub fn size_of(&self, source: (u32, u32)) -> (u32, u32) {
        if let Some(resize) = self.resize {
            return (at_least_one(resize.0), at_least_one(resize.1));
        }
        let cropped = match self.crop {
            Some(crop) => (crop.width, crop.height),
            None => source,
        };
        self.transform.size_of(cropped)
    }

    /// The size a resize box should start from: what the picture would be
    /// with everything else applied but no resize.
    pub fn size_before_resize(&self, source: (u32, u32)) -> (u32, u32) {
        let cropped = match self.crop {
            Some(crop) => (crop.width, crop.height),
            None => source,
        };
        self.transform.size_of(cropped)
    }

    /// What the toolbar says has been done, for the line under the picture.
    pub fn describe(&self, source: (u32, u32)) -> String {
        if self.is_identity() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(crop) = self.crop {
            parts.push(format!("cropped to {}x{}", crop.width, crop.height));
        }
        if self.transform.turn != Turn::None {
            parts.push(format!("turned {}", self.transform.turn.label()));
        }
        // Named as they look, not as they are stored: the button said Mirror,
        // so the line under the picture had better not say "flipped".
        if self.transform.mirrored_on_screen() {
            parts.push("mirrored".to_string());
        }
        if self.transform.flipped_on_screen() {
            parts.push("flipped".to_string());
        }
        if self.resize.is_some() {
            let size = self.size_of(source);
            parts.push(format!("resized to {}x{}", size.0, size.1));
        }
        parts.join(", ")
    }
}

/// Whether writing this picture back to `extension` loses something.
///
/// JPEG is re-encoded from pixels, so saving over one costs a generation of
/// quality even where nothing was changed. Worth saying before it happens
/// rather than after.
pub fn is_lossy(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif" | "webp"
    )
}

/// What a file carries that an editor working in pixels cannot put back.
///
/// A decoder hands over a grid of colours. Everything else the file held - the
/// frames after the first, the camera's own record of the shot - is not in
/// that grid and cannot come out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Carries {
    /// More than one frame.
    ///
    /// Deliberately not a count. Counting frames means decoding them, and the
    /// question here is only ever "is this an animation" - so the answer is
    /// found by decoding two and stopping. A message that then printed a
    /// number would be printing the number two, whatever the file held.
    pub animated: bool,
    /// Whether it has a metadata block: EXIF, and its neighbours.
    pub metadata: bool,
}

/// The widest an ICO may be, in either direction. Not a policy - the format
/// stores each dimension in one byte.
pub const ICO_MAX: u32 = 256;

/// Why this picture cannot be written to `extension` at all.
///
/// Asked before the work rather than after it: an editor that lets you crop a
/// screenshot for five minutes and *then* says the format will not take it has
/// wasted five minutes and taught you nothing.
pub fn refuses(extension: &str, size: (u32, u32)) -> Option<String> {
    if extension.eq_ignore_ascii_case("ico") && (size.0 > ICO_MAX || size.1 > ICO_MAX) {
        return Some(format!(
            "an ICO cannot be bigger than {ICO_MAX}x{ICO_MAX}, and this is {}x{} - \
             resize it, or save it as a .png",
            size.0, size.1
        ));
    }
    None
}

/// What writing this picture back would quietly leave behind.
///
/// Not errors: the file is written and the pixels are right. But an animation
/// that becomes a still, or a photograph that loses where it was taken, is the
/// kind of loss you only notice long afterwards - so it is said out loud while
/// there is still the option of Save as.
pub fn losses(extension: &str, carries: Carries) -> Vec<String> {
    let mut losses = Vec::new();
    if carries.animated {
        losses.push(
            "this is an animation, and only its first frame is kept - the rest do not \
             survive being saved"
                .to_string(),
        );
    }
    if carries.metadata {
        losses.push(
            "the metadata (EXIF and the rest) is not carried over - dates, camera \
             settings, orientation and any location go"
                .to_string(),
        );
    }
    if is_lossy(extension) {
        losses.push(format!(
            ".{} is re-encoded from pixels, which costs a generation of quality \
             even where nothing was touched",
            extension.to_ascii_lowercase()
        ));
    }
    losses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_compose_rather_than_accumulate() {
        let mut transform = Transform::default();
        transform.turn_right();
        transform.turn_right();
        assert_eq!(transform.turn, Turn::Half);
        transform.turn_right();
        transform.turn_right();
        assert_eq!(
            transform.turn,
            Turn::None,
            "four times round is where it started"
        );

        transform.turn_right();
        transform.turn_left();
        assert_eq!(transform.turn, Turn::None);
        transform.turn_left();
        assert_eq!(transform.turn, Turn::Left);
    }

    #[test]
    fn a_quarter_turn_puts_the_width_where_the_height_was() {
        let landscape = (400, 300);
        assert_eq!(
            Transform {
                turn: Turn::Right,
                ..Default::default()
            }
            .size_of(landscape),
            (300, 400)
        );
        assert_eq!(
            Transform {
                turn: Turn::Left,
                ..Default::default()
            }
            .size_of(landscape),
            (300, 400)
        );
        assert_eq!(
            Transform {
                turn: Turn::Half,
                ..Default::default()
            }
            .size_of(landscape),
            landscape
        );
    }

    #[test]
    fn flipping_means_flipping_what_is_on_screen() {
        // With a quarter turn in effect the screen's left-to-right is the
        // source's top-to-bottom, so the flip is recorded as the other one.
        // A button that flips the right way until you rotate is worse than
        // no button.
        let mut upright = Transform::default();
        upright.flip_horizontal();
        assert!(upright.flip_h && !upright.flip_v);

        let mut turned = Transform {
            turn: Turn::Right,
            ..Default::default()
        };
        turned.flip_horizontal();
        assert!(
            turned.flip_v && !turned.flip_h,
            "the source flips the other way"
        );

        let mut half = Transform {
            turn: Turn::Half,
            ..Default::default()
        };
        half.flip_horizontal();
        assert!(half.flip_h, "a half turn does not swap the axes");

        // And twice is none, whatever the turn.
        for turn in [Turn::None, Turn::Right, Turn::Half, Turn::Left] {
            let mut transform = Transform {
                turn,
                ..Default::default()
            };
            transform.flip_horizontal();
            transform.flip_horizontal();
            assert!(!transform.flip_h, "{turn:?}");
            assert!(!transform.flip_v, "{turn:?}");
        }
    }

    #[test]
    fn a_crop_is_trimmed_to_the_picture_and_never_to_nothing() {
        let size = (100, 80);
        let inside = Crop {
            x: 10,
            y: 10,
            width: 20,
            height: 20,
        };
        assert_eq!(inside.clamped(size), Some(inside));

        // Hanging off the right and the bottom.
        assert_eq!(
            Crop {
                x: 90,
                y: 70,
                width: 50,
                height: 50
            }
            .clamped(size),
            Some(Crop {
                x: 90,
                y: 70,
                width: 10,
                height: 10
            })
        );

        // Entirely outside, and a zero-width drag: neither is a crop.
        assert_eq!(
            Crop {
                x: 100,
                y: 0,
                width: 10,
                height: 10
            }
            .clamped(size),
            None
        );
        assert_eq!(
            Crop {
                x: 10,
                y: 10,
                width: 0,
                height: 5
            }
            .clamped(size),
            None
        );

        assert!(Crop::whole(size).is_whole(size));
        assert!(!inside.is_whole(size));
    }

    #[test]
    fn a_drag_across_the_screen_lands_on_the_right_pixels() {
        // A 200x100 picture drawn at 400x200 - twice its size - starting 50
        // points in and 20 down.
        let drawn = Drawn {
            x: 50.0,
            y: 20.0,
            width: 400.0,
            height: 200.0,
        };
        let size = (200, 100);

        assert_eq!(at_screen((50.0, 20.0), drawn, size), (0, 0));
        assert_eq!(at_screen((450.0, 220.0), drawn, size), (200, 100));
        assert_eq!(at_screen((250.0, 120.0), drawn, size), (100, 50));

        // Off the edges, in both directions.
        assert_eq!(at_screen((-500.0, -500.0), drawn, size), (0, 0));
        assert_eq!(at_screen((9999.0, 9999.0), drawn, size), (200, 100));

        // Dragged up and to the left: the rectangle is the same either way.
        let forward = crop_from_drag((150.0, 60.0), (350.0, 160.0), drawn, size);
        let backward = crop_from_drag((350.0, 160.0), (150.0, 60.0), drawn, size);
        assert_eq!(forward, backward);
        assert_eq!(
            forward,
            Some(Crop {
                x: 50,
                y: 20,
                width: 100,
                height: 50
            })
        );

        // A click that does not move is not a crop.
        assert_eq!(
            crop_from_drag((150.0, 60.0), (150.0, 60.0), drawn, size),
            None
        );
    }

    #[test]
    fn a_crop_drawn_back_onto_the_screen_lands_where_it_was_dragged() {
        let drawn = Drawn {
            x: 50.0,
            y: 20.0,
            width: 400.0,
            height: 200.0,
        };
        let size = (200, 100);
        let crop = crop_from_drag((150.0, 60.0), (350.0, 160.0), drawn, size).unwrap();
        let back = crop_on_screen(crop, drawn, size);
        assert!((back.x - 150.0).abs() < 0.001, "{back:?}");
        assert!((back.y - 60.0).abs() < 0.001, "{back:?}");
        assert!((back.width - 200.0).abs() < 0.001, "{back:?}");
        assert!((back.height - 100.0).abs() < 0.001, "{back:?}");
    }

    #[test]
    fn a_crop_of_the_whole_view_is_the_crop_that_was_already_there() {
        // The property that matters: whatever has been done to the picture,
        // dragging a rectangle over the entire thing on screen must come back
        // as exactly the crop already in effect. If it does not, an existing
        // crop jumps the moment anything is rotated.
        let source = (400, 300);
        for base in [
            Crop::whole(source),
            Crop {
                x: 40,
                y: 30,
                width: 200,
                height: 100,
            },
        ] {
            for turn in [Turn::None, Turn::Right, Turn::Half, Turn::Left] {
                for flip_h in [false, true] {
                    for flip_v in [false, true] {
                        let transform = Transform {
                            turn,
                            flip_h,
                            flip_v,
                        };
                        // The whole of what is on screen, in its own pixels.
                        let shown = transform.size_of((base.width, base.height));
                        let everything = Crop {
                            x: 0,
                            y: 0,
                            width: shown.0,
                            height: shown.1,
                        };
                        assert_eq!(
                            fold_crop(everything, base, transform, source),
                            Some(base),
                            "{transform:?} over {base:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_crop_dragged_over_a_turned_picture_lands_on_the_right_corner() {
        let source = (100, 60);
        let base = Crop::whole(source);

        // Turned a quarter to the right, the picture on screen is 60x100 and
        // its top-left corner is the source's bottom-left.
        let transform = Transform {
            turn: Turn::Right,
            ..Default::default()
        };
        let top_left_on_screen = Crop {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        assert_eq!(
            fold_crop(top_left_on_screen, base, transform, source),
            Some(Crop {
                x: 0,
                y: 50,
                width: 10,
                height: 10
            })
        );

        // Mirrored, the screen's left is the source's right.
        let mirrored = Transform {
            flip_h: true,
            ..Default::default()
        };
        assert_eq!(
            fold_crop(top_left_on_screen, base, mirrored, source),
            Some(Crop {
                x: 90,
                y: 0,
                width: 10,
                height: 10
            })
        );
    }

    #[test]
    fn cropping_a_crop_adds_the_corners_up() {
        let source = (400, 300);
        let base = Crop {
            x: 100,
            y: 50,
            width: 200,
            height: 100,
        };
        let again = Crop {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        };
        assert_eq!(
            fold_crop(again, base, Transform::default(), source),
            Some(Crop {
                x: 110,
                y: 70,
                width: 30,
                height: 40
            })
        );
    }

    #[test]
    fn keeping_the_shape_follows_whichever_side_was_typed_into() {
        let size = (400, 300);
        assert_eq!(keep_aspect(size, (200, 300), true), (200, 150));
        assert_eq!(keep_aspect(size, (400, 150), false), (200, 150));

        // Never nothing, however extreme the panorama or the number typed.
        assert_eq!(keep_aspect((4000, 3), (1, 0), true), (1, 1));
        assert_eq!(keep_aspect(size, (0, 0), true), (1, 1));
        assert_eq!(keep_aspect((0, 0), (0, 0), true), (1, 1));
    }

    #[test]
    fn a_percentage_is_a_size() {
        assert_eq!(scaled((400, 300), 50.0), (200, 150));
        assert_eq!(scaled((400, 300), 200.0), (800, 600));
        assert_eq!(
            scaled((401, 301), 50.0),
            (201, 151),
            "rounds rather than loses a row"
        );
        assert_eq!(scaled((400, 300), 0.0), (1, 1));
        assert_eq!(scaled((400, 300), -10.0), (1, 1));
    }

    #[test]
    fn the_size_of_a_whole_edit_is_the_operations_in_order() {
        let source = (400, 300);
        let mut edit = Edit::default();
        assert!(edit.is_identity());
        assert_eq!(edit.size_of(source), source);

        edit.crop = Some(Crop {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        });
        assert_eq!(edit.size_of(source), (200, 100));

        // The turn applies to what is left after the crop.
        edit.transform.turn_right();
        assert_eq!(edit.size_of(source), (100, 200));
        assert_eq!(edit.size_before_resize(source), (100, 200));

        // And a resize is the last word on the size.
        edit.resize = Some((50, 50));
        assert_eq!(edit.size_of(source), (50, 50));
        assert_eq!(
            edit.size_before_resize(source),
            (100, 200),
            "what the resize box counts from"
        );
        assert!(!edit.is_identity());
    }

    #[test]
    fn what_was_done_is_said_in_words() {
        let source = (400, 300);
        assert_eq!(Edit::default().describe(source), "");

        let edit = Edit {
            crop: Some(Crop {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
            }),
            transform: Transform {
                turn: Turn::Right,
                flip_h: false,
                flip_v: true,
            },
            resize: Some((50, 25)),
        };
        assert_eq!(
            edit.describe(source),
            "cropped to 200x100, turned 90°, mirrored, resized to 50x25"
        );
    }

    #[test]
    fn what_was_done_is_named_the_way_the_button_that_did_it_was() {
        // Turn, then press Mirror. The stored flip is the vertical one - and
        // the line under the picture still has to read "mirrored", because
        // that is the button that was pressed and that is what is on screen.
        let mut transform = Transform::default();
        transform.turn_right();
        transform.flip_horizontal();
        assert!(transform.flip_v, "stored against the source");
        assert!(transform.mirrored_on_screen());
        assert!(!transform.flipped_on_screen());

        let edit = Edit {
            transform,
            ..Default::default()
        };
        assert_eq!(edit.describe((400, 300)), "turned 90°, mirrored");

        // And the same for Flip, whatever the turn.
        for turn in [Turn::None, Turn::Right, Turn::Half, Turn::Left] {
            let mut transform = Transform {
                turn,
                ..Default::default()
            };
            transform.flip_vertical();
            assert!(transform.flipped_on_screen(), "{turn:?}");
            assert!(!transform.mirrored_on_screen(), "{turn:?}");
        }
    }

    #[test]
    fn saving_over_a_jpeg_costs_a_generation_and_says_so() {
        assert!(is_lossy("jpg"));
        assert!(is_lossy("JPEG"));
        assert!(is_lossy("webp"));
        assert!(!is_lossy("png"));
        assert!(!is_lossy("bmp"));
        assert!(!is_lossy(""));
    }

    #[test]
    fn a_format_that_cannot_hold_the_picture_says_so_before_the_work() {
        // An ICO stores each dimension in one byte. Finding that out after
        // five minutes of cropping is finding it out too late.
        assert!(refuses("ico", (300, 100)).is_some());
        assert!(refuses("ICO", (100, 300)).is_some());
        assert_eq!(refuses("ico", (256, 256)), None);
        assert_eq!(refuses("png", (4000, 4000)), None);

        let complaint = refuses("ico", (300, 300)).expect("a complaint");
        assert!(complaint.contains("300x300"), "{complaint}");
        assert!(complaint.contains(".png"), "and what to do instead");
    }

    #[test]
    fn what_a_write_leaves_behind_is_listed_before_it_happens() {
        // Nothing to say about a plain, still PNG.
        assert!(losses("png", Carries::default()).is_empty());

        // An animation becomes a still. This is the one that is easy to miss
        // afterwards and impossible to undo.
        let animated = losses(
            "gif",
            Carries {
                animated: true,
                metadata: false,
            },
        );
        assert_eq!(animated.len(), 1);
        assert!(animated[0].contains("animation"), "{animated:?}");
        // And no count. The detection stops at two frames, so any number here
        // would be the number two whatever the file actually held.
        assert!(
            !animated[0].chars().any(|c| c.is_ascii_digit()),
            "a number it never worked out: {animated:?}"
        );

        // A photograph loses its own record of itself.
        let photo = losses(
            "jpg",
            Carries {
                animated: false,
                metadata: true,
            },
        );
        assert_eq!(photo.len(), 2, "metadata and the re-encode: {photo:?}");
        assert!(photo.iter().any(|l| l.contains("EXIF")));
        assert!(photo.iter().any(|l| l.contains("generation of quality")));
    }
}

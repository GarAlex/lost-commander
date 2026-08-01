//! File-type icons drawn as vector shapes.
//!
//! Deliberately *not* glyphs or an image atlas: the icons are painted from
//! primitives, so they stay crisp at any size and any DPI, need no assets
//! shipped alongside the binary, and cannot fall back to tofu boxes when a
//! font is missing. Colour carries the type at a glance; the silhouette
//! confirms it.

use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Vec2};
use eframe::epaint::Shape;

// Which bucket a file falls into is the engine's decision, not this view's:
// the other front-end asks the same question and answers it with the shell's
// own icons rather than with these shapes. What is left here is only the
// painting, and the colour, which is the part that belongs to a theme.
pub use rust_commander_core::filekind::{classify, Kind};

/// What colour a kind is drawn in.
///
/// From the palette, so a theme carries the icons with it - the icons are
/// where this view puts its colour, and a theme that left them behind would
/// only be half a theme.
///
/// A free function and not a method on `Kind`, because `Kind` belongs to the
/// engine and only its own crate may hang inherent methods on it. That is the
/// rule working rather than getting in the way: what colour something is
/// drawn in is this view's opinion, and it reads as one here.
pub fn colour(kind: Kind) -> Color32 {
    let palette = super::theme::palette();
    match kind {
        Kind::Parent => palette.icon_parent,
        Kind::Folder => palette.icon_folder,
        Kind::Image => palette.icon_image,
        Kind::Code => palette.icon_code,
        Kind::Archive => palette.icon_archive,
        Kind::Audio => palette.icon_audio,
        Kind::Video => palette.icon_video,
        Kind::Document => palette.icon_document,
        Kind::Binary => palette.icon_binary,
        Kind::Plain => palette.icon_plain,
    }
}

/// Paint `kind` to fill `rect`. The shapes are laid out in a unit square and
/// scaled, so one implementation serves both the 18px list rows and the 44px
/// grid tiles.
pub fn draw(painter: &Painter, rect: Rect, kind: Kind, dimmed: bool) {
    let colour = if dimmed {
        colour(kind).gamma_multiply(0.55)
    } else {
        colour(kind)
    };
    // Work in a centred square so non-square slots still look right.
    let side = rect.width().min(rect.height());
    let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let p = |x: f32, y: f32| Pos2::new(square.min.x + x * side, square.min.y + y * side);
    let radius = CornerRadius::same((side * 0.10).round().clamp(1.0, 6.0) as u8);

    match kind {
        Kind::Parent => {
            // An upward arrow: leaving is a direction, not a file type.
            painter.add(Shape::convex_polygon(
                vec![p(0.50, 0.18), p(0.86, 0.54), p(0.14, 0.54)],
                colour,
                Stroke::NONE,
            ));
            painter.rect_filled(
                Rect::from_min_max(p(0.36, 0.54), p(0.64, 0.84)),
                CornerRadius::same(1),
                colour,
            );
        }

        Kind::Folder => {
            // Tab, then body: the classic silhouette reads instantly.
            painter.rect_filled(
                Rect::from_min_max(p(0.08, 0.20), p(0.46, 0.30)),
                CornerRadius::same(2),
                colour.gamma_multiply(0.8),
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.08, 0.26), p(0.92, 0.80)),
                radius,
                colour,
            );
        }

        Kind::Image => {
            sheet(painter, square, p, radius, colour);
            // A sun over a ridge line, inside the sheet.
            painter.circle_filled(
                p(0.38, 0.42),
                side * 0.06,
                Color32::WHITE.gamma_multiply(0.9),
            );
            painter.add(Shape::convex_polygon(
                vec![p(0.24, 0.68), p(0.46, 0.46), p(0.68, 0.68)],
                Color32::WHITE.gamma_multiply(0.75),
                Stroke::NONE,
            ));
            painter.add(Shape::convex_polygon(
                vec![p(0.52, 0.68), p(0.68, 0.52), p(0.80, 0.68)],
                Color32::WHITE.gamma_multiply(0.55),
                Stroke::NONE,
            ));
        }

        Kind::Code => {
            sheet(painter, square, p, radius, colour);
            let ink = Stroke::new((side * 0.055).max(1.0), Color32::WHITE.gamma_multiply(0.85));
            // < and > chevrons.
            painter.line_segment([p(0.40, 0.40), p(0.28, 0.55)], ink);
            painter.line_segment([p(0.28, 0.55), p(0.40, 0.70)], ink);
            painter.line_segment([p(0.60, 0.40), p(0.72, 0.55)], ink);
            painter.line_segment([p(0.72, 0.55), p(0.60, 0.70)], ink);
        }

        Kind::Archive => {
            painter.rect_filled(
                Rect::from_min_max(p(0.18, 0.16), p(0.82, 0.84)),
                radius,
                colour,
            );
            // Zip band and pull tab.
            painter.rect_filled(
                Rect::from_min_max(p(0.46, 0.16), p(0.54, 0.84)),
                CornerRadius::ZERO,
                Color32::WHITE.gamma_multiply(0.5),
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.43, 0.44), p(0.57, 0.60)),
                CornerRadius::same(2),
                Color32::WHITE.gamma_multiply(0.85),
            );
        }

        Kind::Audio => {
            // Note head plus stem.
            painter.circle_filled(p(0.38, 0.70), side * 0.14, colour);
            painter.rect_filled(
                Rect::from_min_max(p(0.49, 0.20), p(0.56, 0.72)),
                CornerRadius::same(1),
                colour,
            );
            painter.add(Shape::convex_polygon(
                vec![p(0.56, 0.20), p(0.80, 0.28), p(0.80, 0.40), p(0.56, 0.32)],
                colour,
                Stroke::NONE,
            ));
        }

        Kind::Video => {
            painter.rect_filled(
                Rect::from_min_max(p(0.12, 0.24), p(0.88, 0.76)),
                radius,
                colour,
            );
            painter.add(Shape::convex_polygon(
                vec![p(0.42, 0.38), p(0.66, 0.50), p(0.42, 0.62)],
                Color32::WHITE.gamma_multiply(0.9),
                Stroke::NONE,
            ));
        }

        Kind::Document => {
            sheet(painter, square, p, radius, colour);
            let ink = Stroke::new((side * 0.045).max(1.0), Color32::WHITE.gamma_multiply(0.75));
            for (i, y) in [0.44f32, 0.56, 0.68].iter().enumerate() {
                let right = if i == 2 { 0.62 } else { 0.74 };
                painter.line_segment([p(0.28, *y), p(right, *y)], ink);
            }
        }

        Kind::Binary => {
            painter.rect_filled(
                Rect::from_min_max(p(0.20, 0.20), p(0.80, 0.80)),
                radius,
                colour,
            );
            // Pin legs, so it reads as a chip rather than a plain box.
            let leg = Stroke::new((side * 0.05).max(1.0), colour);
            for y in [0.34f32, 0.50, 0.66] {
                painter.line_segment([p(0.10, y), p(0.20, y)], leg);
                painter.line_segment([p(0.80, y), p(0.90, y)], leg);
            }
        }

        Kind::Plain => {
            sheet(painter, square, p, radius, colour);
        }
    }
}

/// Toolbar symbols. These are shapes for the same reason the file icons are:
/// the arrows and glyphs that would otherwise be used (`←`, `↻`, `▦`) are not
/// in the default font and render as empty boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Up,
    TreeView,
    Reload,
    ListView,
    GridView,
    Copy,
    Trash,
    Star,
    Sidebar,
    TwoPanes,
    QuickView,
    Move,
    Select,
}

pub fn draw_tool(painter: &Painter, rect: Rect, tool: Tool, colour: Color32) {
    let side = rect.width().min(rect.height());
    let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let p = |x: f32, y: f32| Pos2::new(square.min.x + x * side, square.min.y + y * side);
    let line = Stroke::new((side * 0.09).max(1.2), colour);

    match tool {
        Tool::Up => {
            painter.add(Shape::convex_polygon(
                vec![p(0.50, 0.18), p(0.80, 0.50), p(0.20, 0.50)],
                colour,
                Stroke::NONE,
            ));
            painter.rect_filled(
                Rect::from_min_max(p(0.42, 0.48), p(0.58, 0.82)),
                CornerRadius::same(1),
                colour,
            );
        }
        Tool::Reload => {
            // Three-quarter ring with an arrowhead, drawn as a polyline.
            let centre = p(0.50, 0.52);
            let radius = side * 0.28;
            let points: Vec<Pos2> = (0..=26)
                .map(|i| {
                    let t = 0.6 + (i as f32 / 26.0) * (std::f32::consts::TAU * 0.82);
                    Pos2::new(centre.x + radius * t.cos(), centre.y + radius * t.sin())
                })
                .collect();
            painter.add(Shape::line(points, line));
            painter.add(Shape::convex_polygon(
                vec![p(0.62, 0.10), p(0.82, 0.30), p(0.56, 0.34)],
                colour,
                Stroke::NONE,
            ));
        }
        Tool::ListView => {
            for y in [0.30f32, 0.50, 0.70] {
                painter.rect_filled(
                    Rect::from_min_max(p(0.18, y - 0.045), p(0.82, y + 0.045)),
                    CornerRadius::same(1),
                    colour,
                );
            }
        }
        Tool::TreeView => {
            // A trunk with two branches, each ending in a node.
            let trunk = Stroke::new((side * 0.07).max(1.0), colour);
            painter.line_segment([p(0.26, 0.16), p(0.26, 0.74)], trunk);
            painter.line_segment([p(0.26, 0.38), p(0.52, 0.38)], trunk);
            painter.line_segment([p(0.26, 0.72), p(0.52, 0.72)], trunk);
            painter.rect_filled(
                Rect::from_min_max(p(0.16, 0.10), p(0.36, 0.24)),
                CornerRadius::same(1),
                colour,
            );
            for y in [0.31f32, 0.65] {
                painter.rect_filled(
                    Rect::from_min_max(p(0.54, y), p(0.84, y + 0.14)),
                    CornerRadius::same(1),
                    colour,
                );
            }
        }
        Tool::GridView => {
            for (x, y) in [(0.20f32, 0.20f32), (0.56, 0.20), (0.20, 0.56), (0.56, 0.56)] {
                painter.rect_filled(
                    Rect::from_min_max(p(x, y), p(x + 0.24, y + 0.24)),
                    CornerRadius::same(1),
                    colour,
                );
            }
        }
        Tool::Copy => {
            // Two offset pages.
            painter.rect_filled(
                Rect::from_min_max(p(0.18, 0.14), p(0.62, 0.66)),
                CornerRadius::same(2),
                colour.gamma_multiply(0.55),
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.38, 0.34), p(0.82, 0.86)),
                CornerRadius::same(2),
                colour,
            );
        }
        Tool::Trash => {
            // Lid, handle, bin, ribs.
            painter.rect_filled(
                Rect::from_min_max(p(0.16, 0.22), p(0.84, 0.32)),
                CornerRadius::same(1),
                colour,
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.40, 0.12), p(0.60, 0.22)),
                CornerRadius::same(1),
                colour,
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.24, 0.32), p(0.76, 0.86)),
                CornerRadius::same(2),
                colour.gamma_multiply(0.85),
            );
            let rib = Stroke::new((side * 0.05).max(1.0), Color32::BLACK.gamma_multiply(0.35));
            for x in [0.38f32, 0.50, 0.62] {
                painter.line_segment([p(x, 0.42), p(x, 0.76)], rib);
            }
        }
        Tool::Star => {
            let centre = p(0.50, 0.52);
            let outer = side * 0.34;
            let inner = side * 0.15;
            let mut points = Vec::with_capacity(10);
            for i in 0..10 {
                let radius = if i % 2 == 0 { outer } else { inner };
                let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                points.push(Pos2::new(
                    centre.x + radius * angle.cos(),
                    centre.y + radius * angle.sin(),
                ));
            }
            // A star is concave, so it needs the general path, not convex_polygon.
            painter.add(Shape::Path(eframe::epaint::PathShape {
                points,
                closed: true,
                fill: colour,
                stroke: Stroke::NONE.into(),
            }));
        }
        Tool::Sidebar => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.14, 0.20), p(0.86, 0.80)),
                CornerRadius::same(2),
                line,
                eframe::egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_min_max(p(0.14, 0.20), p(0.40, 0.80)),
                CornerRadius::same(2),
                colour,
            );
        }
        // An arrow into a tray: move, as against copy's two sheets.
        Tool::Move => {
            painter.add(Shape::convex_polygon(
                vec![p(0.44, 0.20), p(0.80, 0.44), p(0.44, 0.68)],
                colour,
                Stroke::NONE,
            ));
            painter.line_segment([p(0.16, 0.44), p(0.52, 0.44)], line);
            painter.line_segment([p(0.16, 0.82), p(0.84, 0.82)], line);
        }
        // A tick in a box: the selection menu.
        Tool::Select => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.14, 0.16), p(0.86, 0.84)),
                CornerRadius::same(2),
                line,
                eframe::egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.30, 0.52), p(0.44, 0.66)], line);
            painter.line_segment([p(0.44, 0.66), p(0.72, 0.32)], line);
        }
        // A picture in a frame: a hill and a sun, the universal shorthand.
        Tool::QuickView => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.12, 0.20), p(0.88, 0.80)),
                CornerRadius::same(2),
                line,
                eframe::egui::StrokeKind::Inside,
            );
            painter.add(Shape::convex_polygon(
                vec![p(0.20, 0.72), p(0.42, 0.42), p(0.64, 0.72)],
                colour,
                Stroke::NONE,
            ));
            painter.add(Shape::convex_polygon(
                vec![p(0.54, 0.72), p(0.70, 0.52), p(0.84, 0.72)],
                colour,
                Stroke::NONE,
            ));
            painter.circle_filled(p(0.68, 0.34), side * 0.07, colour);
        }
        // Two panes side by side; the toggle that folds the second one away.
        Tool::TwoPanes => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.10, 0.20), p(0.47, 0.80)),
                CornerRadius::same(2),
                line,
                eframe::egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_min_max(p(0.53, 0.20), p(0.90, 0.80)),
                CornerRadius::same(2),
                line,
                eframe::egui::StrokeKind::Inside,
            );
        }
    }
}

/// A page with a folded corner, shared by the sheet-like icons.
fn sheet(
    painter: &Painter,
    square: Rect,
    p: impl Fn(f32, f32) -> Pos2,
    radius: CornerRadius,
    colour: Color32,
) {
    let _ = square;
    painter.rect_filled(
        Rect::from_min_max(p(0.22, 0.14), p(0.78, 0.86)),
        radius,
        colour,
    );
    // The fold, drawn as a lighter triangle in the top-right.
    painter.add(Shape::convex_polygon(
        vec![p(0.60, 0.14), p(0.78, 0.32), p(0.60, 0.32)],
        Color32::BLACK.gamma_multiply(0.28),
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    // What `classify` answers is tested beside it, in `rust_commander_core::filekind`. What
    // is left to check here is the part this module actually owns: that the
    // theme gives every kind its own colour, since colour is what carries the
    // type at a glance and two kinds sharing one would silently merge them.
    #[test]
    fn every_kind_has_a_distinct_colour() {
        let kinds = [
            Kind::Parent,
            Kind::Folder,
            Kind::Image,
            Kind::Code,
            Kind::Archive,
            Kind::Audio,
            Kind::Video,
            Kind::Document,
            Kind::Binary,
            Kind::Plain,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(colour(*a), colour(*b), "{a:?} and {b:?} share a colour");
            }
        }
    }
}

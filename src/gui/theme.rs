//! The graphical palette, and the styling built from it.
//!
//! Every colour the view uses lives in one [`Palette`], so all of them can be
//! changed at once by picking a preset, or one at a time from the theme form.
//! Nothing anywhere else holds a colour of its own.
//!
//! The current palette is a thread-local rather than a lock: the drawing code
//! asks for colours thousands of times a frame and all of it runs on the one
//! thread, so a lock would be paid for constantly and never contended.

use std::cell::Cell;

use eframe::egui::{self, Color32, CornerRadius, Stroke};
use serde::{Deserialize, Serialize};

/// Every colour, in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Palette {
    // ---- the window itself
    #[serde(with = "hex")]
    pub bg: Color32,
    #[serde(with = "hex")]
    pub surface: Color32,
    #[serde(with = "hex")]
    pub surface_hi: Color32,
    #[serde(with = "hex")]
    pub sidebar: Color32,
    #[serde(with = "hex")]
    pub border: Color32,

    // ---- text
    #[serde(with = "hex")]
    pub text: Color32,
    #[serde(with = "hex")]
    pub text_dim: Color32,
    #[serde(with = "hex")]
    pub text_faint: Color32,

    // ---- the cursor, the marks, and what is hovered
    #[serde(with = "hex")]
    pub accent: Color32,
    #[serde(with = "hex")]
    pub accent_dim: Color32,
    #[serde(with = "hex")]
    pub selected: Color32,
    #[serde(with = "hex")]
    pub selected_idle: Color32,
    #[serde(with = "hex")]
    pub hover: Color32,
    #[serde(with = "hex")]
    pub marked: Color32,
    #[serde(with = "hex")]
    pub marked_cursor: Color32,
    #[serde(with = "hex")]
    pub marked_text: Color32,

    // ---- the tab strip
    /// A tab that is not the one on show.
    #[serde(with = "hex")]
    pub tab: Color32,
    /// The one that is.
    #[serde(with = "hex")]
    pub tab_active: Color32,

    // ---- what happened
    #[serde(with = "hex")]
    pub ok: Color32,
    #[serde(with = "hex")]
    pub danger: Color32,

    // ---- the file-type icons, which is where this view carries its colour
    #[serde(with = "hex")]
    pub icon_parent: Color32,
    #[serde(with = "hex")]
    pub icon_folder: Color32,
    #[serde(with = "hex")]
    pub icon_image: Color32,
    #[serde(with = "hex")]
    pub icon_code: Color32,
    #[serde(with = "hex")]
    pub icon_archive: Color32,
    #[serde(with = "hex")]
    pub icon_audio: Color32,
    #[serde(with = "hex")]
    pub icon_video: Color32,
    #[serde(with = "hex")]
    pub icon_document: Color32,
    #[serde(with = "hex")]
    pub icon_binary: Color32,
    #[serde(with = "hex")]
    pub icon_plain: Color32,
}

impl Default for Palette {
    fn default() -> Self {
        Palette::midnight()
    }
}

/// A preset: the name the picker shows, and the palette it builds.
pub type Preset = (&'static str, fn() -> Palette);

/// The presets, in the order the picker offers them.
pub const PRESETS: &[Preset] = &[
    ("Midnight", Palette::midnight),
    ("Commander", Palette::commander),
    ("Paper", Palette::paper),
];

pub fn preset(name: &str) -> Option<Palette> {
    PRESETS
        .iter()
        .find(|(preset, _)| preset.eq_ignore_ascii_case(name))
        .map(|(_, build)| build())
}

/// The name of the preset a palette matches exactly, if any.
pub fn preset_name(palette: &Palette) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(_, build)| build() == *palette)
        .map(|(name, _)| *name)
}

impl Palette {
    /// A quiet dark surface with one accent. The default.
    ///
    /// Deliberately not the Commander blue: this view is not trying to look
    /// like a terminal, and the file-type icons carry the colour rather than
    /// competing with the chrome.
    pub const fn midnight() -> Palette {
        Palette {
            bg: Color32::from_rgb(0x16, 0x19, 0x1E),
            surface: Color32::from_rgb(0x1D, 0x21, 0x28),
            surface_hi: Color32::from_rgb(0x25, 0x2A, 0x34),
            sidebar: Color32::from_rgb(0x12, 0x15, 0x19),
            border: Color32::from_rgb(0x2B, 0x31, 0x3C),

            text: Color32::from_rgb(0xE4, 0xE8, 0xEF),
            text_dim: Color32::from_rgb(0x8B, 0x94, 0xA6),
            text_faint: Color32::from_rgb(0x5E, 0x67, 0x76),

            accent: Color32::from_rgb(0x4C, 0x8D, 0xFF),
            accent_dim: Color32::from_rgb(0x2A, 0x47, 0x7E),
            selected: Color32::from_rgb(0x2A, 0x47, 0x7E),
            selected_idle: Color32::from_rgb(0x24, 0x2A, 0x36),
            tab: Color32::from_rgb(0x1D, 0x21, 0x28),
            tab_active: Color32::from_rgb(0x2A, 0x47, 0x7E),
            hover: Color32::from_rgb(0x25, 0x2A, 0x34),
            marked: Color32::from_rgb(0x4A, 0x39, 0x18),
            marked_cursor: Color32::from_rgb(0x74, 0x59, 0x22),
            marked_text: Color32::from_rgb(0xF5, 0xC9, 0x7A),

            ok: Color32::from_rgb(0x4F, 0xC1, 0x9A),
            danger: Color32::from_rgb(0xE9, 0x6A, 0x7B),

            icon_parent: Color32::from_rgb(0x8B, 0x94, 0xA6),
            icon_folder: Color32::from_rgb(0xF2, 0xB4, 0x4C),
            icon_image: Color32::from_rgb(0x4F, 0xC1, 0x9A),
            icon_code: Color32::from_rgb(0x6E, 0x9E, 0xF5),
            icon_archive: Color32::from_rgb(0xB4, 0x86, 0xE8),
            icon_audio: Color32::from_rgb(0xF0, 0x92, 0x6B),
            icon_video: Color32::from_rgb(0xE9, 0x6A, 0x7B),
            icon_document: Color32::from_rgb(0x7E, 0xC8, 0xE3),
            icon_binary: Color32::from_rgb(0x9E, 0xA8, 0xB8),
            icon_plain: Color32::from_rgb(0xB8, 0xC0, 0xCE),
        }
    }

    /// The one this program is named after: blue panels, cyan text, yellow
    /// marks. For anyone who wants their file manager to look like 1986.
    pub const fn commander() -> Palette {
        Palette {
            bg: Color32::from_rgb(0x00, 0x00, 0x9C),
            surface: Color32::from_rgb(0x00, 0x00, 0xA8),
            surface_hi: Color32::from_rgb(0x00, 0x14, 0xC0),
            sidebar: Color32::from_rgb(0x00, 0x00, 0x80),
            border: Color32::from_rgb(0x4C, 0xD3, 0xD3),

            text: Color32::from_rgb(0x5A, 0xE6, 0xE6),
            text_dim: Color32::from_rgb(0x3E, 0xB8, 0xC4),
            text_faint: Color32::from_rgb(0x2F, 0x8E, 0xA0),

            accent: Color32::from_rgb(0xF5, 0xF5, 0x5A),
            accent_dim: Color32::from_rgb(0x00, 0x82, 0x82),
            // The cursor bar was cyan in the original.
            selected: Color32::from_rgb(0x00, 0x9C, 0x9C),
            selected_idle: Color32::from_rgb(0x00, 0x4E, 0x66),
            tab: Color32::from_rgb(0x00, 0x00, 0xA8),
            tab_active: Color32::from_rgb(0x00, 0x9C, 0x9C),
            hover: Color32::from_rgb(0x00, 0x1E, 0xC8),
            marked: Color32::from_rgb(0x7A, 0x5E, 0x00),
            marked_cursor: Color32::from_rgb(0xA8, 0x84, 0x00),
            marked_text: Color32::from_rgb(0xFF, 0xF9, 0x7A),

            ok: Color32::from_rgb(0x5A, 0xF5, 0x8C),
            danger: Color32::from_rgb(0xFF, 0x6E, 0x6E),

            icon_parent: Color32::from_rgb(0x8F, 0xD8, 0xE0),
            icon_folder: Color32::from_rgb(0xF5, 0xF5, 0x5A),
            icon_image: Color32::from_rgb(0x5A, 0xF5, 0x8C),
            icon_code: Color32::from_rgb(0x9C, 0xD8, 0xFF),
            icon_archive: Color32::from_rgb(0xE0, 0x9C, 0xFF),
            icon_audio: Color32::from_rgb(0xFF, 0xC1, 0x7A),
            icon_video: Color32::from_rgb(0xFF, 0x9C, 0x9C),
            icon_document: Color32::from_rgb(0xBF, 0xEE, 0xF5),
            icon_binary: Color32::from_rgb(0xC0, 0xC8, 0xD8),
            icon_plain: Color32::from_rgb(0xDE, 0xE6, 0xF0),
        }
    }

    /// Light, for a bright room or a projector.
    pub const fn paper() -> Palette {
        Palette {
            bg: Color32::from_rgb(0xEF, 0xF1, 0xF4),
            surface: Color32::from_rgb(0xFA, 0xFB, 0xFD),
            surface_hi: Color32::from_rgb(0xE6, 0xE9, 0xEF),
            sidebar: Color32::from_rgb(0xE4, 0xE7, 0xEC),
            border: Color32::from_rgb(0xC8, 0xCE, 0xD8),

            text: Color32::from_rgb(0x1C, 0x21, 0x2A),
            text_dim: Color32::from_rgb(0x55, 0x5E, 0x6C),
            text_faint: Color32::from_rgb(0x8A, 0x93, 0xA1),

            accent: Color32::from_rgb(0x1E, 0x66, 0xD0),
            accent_dim: Color32::from_rgb(0xC2, 0xD8, 0xF7),
            selected: Color32::from_rgb(0xBF, 0xD6, 0xF7),
            selected_idle: Color32::from_rgb(0xDD, 0xE3, 0xEC),
            tab: Color32::from_rgb(0xE6, 0xE9, 0xEF),
            tab_active: Color32::from_rgb(0xBF, 0xD6, 0xF7),
            hover: Color32::from_rgb(0xE2, 0xE7, 0xEF),
            marked: Color32::from_rgb(0xFA, 0xE4, 0xB0),
            marked_cursor: Color32::from_rgb(0xF2, 0xCC, 0x74),
            marked_text: Color32::from_rgb(0x6B, 0x4A, 0x00),

            ok: Color32::from_rgb(0x14, 0x7D, 0x54),
            danger: Color32::from_rgb(0xC4, 0x2B, 0x3E),

            icon_parent: Color32::from_rgb(0x6C, 0x76, 0x86),
            icon_folder: Color32::from_rgb(0xD9, 0x93, 0x0B),
            icon_image: Color32::from_rgb(0x18, 0x8A, 0x66),
            icon_code: Color32::from_rgb(0x1E, 0x66, 0xD0),
            icon_archive: Color32::from_rgb(0x7B, 0x45, 0xC4),
            icon_audio: Color32::from_rgb(0xC4, 0x6A, 0x1E),
            icon_video: Color32::from_rgb(0xC4, 0x2B, 0x3E),
            icon_document: Color32::from_rgb(0x1B, 0x77, 0x99),
            icon_binary: Color32::from_rgb(0x64, 0x6E, 0x7C),
            icon_plain: Color32::from_rgb(0x4C, 0x55, 0x62),
        }
    }

    /// The fields, grouped the way the form shows them.
    ///
    /// One list drives both the form and the tests, so a colour added to the
    /// palette and forgotten in the form is a test failure rather than a
    /// setting nobody can reach.
    pub fn sections() -> &'static [(&'static str, &'static [(&'static str, Field)])] {
        use Field::*;
        &[
            (
                "Window",
                &[
                    ("Background", Bg),
                    ("Panels", Surface),
                    ("Raised", SurfaceHi),
                    ("Sidebar", Sidebar),
                    ("Borders", Border),
                ],
            ),
            (
                "Text",
                &[("Normal", Text), ("Dimmed", TextDim), ("Faint", TextFaint)],
            ),
            (
                "Cursor and marks",
                &[
                    ("Accent", Accent),
                    ("Accent, muted", AccentDim),
                    ("Cursor", Selected),
                    ("Cursor, unfocused", SelectedIdle),
                    ("Hover", Hover),
                    ("Marked", Marked),
                    ("Marked under cursor", MarkedCursor),
                    ("Marked text", MarkedText),
                ],
            ),
            ("Tabs", &[("Resting", Tab), ("On show", TabActive)]),
            ("Messages", &[("Success", Ok), ("Failure", Danger)]),
            (
                "File icons",
                &[
                    ("Parent", IconParent),
                    ("Folder", IconFolder),
                    ("Image", IconImage),
                    ("Code", IconCode),
                    ("Archive", IconArchive),
                    ("Audio", IconAudio),
                    ("Video", IconVideo),
                    ("Document", IconDocument),
                    ("Binary", IconBinary),
                    ("Plain", IconPlain),
                ],
            ),
        ]
    }

    pub fn get(&self, field: Field) -> Color32 {
        use Field::*;
        match field {
            Bg => self.bg,
            Surface => self.surface,
            SurfaceHi => self.surface_hi,
            Sidebar => self.sidebar,
            Border => self.border,
            Text => self.text,
            TextDim => self.text_dim,
            TextFaint => self.text_faint,
            Accent => self.accent,
            AccentDim => self.accent_dim,
            Selected => self.selected,
            SelectedIdle => self.selected_idle,
            Hover => self.hover,
            Marked => self.marked,
            MarkedCursor => self.marked_cursor,
            MarkedText => self.marked_text,
            Tab => self.tab,
            TabActive => self.tab_active,
            Ok => self.ok,
            Danger => self.danger,
            IconParent => self.icon_parent,
            IconFolder => self.icon_folder,
            IconImage => self.icon_image,
            IconCode => self.icon_code,
            IconArchive => self.icon_archive,
            IconAudio => self.icon_audio,
            IconVideo => self.icon_video,
            IconDocument => self.icon_document,
            IconBinary => self.icon_binary,
            IconPlain => self.icon_plain,
        }
    }

    pub fn set(&mut self, field: Field, colour: Color32) {
        use Field::*;
        let slot = match field {
            Bg => &mut self.bg,
            Surface => &mut self.surface,
            SurfaceHi => &mut self.surface_hi,
            Sidebar => &mut self.sidebar,
            Border => &mut self.border,
            Text => &mut self.text,
            TextDim => &mut self.text_dim,
            TextFaint => &mut self.text_faint,
            Accent => &mut self.accent,
            AccentDim => &mut self.accent_dim,
            Selected => &mut self.selected,
            SelectedIdle => &mut self.selected_idle,
            Hover => &mut self.hover,
            Marked => &mut self.marked,
            MarkedCursor => &mut self.marked_cursor,
            MarkedText => &mut self.marked_text,
            Tab => &mut self.tab,
            TabActive => &mut self.tab_active,
            Ok => &mut self.ok,
            Danger => &mut self.danger,
            IconParent => &mut self.icon_parent,
            IconFolder => &mut self.icon_folder,
            IconImage => &mut self.icon_image,
            IconCode => &mut self.icon_code,
            IconArchive => &mut self.icon_archive,
            IconAudio => &mut self.icon_audio,
            IconVideo => &mut self.icon_video,
            IconDocument => &mut self.icon_document,
            IconBinary => &mut self.icon_binary,
            IconPlain => &mut self.icon_plain,
        };
        *slot = colour;
    }
}

/// One addressable colour, so the form can be a loop rather than 28 copies of
/// the same four lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Bg,
    Surface,
    SurfaceHi,
    Sidebar,
    Border,
    Text,
    TextDim,
    TextFaint,
    Accent,
    AccentDim,
    Selected,
    SelectedIdle,
    Hover,
    Marked,
    MarkedCursor,
    MarkedText,
    Tab,
    TabActive,
    Ok,
    Danger,
    IconParent,
    IconFolder,
    IconImage,
    IconCode,
    IconArchive,
    IconAudio,
    IconVideo,
    IconDocument,
    IconBinary,
    IconPlain,
}

/// `#rrggbb`, the form every other program writes colours in.
pub fn to_hex(colour: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", colour.r(), colour.g(), colour.b())
}

/// Parse `#rrggbb`, `rrggbb` or the three-digit short form.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let text = text.trim().trim_start_matches('#');
    let digits: Option<Vec<u8>> = text
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect();
    let digits = digits?;
    match digits.len() {
        // #abc means #aabbcc, as it does in CSS.
        3 => Some(Color32::from_rgb(
            digits[0] * 17,
            digits[1] * 17,
            digits[2] * 17,
        )),
        6 => Some(Color32::from_rgb(
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        )),
        _ => None,
    }
}

mod hex {
    use super::{parse_hex, to_hex};
    use eframe::egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(colour: &Color32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&to_hex(*colour))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let text = String::deserialize(d)?;
        parse_hex(&text).ok_or_else(|| serde::de::Error::custom(format!("bad colour: {text}")))
    }
}

thread_local! {
    /// The palette the drawing code reads. A `Cell` rather than a lock: this
    /// is read thousands of times a frame from the one thread that draws.
    static CURRENT: Cell<Palette> = const { Cell::new(Palette::midnight()) };
}

pub fn palette() -> Palette {
    CURRENT.with(|current| current.get())
}

pub fn set_palette(palette: Palette) {
    CURRENT.with(|current| current.set(palette));
}

// The accessors the drawing code uses. Functions rather than constants, since
// they now answer differently depending on the theme.
macro_rules! colour {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}
colour!(
    bg,
    surface,
    surface_hi,
    sidebar,
    border,
    text,
    text_dim,
    text_faint,
    accent,
    accent_dim,
    selected,
    selected_idle,
    hover,
    marked,
    marked_cursor,
    marked_text,
    tab,
    tab_active,
    ok,
    danger,
);

/// The palette a settings file asks for.
///
/// A full palette wins over a preset name, and anything unreadable falls back
/// to the default rather than refusing to start.
pub fn from_settings(settings: &crate::config::Settings) -> Palette {
    settings
        .palette
        .or_else(|| settings.theme.as_deref().and_then(preset))
        .unwrap_or_default()
}

/// Record a palette in the settings, as a name where it has one.
///
/// A preset is stored by name so that a later version of that preset is
/// picked up, rather than frozen as whatever it was the day it was chosen.
pub fn into_settings(palette: Palette, settings: &mut crate::config::Settings) {
    match preset_name(&palette) {
        Some(name) => {
            settings.theme = Some(name.to_string());
            settings.palette = None;
        }
        None => {
            settings.theme = None;
            settings.palette = Some(palette);
        }
    }
}

/// Whether the palette is a dark one, which decides which of egui's own
/// widget defaults go underneath.
pub fn is_dark(palette: &Palette) -> bool {
    let bg = palette.bg;
    (bg.r() as u32 + bg.g() as u32 + bg.b() as u32) < 3 * 128
}

pub fn apply(ctx: &egui::Context) {
    let palette = palette();
    let mut style = (*ctx.style()).clone();

    // A light palette needs egui's light defaults underneath, or its shadows
    // and disabled colours stay tuned for a dark background.
    let dark = is_dark(&palette);
    style.visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    style.visuals.dark_mode = dark;
    style.visuals.panel_fill = palette.bg;
    style.visuals.window_fill = palette.surface;
    style.visuals.extreme_bg_color = palette.sidebar;
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.window_stroke = Stroke::new(1.0, palette.border);

    let widgets = &mut style.visuals.widgets;
    widgets.noninteractive.bg_fill = palette.surface;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text_dim);
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);

    widgets.inactive.bg_fill = palette.surface_hi;
    widgets.inactive.weak_bg_fill = palette.surface_hi;
    widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    widgets.inactive.corner_radius = CornerRadius::same(6);

    widgets.hovered.bg_fill = palette.hover;
    widgets.hovered.weak_bg_fill = palette.hover;
    widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent_dim);
    widgets.hovered.corner_radius = CornerRadius::same(6);

    widgets.active.bg_fill = palette.accent_dim;
    widgets.active.weak_bg_fill = palette.accent_dim;
    widgets.active.fg_stroke = Stroke::new(1.0, palette.text);
    widgets.active.corner_radius = CornerRadius::same(6);

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(10);

    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_takes_the_forms_people_write() {
        assert_eq!(to_hex(Color32::from_rgb(0x4C, 0x8D, 0xFF)), "#4c8dff");
        assert_eq!(
            parse_hex("#4c8dff"),
            Some(Color32::from_rgb(0x4C, 0x8D, 0xFF))
        );
        // With or without the hash, and in either case.
        assert_eq!(
            parse_hex("4C8DFF"),
            Some(Color32::from_rgb(0x4C, 0x8D, 0xFF))
        );
        assert_eq!(
            parse_hex("  #4c8dff  "),
            Some(Color32::from_rgb(0x4C, 0x8D, 0xFF))
        );
        // The CSS short form.
        assert_eq!(parse_hex("#abc"), Some(Color32::from_rgb(0xAA, 0xBB, 0xCC)));

        // Nonsense is refused rather than guessed at.
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);

        // Every colour of every preset survives the trip to a file and back.
        for (_, build) in PRESETS {
            let palette = build();
            for (_, fields) in Palette::sections() {
                for (_, field) in *fields {
                    let colour = palette.get(*field);
                    assert_eq!(parse_hex(&to_hex(colour)), Some(colour));
                }
            }
        }
    }

    #[test]
    fn the_form_reaches_every_colour_there_is() {
        // A colour added to the palette and forgotten in the form would be a
        // setting nobody could change; this is what stops that happening.
        let listed: Vec<Field> = Palette::sections()
            .iter()
            .flat_map(|(_, fields)| fields.iter().map(|(_, field)| *field))
            .collect();

        let mut seen: Vec<String> = listed.iter().map(|f| format!("{f:?}")).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), listed.len(), "a field is listed twice");

        // Set every listed field to the same colour; if the list had missed
        // one, that colour would still hold its Midnight value in the file.
        let mut palette = Palette::midnight();
        for field in &listed {
            palette.set(*field, Color32::from_rgb(1, 2, 3));
            assert_eq!(palette.get(*field), Color32::from_rgb(1, 2, 3));
        }
        let written = toml::to_string(&palette).unwrap();
        assert_eq!(
            written.matches("#010203").count(),
            listed.len(),
            "the form does not reach every colour in the palette"
        );
        assert!(
            !written.contains(&to_hex(Palette::midnight().bg)),
            "something was left at its default"
        );
    }

    #[test]
    fn the_presets_are_distinct_and_recognisable() {
        assert_eq!(PRESETS.len(), 3);
        let (midnight, commander, paper) =
            (Palette::midnight(), Palette::commander(), Palette::paper());
        assert_ne!(midnight, commander);
        assert_ne!(midnight, paper);
        assert_ne!(commander, paper);

        // A palette straight from a preset is reported as that preset, so the
        // picker can show which one is on without keeping a separate note.
        assert_eq!(preset_name(&commander), Some("Commander"));
        assert_eq!(preset("commander"), Some(commander), "case does not matter");
        assert_eq!(preset("nonesuch"), None);

        // Changed by one colour, it is nobody's preset any more.
        let mut edited = midnight;
        edited.accent = Color32::from_rgb(1, 2, 3);
        assert_eq!(preset_name(&edited), None);

        // Paper is light and the other two are dark, which is what decides
        // whether egui's light or dark widget defaults go underneath.
        assert!(!is_dark(&paper));
        assert!(is_dark(&midnight));
        assert!(is_dark(&commander));
    }

    #[test]
    fn a_saved_palette_comes_back_the_same() {
        let mut palette = Palette::commander();
        palette.accent = Color32::from_rgb(0x12, 0x34, 0x56);

        let text = toml::to_string(&palette).unwrap();
        assert!(text.contains("#123456"));
        let back: Palette = toml::from_str(&text).unwrap();
        assert_eq!(back, palette);

        // A file written by an older version, missing keys, still loads: the
        // gaps come from the default rather than the whole file being refused.
        let partial: Palette = toml::from_str("accent = \"#ff0000\"\n").unwrap();
        assert_eq!(partial.accent, Color32::from_rgb(0xFF, 0, 0));
        assert_eq!(partial.bg, Palette::midnight().bg);

        // A colour that is not a colour is refused, rather than silently
        // becoming black.
        assert!(toml::from_str::<Palette>("accent = \"lilac\"\n").is_err());
    }

    #[test]
    fn a_preset_is_remembered_by_name_and_a_custom_one_in_full() {
        use crate::config::Settings;

        let mut settings = Settings::default();
        into_settings(Palette::commander(), &mut settings);
        assert_eq!(settings.theme.as_deref(), Some("Commander"));
        assert!(
            settings.palette.is_none(),
            "a preset should not be frozen as a copy of its colours"
        );
        assert_eq!(from_settings(&settings), Palette::commander());

        // Change one colour and it is nobody's preset, so it is kept whole.
        let mut custom = Palette::commander();
        custom.accent = Color32::from_rgb(0x11, 0x22, 0x33);
        into_settings(custom, &mut settings);
        assert!(settings.theme.is_none());
        assert_eq!(settings.palette, Some(custom));
        assert_eq!(from_settings(&settings), custom);

        // Through a real settings file, and back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        settings.save_to(&path).unwrap();
        let read = Settings::load_from(&path).unwrap();
        assert_eq!(from_settings(&read), custom);

        // An empty file is the default theme, not an error.
        assert_eq!(from_settings(&Settings::default()), Palette::midnight());
        // And a preset name nobody recognises falls back rather than failing.
        let unknown = Settings {
            theme: Some("chartreuse".into()),
            ..Settings::default()
        };
        assert_eq!(from_settings(&unknown), Palette::midnight());
    }

    #[test]
    fn the_current_palette_is_what_the_accessors_answer() {
        set_palette(Palette::commander());
        assert_eq!(accent(), Palette::commander().accent);
        assert_eq!(bg(), Palette::commander().bg);

        set_palette(Palette::midnight());
        assert_eq!(accent(), Palette::midnight().accent);
    }
}

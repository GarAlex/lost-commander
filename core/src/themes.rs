//! Named colour schemes, in a shape any front-end can read.
//!
//! [`crate::gui::theme`] already holds palettes, but behind the `gui` feature
//! and in `Color32` - so the front-end on the other side of the C ABI cannot
//! see them, and a "Norton Commander" that was blue in one window and
//! something else in the other would be two programs wearing one name.
//!
//! This is the same decision as [`crate::filekind`] and [`crate::termview`]:
//! the *choice* is shared and the *drawing* is not. What crosses is a handful
//! of colours by role - what the window is, what text is, what the cursor bar
//! is - and each front-end maps those onto its own machinery. It is
//! deliberately smaller than the graphical front-end's palette: a role that
//! only egui has is a role this has no business naming.

use serde::Serialize;

/// One colour, as bytes. Hex when it crosses, because that is what every
/// front-end's colour parser already takes.
pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Theme {
    pub name: &'static str,
    /// Whether the scheme is built on a dark ground.
    ///
    /// Not a colour but a fact about one, and the front-end needs it before
    /// it draws anything: on Windows it decides which set of system control
    /// colours to start from, and getting it wrong gives black text on a
    /// black field in every control this program did not paint itself.
    pub dark: bool,
    /// A sentence for the picker, so the names are not a guessing game.
    pub about: &'static str,

    pub bg: String,
    /// The panes' own ground, usually a shade off `bg`.
    pub surface: String,
    pub border: String,
    pub text: String,
    pub text_dim: String,
    /// Headings, the sort arrow, the active pane's frame.
    pub accent: String,
    /// The cursor bar.
    pub cursor: String,
    /// What is legible *on* the cursor bar.
    pub cursor_text: String,
    /// The wash behind a marked row.
    pub marked: String,
    /// What a marked row's text becomes.
    pub marked_text: String,
    pub danger: String,
}

fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour.0, colour.1, colour.2)
}

fn theme(name: &'static str, dark: bool, about: &'static str, colours: [Rgb; 11]) -> Theme {
    Theme {
        name,
        dark,
        about,
        bg: hex(colours[0]),
        surface: hex(colours[1]),
        border: hex(colours[2]),
        text: hex(colours[3]),
        text_dim: hex(colours[4]),
        accent: hex(colours[5]),
        cursor: hex(colours[6]),
        cursor_text: hex(colours[7]),
        marked: hex(colours[8]),
        marked_text: hex(colours[9]),
        danger: hex(colours[10]),
    }
}

/// Every scheme, in the order a picker should offer them.
///
/// Left unformatted on purpose: the eleven colours of a scheme are laid out
/// three to a line so the roles line up in columns down the page and one
/// scheme can be read against another. rustfmt would give each its own line,
/// which is fifty-five lines of single colours where the shape is the point.
#[rustfmt::skip]
pub fn all() -> Vec<Theme> {
    vec![
        theme(
            "System",
            true,
            "whatever the desktop is set to",
            [
                (0x20, 0x20, 0x20), (0x2B, 0x2B, 0x2B), (0x3A, 0x3A, 0x3A),
                (0xF2, 0xF2, 0xF2), (0xA0, 0xA0, 0xA0), (0x60, 0xCD, 0xFF),
                (0x4F, 0xC3, 0xF7), (0x00, 0x00, 0x00), (0xFF, 0xC1, 0x07),
                (0xFF, 0xE0, 0x8A), (0xEF, 0x53, 0x50),
            ],
        ),
        theme(
            "Midnight",
            true,
            "a quiet dark surface with one accent",
            [
                (0x14, 0x17, 0x1C), (0x1B, 0x1F, 0x26), (0x2C, 0x33, 0x3D),
                (0xE6, 0xEA, 0xF0), (0x9A, 0xA4, 0xB2), (0x6E, 0x9E, 0xF5),
                (0x2E, 0x5C, 0xB8), (0xFF, 0xFF, 0xFF), (0x7A, 0x5E, 0x00),
                (0xFF, 0xE0, 0x8A), (0xEF, 0x53, 0x50),
            ],
        ),
        theme(
            "Norton Commander",
            true,
            "blue ground, cyan text, yellow folders - the 1986 original",
            [
                // The DOS palette this was drawn in: background blue 1,
                // bright cyan 11 for text, bright yellow 14 for what matters,
                // and a cyan bar for the cursor. Taken from the graphical
                // front-end's Commander preset so the two agree.
                (0x00, 0x00, 0x9C), (0x00, 0x00, 0xA8), (0x4C, 0xD3, 0xD3),
                (0x5A, 0xE6, 0xE6), (0x3E, 0xB8, 0xC4), (0xF5, 0xF5, 0x5A),
                (0x00, 0x9C, 0x9C), (0x00, 0x00, 0x00), (0x7A, 0x5E, 0x00),
                (0xFF, 0xF9, 0x7A), (0xFF, 0x6E, 0x6E),
            ],
        ),
        theme(
            "XTree Gold",
            true,
            "black ground, grey files, cyan bar, gold for what is tagged",
            [
                // Reconstructed from what the DOS original looked like rather
                // than from a spec: a black field, light grey listings, a
                // cyan cursor bar, and gold - the name is not an accident -
                // for headings and for files that are tagged. Tagging across
                // directories was the thing XTree had that nothing else did,
                // so the tag colour is the one that has to shout.
                (0x00, 0x00, 0x00), (0x0A, 0x0A, 0x0A), (0x00, 0xA8, 0xA8),
                (0xC0, 0xC0, 0xC0), (0x80, 0x80, 0x80), (0xFF, 0xD7, 0x00),
                (0x00, 0xA8, 0xA8), (0x00, 0x00, 0x00), (0x6B, 0x53, 0x00),
                (0xFF, 0xD7, 0x00), (0xFF, 0x55, 0x55),
            ],
        ),
        theme(
            "Paper",
            false,
            "ink on white, for a bright room",
            [
                (0xF7, 0xF7, 0xF5), (0xFF, 0xFF, 0xFF), (0xD4, 0xD4, 0xD0),
                (0x1C, 0x1C, 0x1C), (0x5F, 0x5F, 0x5F), (0x0B, 0x5F, 0xB0),
                (0x0B, 0x5F, 0xB0), (0xFF, 0xFF, 0xFF), (0xB4, 0x53, 0x09),
                (0x4A, 0x2A, 0x00), (0xC6, 0x28, 0x28),
            ],
        ),
    ]
}

/// One scheme by name, matched however it was capitalised.
pub fn named(name: &str) -> Option<Theme> {
    all()
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_is_named_once_and_findable() {
        let themes = all();
        let mut names: Vec<&str> = themes.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two schemes share a name");

        for theme in &themes {
            assert_eq!(named(theme.name).as_ref(), Some(theme));
        }
        // Case is not part of the name: a settings file written by hand
        // should not have to match capitalisation.
        assert!(named("norton commander").is_some());
        assert!(named("XTREE GOLD").is_some());
        assert!(named("no such scheme").is_none());
    }

    #[test]
    fn the_commander_is_the_blue_and_yellow_everyone_remembers() {
        let nc = named("Norton Commander").expect("the preset");
        assert!(nc.dark);
        // Blue ground, and folders in bright yellow.
        assert_eq!(nc.bg, "#00009c");
        assert_eq!(nc.accent, "#f5f55a");
        // The same values the graphical front-end's preset uses, so the two
        // windows are one program.
        assert_eq!(nc.cursor, "#009c9c");
    }

    #[test]
    fn xtree_gold_is_black_and_gold() {
        let xt = named("XTree Gold").expect("the preset");
        assert!(xt.dark);
        assert_eq!(xt.bg, "#000000");
        // Gold for headings and for what is tagged - tagging across
        // directories was the thing XTree had, so it is the colour that
        // has to carry.
        assert_eq!(xt.accent, "#ffd700");
        assert_eq!(xt.marked_text, "#ffd700");
    }

    #[test]
    fn a_light_scheme_says_it_is_one() {
        // Not decoration: on Windows this picks which set of system control
        // colours to start from, and getting it wrong gives black text on a
        // black field in every control the program does not paint itself.
        assert!(!named("Paper").unwrap().dark);
        assert!(all().iter().filter(|t| !t.dark).count() >= 1);
    }

    // The test that the graphical front-end's Commander preset matches the
    // one above is not here. It cannot be: this crate does not depend on that
    // one, on purpose, and a test is not an excuse to reach the other way.
    // It lives in `egui/src/theme.rs`, which can see both - and still runs
    // under `cargo test --workspace`, which is the only place it ever ran.

    #[test]
    fn every_colour_is_a_hex_triple() {
        for theme in all() {
            for (role, colour) in [
                ("bg", &theme.bg),
                ("surface", &theme.surface),
                ("border", &theme.border),
                ("text", &theme.text),
                ("text_dim", &theme.text_dim),
                ("accent", &theme.accent),
                ("cursor", &theme.cursor),
                ("cursor_text", &theme.cursor_text),
                ("marked", &theme.marked),
                ("marked_text", &theme.marked_text),
                ("danger", &theme.danger),
            ] {
                assert_eq!(colour.len(), 7, "{} {role}: {colour}", theme.name);
                assert!(colour.starts_with('#'), "{} {role}: {colour}", theme.name);
                assert!(
                    colour[1..].chars().all(|c| c.is_ascii_hexdigit()),
                    "{} {role}: {colour}",
                    theme.name
                );
            }
        }
    }
}

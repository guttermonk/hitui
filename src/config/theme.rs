//! Colour themes.
//!
//! `preset = "terminal"` is the default and uses [`Color::Reset`] plus the
//! ANSI names, so hitui inherits whatever palette the terminal already has —
//! including a transparent background. The named presets are for when you want
//! hitui to look the same everywhere regardless of the terminal.
//!
//! Any individual colour can be overridden on top of a preset:
//!
//! ```toml
//! [theme]
//! preset = "gruvbox-dark"
//!
//! [theme.colors]
//! border_focused = "#fabd2f"
//! ```

use std::collections::BTreeMap;
use std::fmt;

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The `[theme]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub preset: Preset,
    /// Per-slot overrides applied over the preset.
    pub colors: BTreeMap<String, ColorSpec>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            preset: Preset::Terminal,
            colors: BTreeMap::new(),
        }
    }
}

impl Theme {
    /// The resolved palette: the preset, with `[theme.colors]` applied.
    pub fn palette(&self) -> Result<Palette, UnknownColorSlot> {
        let mut palette = self.preset.palette();
        for (slot, spec) in &self.colors {
            palette.set(slot, spec.0)?;
        }
        Ok(palette)
    }
}

/// A built-in palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Follow the terminal's own colours.
    #[default]
    Terminal,
    CatppuccinMocha,
    CatppuccinLatte,
    GruvboxDark,
    Nord,
    Dracula,
    OneDark,
    SolarizedDark,
}

/// Every colour the interface uses.
///
/// A flat struct rather than a map, so a slot nobody set is a compile error
/// rather than a black-on-black surprise at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub fg: Color,
    pub bg: Color,
    /// Secondary text: field labels, the provenance tag, help hints.
    pub dim: Color,

    pub border: Color,
    pub border_focused: Color,

    /// Section headings in the detail pane and group headings in the sidebar.
    pub heading: Color,
    /// General emphasis: key names in the footer, the title.
    pub accent: Color,

    /// The selected sidebar row.
    pub cursor_fg: Color,
    pub cursor_bg: Color,

    pub status_fg: Color,
    pub status_bg: Color,

    /// The unfilled part of a gauge bar.
    pub gauge_track: Color,
    /// Gauge severity, and the error marker beside a page that failed.
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,

    /// Which of the quiet slots are drawn with the terminal's dim attribute
    /// rather than with a colour of their own.
    pub faint: Faint,
}

/// Slots drawn faint instead of coloured.
///
/// The three quiet roles — field labels, unfocused borders, gauge tracks —
/// have to read as subordinate to body text. Naming a colour for that only
/// works if you know the background: ANSI 8 is a mid-grey on a dark theme and
/// all but invisible on a light one, measured at 1.05:1 against rose-pine-dawn
/// and 1.06:1 against everforest-light, where 3:1 is the floor for legible
/// text. ANSI 7 is no escape either, being exactly what `fg = "reset"`
/// resolves to, so `dim` would come out the same colour as the text it is
/// meant to sit behind.
///
/// `Color::Reset` plus `Modifier::DIM` asks the terminal to subordinate its
/// own foreground, which is a question with a right answer on a light theme
/// and on a dark one. It also keeps the value palette-following: unlike a hex
/// triplet, it survives a live theme switch, so the slots that most need to
/// track the terminal are not the ones forced to freeze.
///
/// Rejected: a `terminal-light` preset, which pushes onto the user a choice
/// they would have to make again every time they switch, and get wrong in one
/// direction silently. Rejected: querying the background with OSC 11 and
/// picking a ramp from the reply — it needs a timeout on a response that may
/// never come, and misreports under a multiplexer. Rejected: documenting the
/// sharp edge and leaving it, which is cheapest and leaves the *default*
/// preset unreadable out of the box.
///
/// Failure mode: a terminal that ignores SGR 2 draws quiet text at normal
/// weight. Unsubordinated but legible, which beats invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Faint {
    pub dim: bool,
    pub border: bool,
    pub gauge_track: bool,
}

impl Faint {
    /// Every slot carries its own colour.
    pub const NONE: Self = Self {
        dim: false,
        border: false,
        gauge_track: false,
    };
    /// The quiet slots defer to the terminal's foreground.
    pub const QUIET: Self = Self {
        dim: true,
        border: true,
        gauge_track: true,
    };
}

impl Palette {
    /// Slot names accepted in `[theme.colors]`, for error messages and docs.
    ///
    /// One entry per field. A test asserts every name here is settable, so a
    /// field added without a matching arm in [`Palette::set`] is caught.
    pub const SLOTS: [&'static str; 15] = [
        "fg",
        "bg",
        "dim",
        "border",
        "border_focused",
        "heading",
        "accent",
        "cursor_fg",
        "cursor_bg",
        "status_fg",
        "status_bg",
        "gauge_track",
        "ok",
        "warn",
        "danger",
    ];

    fn set(&mut self, slot: &str, color: Color) -> Result<(), UnknownColorSlot> {
        // A colour you chose explicitly is drawn as you chose it, not faded.
        // Per slot, so overriding `dim` does not also un-faint `border`.
        match slot {
            "dim" => self.faint.dim = false,
            "border" => self.faint.border = false,
            "gauge_track" => self.faint.gauge_track = false,
            _ => {}
        }

        let target = match slot {
            "fg" => &mut self.fg,
            "bg" => &mut self.bg,
            "dim" => &mut self.dim,
            "border" => &mut self.border,
            "border_focused" => &mut self.border_focused,
            "heading" => &mut self.heading,
            "accent" => &mut self.accent,
            "cursor_fg" => &mut self.cursor_fg,
            "cursor_bg" => &mut self.cursor_bg,
            "status_fg" => &mut self.status_fg,
            "status_bg" => &mut self.status_bg,
            "gauge_track" => &mut self.gauge_track,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "danger" => &mut self.danger,
            other => return Err(UnknownColorSlot(other.to_owned())),
        };
        *target = color;
        Ok(())
    }
}

/// A `[theme.colors]` key that is not a real colour slot.
#[derive(Debug, Clone)]
pub struct UnknownColorSlot(pub String);

impl fmt::Display for UnknownColorSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Built from SLOTS rather than written out, so the message cannot
        // drift away from the slots that actually exist.
        write!(
            f,
            "unknown theme colour {:?} (expected one of: {})",
            self.0,
            Palette::SLOTS.join(", ")
        )
    }
}

impl std::error::Error for UnknownColorSlot {}

impl Preset {
    pub fn palette(self) -> Palette {
        match self {
            // Reset means "whatever the terminal already uses", which is what
            // keeps a transparent background transparent.
            // Reset plus the dim attribute for the quiet slots, rather than a
            // grey: see [`Faint`] for why a colour cannot be chosen here
            // without knowing whether the terminal is light or dark.
            Preset::Terminal => Palette {
                fg: Color::Reset,
                bg: Color::Reset,
                dim: Color::Reset,
                border: Color::Reset,
                border_focused: Color::Cyan,
                heading: Color::Cyan,
                accent: Color::Cyan,
                cursor_fg: Color::Black,
                cursor_bg: Color::Cyan,
                status_fg: Color::Reset,
                status_bg: Color::Reset,
                gauge_track: Color::Reset,
                ok: Color::Green,
                warn: Color::Yellow,
                danger: Color::Red,
                faint: Faint::QUIET,
            },
            Preset::CatppuccinMocha => Palette {
                fg: rgb(0xcdd6f4),
                bg: rgb(0x1e1e2e),
                dim: rgb(0x6c7086),
                border: rgb(0x45475a),
                border_focused: rgb(0x89b4fa),
                heading: rgb(0x89b4fa),
                accent: rgb(0xcba6f7),
                cursor_fg: rgb(0x1e1e2e),
                cursor_bg: rgb(0x89b4fa),
                status_fg: rgb(0xcdd6f4),
                status_bg: rgb(0x313244),
                gauge_track: rgb(0x45475a),
                ok: rgb(0xa6e3a1),
                warn: rgb(0xf9e2af),
                danger: rgb(0xf38ba8),
                faint: Faint::NONE,
            },
            Preset::CatppuccinLatte => Palette {
                fg: rgb(0x4c4f69),
                bg: rgb(0xeff1f5),
                dim: rgb(0x8c8fa1),
                border: rgb(0xbcc0cc),
                border_focused: rgb(0x1e66f5),
                heading: rgb(0x1e66f5),
                accent: rgb(0x8839ef),
                cursor_fg: rgb(0xeff1f5),
                cursor_bg: rgb(0x1e66f5),
                status_fg: rgb(0x4c4f69),
                status_bg: rgb(0xccd0da),
                gauge_track: rgb(0xbcc0cc),
                ok: rgb(0x40a02b),
                warn: rgb(0xdf8e1d),
                danger: rgb(0xd20f39),
                faint: Faint::NONE,
            },
            Preset::GruvboxDark => Palette {
                fg: rgb(0xebdbb2),
                bg: rgb(0x282828),
                dim: rgb(0x928374),
                border: rgb(0x504945),
                border_focused: rgb(0xfabd2f),
                heading: rgb(0x8ec07c),
                accent: rgb(0xfabd2f),
                cursor_fg: rgb(0x282828),
                cursor_bg: rgb(0xfabd2f),
                status_fg: rgb(0xebdbb2),
                status_bg: rgb(0x3c3836),
                gauge_track: rgb(0x504945),
                ok: rgb(0xb8bb26),
                warn: rgb(0xfabd2f),
                danger: rgb(0xfb4934),
                faint: Faint::NONE,
            },
            Preset::Nord => Palette {
                fg: rgb(0xd8dee9),
                bg: rgb(0x2e3440),
                dim: rgb(0x616e88),
                border: rgb(0x4c566a),
                border_focused: rgb(0x88c0d0),
                heading: rgb(0x88c0d0),
                accent: rgb(0x81a1c1),
                cursor_fg: rgb(0x2e3440),
                cursor_bg: rgb(0x88c0d0),
                status_fg: rgb(0xd8dee9),
                status_bg: rgb(0x3b4252),
                gauge_track: rgb(0x4c566a),
                ok: rgb(0xa3be8c),
                warn: rgb(0xebcb8b),
                danger: rgb(0xbf616a),
                faint: Faint::NONE,
            },
            Preset::Dracula => Palette {
                fg: rgb(0xf8f8f2),
                bg: rgb(0x282a36),
                dim: rgb(0x6272a4),
                border: rgb(0x44475a),
                border_focused: rgb(0x8be9fd),
                heading: rgb(0x8be9fd),
                accent: rgb(0xbd93f9),
                cursor_fg: rgb(0x282a36),
                cursor_bg: rgb(0x8be9fd),
                status_fg: rgb(0xf8f8f2),
                status_bg: rgb(0x44475a),
                gauge_track: rgb(0x44475a),
                ok: rgb(0x50fa7b),
                warn: rgb(0xf1fa8c),
                danger: rgb(0xff5555),
                faint: Faint::NONE,
            },
            Preset::OneDark => Palette {
                fg: rgb(0xabb2bf),
                bg: rgb(0x282c34),
                dim: rgb(0x5c6370),
                border: rgb(0x3e4451),
                border_focused: rgb(0x61afef),
                heading: rgb(0x61afef),
                accent: rgb(0xc678dd),
                cursor_fg: rgb(0x282c34),
                cursor_bg: rgb(0x61afef),
                status_fg: rgb(0xabb2bf),
                status_bg: rgb(0x2c323c),
                gauge_track: rgb(0x3e4451),
                ok: rgb(0x98c379),
                warn: rgb(0xe5c07b),
                danger: rgb(0xe06c75),
                faint: Faint::NONE,
            },
            Preset::SolarizedDark => Palette {
                fg: rgb(0x839496),
                bg: rgb(0x002b36),
                dim: rgb(0x586e75),
                border: rgb(0x073642),
                border_focused: rgb(0x268bd2),
                heading: rgb(0x268bd2),
                accent: rgb(0x6c71c4),
                cursor_fg: rgb(0x002b36),
                cursor_bg: rgb(0x268bd2),
                status_fg: rgb(0x839496),
                status_bg: rgb(0x073642),
                gauge_track: rgb(0x073642),
                ok: rgb(0x859900),
                warn: rgb(0xb58900),
                danger: rgb(0xdc322f),
                faint: Faint::NONE,
            },
        }
    }
}

fn rgb(packed: u32) -> Color {
    Color::Rgb(
        ((packed >> 16) & 0xff) as u8,
        ((packed >> 8) & 0xff) as u8,
        (packed & 0xff) as u8,
    )
}

/// A colour as written in the config: a hex triplet or an ANSI name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorSpec(pub Color);

impl<'de> Deserialize<'de> for ColorSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_color(&text)
            .map(ColorSpec)
            // The reason is relayed rather than flattened: for an ambiguous
            // word it names the two spellings that resolve it, and a generic
            // "invalid colour" would send the reader back to the docs for
            // something the error already knows.
            .map_err(|why| serde::de::Error::custom(format!("invalid colour {text:?}: {why}")))
    }
}

impl Serialize for ColorSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = match self.0 {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Reset => "reset".into(),
            other => format!("{other:?}").to_lowercase(),
        };
        serializer.serialize_str(&text)
    }
}

/// A colour string that cannot be used.
#[derive(Debug)]
pub enum InvalidColor {
    /// Not a colour in any vocabulary.
    Unknown,
    /// A word that parses here *and* in ratatui's own colour vocabulary, with
    /// a different colour each time. Carries the advice for resolving it,
    /// since the fix is to write one of two other words.
    Ambiguous(&'static str),
}

impl fmt::Display for InvalidColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(
                f,
                "expected a hex triplet like \"#89b4fa\" or an ANSI name like \"bright-blue\""
            ),
            Self::Ambiguous(advice) => write!(f, "{advice}"),
        }
    }
}

/// Why `white` is refused.
///
/// It is the one word a reader is most likely to assume is safe, and it was
/// the one word that quietly meant something else: ANSI 7 here, ANSI 15 to
/// every other ratatui application. Both readings are reasonable, so neither
/// is chosen.
const WHITE_IS_AMBIGUOUS: &str = "\"white\" means ANSI 7 to some tools and ANSI 15 to ratatui, \
     so it is refused rather than guessed — write \"gray\" for ANSI 7, which is also what \
     \"reset\" resolves to, or \"bright-white\" for ANSI 15, which is what ratatui calls white";

/// Why `bright-gray` is refused.
const BRIGHT_GRAY_IS_AMBIGUOUS: &str = "\"bright-gray\" reads as ANSI 8 but ratatui folds it to \
     white at ANSI 15, so it is refused rather than guessed — write \"bright-white\" for ANSI 15 \
     or \"dark-gray\" for ANSI 8";

/// Parse `#rrggbb`, `0xrrggbb`, or an ANSI colour name.
///
/// Names are normalised the way ratatui normalises its own: lowercased, with
/// spaces, hyphens and underscores removed, then `bright` read as `light` and
/// `grey` as `gray`. So `bright-red`, `brightred`, `light_red` and `lightred`
/// are one spelling, and a value copied out of another ratatui application's
/// config means here what it meant there.
///
/// That matters because the two vocabularies used to agree on ANSI 0-7 and
/// disagree on all of 8-15, and only one of those disagreements was loud:
/// `lightred` matched nothing, fell through to the hex branch and was
/// rejected, naming itself. Two words parsed in both and meant different
/// colours, and those two are now refused outright — see
/// [`WHITE_IS_AMBIGUOUS`]. Rejecting a word that used to parse is a breaking
/// change, which at v0.1.0 costs one config edit; the alternative is a wrong
/// colour that nobody notices.
pub fn parse_color(text: &str) -> Result<Color, InvalidColor> {
    let trimmed = text.trim();

    // Hex, in either of the two spellings people actually write.
    let hex = trimmed
        .strip_prefix('#')
        .or_else(|| trimmed.strip_prefix("0x"))
        .or_else(|| trimmed.strip_prefix("0X"));
    if let Some(hex) = hex {
        if hex.len() != 6 {
            return Err(InvalidColor::Unknown);
        }
        let packed = u32::from_str_radix(hex, 16).map_err(|_| InvalidColor::Unknown)?;
        return Ok(rgb(packed));
    }

    let name = trimmed
        .to_ascii_lowercase()
        .replace([' ', '-', '_'], "")
        .replace("bright", "light")
        .replace("grey", "gray");

    Ok(match name.as_str() {
        "reset" | "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        // ANSI 7. "silver" is ratatui's alias for it.
        "gray" | "silver" => Color::Gray,
        // ANSI 8. ratatui reads "lightblack" as this too, and the name is a
        // fair description of what the slot is.
        "darkgray" | "lightblack" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        // ANSI 15, which ratatui spells "white".
        "lightwhite" => Color::White,

        "white" => return Err(InvalidColor::Ambiguous(WHITE_IS_AMBIGUOUS)),
        "lightgray" => return Err(InvalidColor::Ambiguous(BRIGHT_GRAY_IS_AMBIGUOUS)),
        _ => return Err(InvalidColor::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_is_refused_as_ambiguous() {
        // The one word that used to parse in both vocabularies and mean a
        // different colour in each: ANSI 7 here, ANSI 15 to ratatui. It is
        // also the word a reader is most likely to assume is safe, which is
        // what made it the only silent disagreement of the sixteen.
        let error = parse_color("white").expect_err("white is ambiguous");
        let message = error.to_string();
        // The error has to carry both fixes, or it is just a refusal.
        assert!(message.contains("gray"), "{message}");
        assert!(message.contains("bright-white"), "{message}");
        assert!(
            message.contains("ANSI 7") && message.contains("ANSI 15"),
            "{message}"
        );

        // ratatui folds lightgray to white at ANSI 15 despite the name, so
        // that spelling is ambiguous in the same way.
        let error = parse_color("bright-gray").expect_err("bright-gray is ambiguous");
        let message = error.to_string();
        assert!(
            message.contains("bright-white") && message.contains("dark-gray"),
            "{message}"
        );
        // Same word, after normalisation.
        assert!(parse_color("lightgray").is_err());
        assert!(parse_color("light_grey").is_err());
    }

    #[test]
    fn ratatui_spellings_mean_what_they_mean_in_ratatui() {
        // A value copied out of another ratatui application's config should do
        // the obvious thing rather than fall through to the hex branch and be
        // rejected. Normalisation is ratatui's: lowercase, strip spaces,
        // hyphens and underscores, bright -> light, grey -> gray.
        for (ours, theirs) in [
            ("bright-red", "lightred"),
            ("bright-green", "LightGreen"),
            ("bright-yellow", "light_yellow"),
            ("bright-blue", "LIGHTBLUE"),
            ("bright-magenta", "light magenta"),
            ("bright-cyan", "lightcyan"),
            ("bright-white", "lightwhite"),
            ("dark-gray", "darkgray"),
            ("dark-grey", "DarkGrey"),
        ] {
            assert_eq!(
                parse_color(ours).unwrap(),
                parse_color(theirs).unwrap(),
                "{ours} and {theirs} should be one colour"
            );
        }

        // ratatui's own aliases for the two greys at either end.
        assert_eq!(parse_color("silver").unwrap(), Color::Gray);
        assert_eq!(parse_color("bright-black").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("lightblack").unwrap(), Color::DarkGray);
        // And ANSI 15 is reachable, under the name that is not contested.
        assert_eq!(parse_color("bright-white").unwrap(), Color::White);
    }

    #[test]
    fn the_terminal_preset_subordinates_its_quiet_slots_without_a_colour() {
        // Naming a colour here cannot work: ANSI 8 measures 1.05:1 against
        // rose-pine-dawn's background, and ANSI 7 is exactly what `reset`
        // resolves to, so `dim` would be the colour of the text it sits
        // behind. See Faint.
        let palette = Preset::Terminal.palette();
        for (slot, colour) in [
            ("dim", palette.dim),
            ("border", palette.border),
            ("gauge_track", palette.gauge_track),
        ] {
            assert_eq!(
                colour,
                Color::Reset,
                "{slot} should defer to the terminal's foreground"
            );
        }
        assert_eq!(palette.faint, Faint::QUIET);

        // The focused border is the one that has to be found at a glance, so
        // it keeps a colour of its own.
        assert_ne!(palette.border_focused, Color::Reset);
    }

    #[test]
    fn a_preset_that_names_its_colours_is_not_faint() {
        // Faintness is a consequence of not knowing the background. A preset
        // that sets every colour knows it.
        for preset in [
            Preset::CatppuccinMocha,
            Preset::CatppuccinLatte,
            Preset::GruvboxDark,
            Preset::Nord,
            Preset::Dracula,
            Preset::OneDark,
            Preset::SolarizedDark,
        ] {
            assert_eq!(
                preset.palette().faint,
                Faint::NONE,
                "{preset:?} names its own colours"
            );
        }
    }

    #[test]
    fn an_explicit_colour_clears_the_faint_attribute_for_that_slot_only() {
        let theme = Theme {
            preset: Preset::Terminal,
            colors: [("dim".to_string(), ColorSpec(Color::Blue))]
                .into_iter()
                .collect(),
        };
        let palette = theme.palette().expect("valid theme");

        // A colour you chose is drawn as you chose it, not faded.
        assert_eq!(palette.dim, Color::Blue);
        assert!(!palette.faint.dim);
        // And the slots you said nothing about are untouched.
        assert!(palette.faint.border);
        assert!(palette.faint.gauge_track);
    }

    #[test]
    fn hex_is_accepted_in_both_spellings() {
        assert_eq!(
            parse_color("#89b4fa").unwrap(),
            Color::Rgb(0x89, 0xb4, 0xfa)
        );
        assert_eq!(
            parse_color("0x89b4fa").unwrap(),
            Color::Rgb(0x89, 0xb4, 0xfa)
        );
        // Case and surrounding space should not matter.
        assert_eq!(
            parse_color(" #89B4FA ").unwrap(),
            Color::Rgb(0x89, 0xb4, 0xfa)
        );
    }

    #[test]
    fn ansi_names_are_accepted() {
        assert_eq!(parse_color("cyan").unwrap(), Color::Cyan);
        assert_eq!(parse_color("dark-grey").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("bright-blue").unwrap(), Color::LightBlue);
        assert_eq!(parse_color("reset").unwrap(), Color::Reset);
    }

    #[test]
    fn malformed_colours_are_rejected_not_guessed() {
        // Silently falling back to a default would give a theme that is wrong
        // in a way nobody can see in the config.
        assert!(parse_color("#89b4f").is_err(), "five digits");
        assert!(parse_color("#89b4faa").is_err(), "seven digits");
        assert!(parse_color("#zzzzzz").is_err());
        assert!(parse_color("puce").is_err());
        assert!(parse_color("").is_err());
    }

    #[test]
    fn the_terminal_preset_leaves_the_background_alone() {
        // The reason this is the default: a themed or transparent terminal
        // keeps its appearance instead of being painted over.
        let palette = Preset::Terminal.palette();
        assert_eq!(palette.bg, Color::Reset);
        assert_eq!(palette.fg, Color::Reset);
    }

    #[test]
    fn named_presets_set_every_slot_explicitly() {
        // A preset that left a slot as Reset would inherit a terminal colour
        // the rest of the palette was not designed against.
        for preset in [
            Preset::CatppuccinMocha,
            Preset::CatppuccinLatte,
            Preset::GruvboxDark,
            Preset::Nord,
            Preset::Dracula,
            Preset::OneDark,
            Preset::SolarizedDark,
        ] {
            let palette = preset.palette();
            for (name, colour) in [
                ("fg", palette.fg),
                ("bg", palette.bg),
                ("dim", palette.dim),
                ("border", palette.border),
                ("border_focused", palette.border_focused),
                ("heading", palette.heading),
                ("accent", palette.accent),
                ("cursor_fg", palette.cursor_fg),
                ("cursor_bg", palette.cursor_bg),
                ("status_fg", palette.status_fg),
                ("status_bg", palette.status_bg),
                ("gauge_track", palette.gauge_track),
                ("ok", palette.ok),
                ("warn", palette.warn),
                ("danger", palette.danger),
            ] {
                assert!(
                    matches!(colour, Color::Rgb(..)),
                    "{preset:?} leaves {name} unset"
                );
            }
        }
    }

    #[test]
    fn overrides_apply_on_top_of_a_preset() {
        let theme: Theme = toml::from_str(
            r##"
            preset = "nord"
            [colors]
            border_focused = "#fabd2f"
            danger = "bright-red"
            "##,
        )
        .expect("valid theme");

        let palette = theme.palette().expect("known slots");
        assert_eq!(palette.border_focused, Color::Rgb(0xfa, 0xbd, 0x2f));
        assert_eq!(palette.danger, Color::LightRed);
        // Untouched slots keep the preset's value.
        assert_eq!(palette.ok, Preset::Nord.palette().ok);
    }

    #[test]
    fn an_unknown_slot_names_itself_and_the_alternatives() {
        let theme: Theme = toml::from_str(
            r##"
            [colors]
            bordr_focused = "#fabd2f"
            "##,
        )
        .expect("parses; the slot name is checked when resolving");

        let error = theme.palette().expect_err("unknown slot");
        let message = error.to_string();
        assert!(message.contains("bordr_focused"), "{message}");
        assert!(message.contains("border_focused"), "{message}");
    }

    #[test]
    fn every_palette_field_has_a_settable_slot() {
        // Guards the pairing between the struct and `set`: a field added
        // without a match arm would be invisible to `[theme.colors]`.
        let mut palette = Preset::Nord.palette();
        let marker = Color::Rgb(1, 2, 3);

        for slot in Palette::SLOTS {
            palette
                .set(slot, marker)
                .unwrap_or_else(|e| panic!("{slot} is listed but not settable: {e}"));
        }

        // Setting every listed slot must leave nothing at its preset value —
        // which only holds if SLOTS covers the whole struct.
        let untouched = Preset::Nord.palette();
        assert_ne!(palette, untouched);
        for colour in [
            palette.fg,
            palette.bg,
            palette.dim,
            palette.border,
            palette.border_focused,
            palette.heading,
            palette.accent,
            palette.cursor_fg,
            palette.cursor_bg,
            palette.status_fg,
            palette.status_bg,
            palette.gauge_track,
            palette.ok,
            palette.warn,
            palette.danger,
        ] {
            assert_eq!(colour, marker, "a field is missing from SLOTS");
        }
    }
}

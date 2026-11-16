//! Configuration.
//!
//! Loaded from `$XDG_CONFIG_HOME/hitui/config.toml`, or
//! `~/.config/hitui/config.toml` when `XDG_CONFIG_HOME` is unset. Every value
//! has a default, so no config file at all behaves exactly like the shipped
//! `config.example.toml`.
//!
//! A malformed config is an error rather than a silent fallback. Starting with
//! default colours because a hex triplet had five digits would leave you
//! wondering why the theme did nothing.

pub mod theme;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::keys::{KeymapSection, Keymaps, Mode};
use theme::{Palette, Theme};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: Theme,
    /// Per-mode keybinding layers.
    pub keys: BTreeMap<Mode, KeymapSection>,
    pub layout: Layout,
    pub benchmarks: Benchmarks,

    /// What a colour overlay contributed, if one was read. Not a config key:
    /// the loader fills it in and `--theme` reports it.
    #[serde(skip)]
    pub overlay: Option<Overlay>,
}

/// What a wallust-style colour overlay contributed to this config.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub path: PathBuf,
    /// Slots it supplied.
    pub applied: Vec<String>,
    /// Keys it named that are not colour slots, dropped on the way through.
    pub ignored: Vec<String>,
}

/// What the Benchmarks page keeps, and what it is allowed to send.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Benchmarks {
    /// Save each run so later runs can be compared against it.
    ///
    /// On by default, because nothing is written unless a benchmark was
    /// explicitly asked for, the file is plain JSON under $XDG_STATE_HOME,
    /// and a comparison is the main thing anyone wants from a second run.
    pub history: bool,

    /// How many runs to keep, newest first out of the far end.
    pub history_limit: usize,

    /// Upload results so they can be compared against other machines.
    ///
    /// Off, and there is deliberately nothing behind it yet: see
    /// [`Benchmarks::share_endpoint`].
    pub share: bool,

    /// Where results would be sent.
    ///
    /// Empty by default and with no built-in value, so hitui cannot reach a
    /// server nobody named. Uploading is not implemented in this build; the
    /// Benchmarks page says so rather than leaving the setting to look as
    /// though it did something.
    pub share_endpoint: String,
}

impl Default for Benchmarks {
    fn default() -> Self {
        Self {
            history: true,
            history_limit: 20,
            share: false,
            share_endpoint: String::new(),
        }
    }
}

/// How the detail pane spends the width it has.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    /// Most columns a page may be split into.
    ///
    /// Only a page that gets *shorter* in more columns is split at all, so this
    /// is a ceiling rather than a target. Set it to 1 to keep every page in one
    /// column however much width is going spare.
    pub max_columns: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self { max_columns: 4 }
    }
}

impl Config {
    /// Load the config, or the defaults if there is no file.
    pub fn load() -> Result<Self> {
        let text = match Self::path() {
            Some(path) if path.is_file() => std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?,
            // No config file is not the same as no overlay: a palette derived
            // from the wallpaper still has to reach a machine whose owner
            // never wrote a config.
            _ => String::new(),
        };
        Self::parse(&text)
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Parse config text, with the colour overlay layered over it.
    fn parse(text: &str) -> Result<Self> {
        let Some((path, colors)) = colors_overlay() else {
            // Nothing to merge. Deserialising straight from the text keeps
            // toml's line and column numbers in any error message, which the
            // Value round trip below loses, so the common case keeps the
            // better errors.
            return Self::from_str(text);
        };

        // Merged into a toml::Value before anything is deserialised.
        // Concatenating the two files as text cannot work: TOML rejects a
        // second [theme.colors] header as a duplicate key.
        let mut value: toml::Value = toml::from_str(text)?;
        let overlay = merge_colors_overlay(&mut value, path, colors);
        let mut config: Config = value.try_into()?;
        config.overlay = Some(overlay);
        config.validate()?;
        Ok(config)
    }

    /// Parse config text on its own, with no overlay.
    ///
    /// This is what the tests use, including the one asserting the shipped
    /// example describes the defaults. Merging the overlay here would make the
    /// suite depend on whatever overlay happens to exist on the machine
    /// running it; `HITUI_COLORS_OVERLAY` is what makes the merge itself
    /// testable.
    pub fn from_str(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        // Resolve the palette now rather than at first draw, so an unknown
        // colour slot is reported before the terminal is taken over.
        self.palette()?;
        // Zero columns is a page with nowhere to draw, not a page in one
        // column — say so rather than silently reading it as 1.
        if self.layout.max_columns == 0 {
            bail!("layout.max_columns must be at least 1");
        }
        // Keeping zero runs is not a shorter history, it is no history —
        // which `history = false` already says, so this would be a setting
        // that silently contradicts the one next to it.
        if self.benchmarks.history && self.benchmarks.history_limit == 0 {
            bail!(
                "benchmarks.history_limit must be at least 1 when history is on \
                 (set history = false to keep nothing)"
            );
        }
        Ok(())
    }

    /// Where the config is read from, if a home directory can be determined.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("hitui").join("config.toml"))
    }

    /// Where a colour overlay would be read from, for `--theme` to report.
    pub fn overlay_path() -> Option<PathBuf> {
        colors_overlay_path()
    }

    pub fn palette(&self) -> Result<Palette> {
        Ok(self.theme.palette()?)
    }

    pub fn keymaps(&self) -> Keymaps {
        Keymaps::resolve(&self.keys)
    }
}

/// Where the optional second colour file lives.
///
/// It exists for tools that recolour a desktop from the current wallpaper —
/// wallust, pywal — which re-extract a palette on every wallpaper change and
/// have no business owning a file that also holds your keybindings and layout.
/// Such a tool renders the overlay; hitui is a fresh process every launch, so
/// the next run already has the new colours and nothing here needs a reload
/// hook or a file watcher.
///
/// Set `HITUI_COLORS_OVERLAY` to read it from somewhere else. The sibling
/// application bluetui has the same seam under `BLUETUI_COLORS_OVERLAY`, and
/// the two are deliberately the same shape so they can be reasoned about — and
/// generated — together.
fn colors_overlay_path() -> Option<PathBuf> {
    overlay_path_from(
        std::env::var_os("HITUI_COLORS_OVERLAY").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The precedence, separated from the environment that supplies it.
///
/// Taking the three values as arguments is what makes this testable: mutating
/// the process environment from a test would race every other test in the
/// suite, and one of them enumerates the environment on purpose.
fn overlay_path_from(
    override_var: Option<&OsStr>,
    cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(path) = override_var {
        // An empty variable is the shell's way of saying nothing, not a
        // request to read the root directory.
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let cache = cache_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("wallust").join("colors-hitui.toml"))
}

/// The overlay's `[colors]` table, if there is a readable one.
fn colors_overlay() -> Option<(PathBuf, toml::Table)> {
    let path = colors_overlay_path()?;
    let colors = read_overlay(&path)?;
    Some((path, colors))
}

/// The `[colors]` table of one overlay file.
///
/// A missing, unreadable or malformed overlay is the normal state — it means
/// "use the config" — and is silently nothing, which is what every palette
/// that is not wallpaper-derived wants. It also covers the overlay being
/// half-written at the moment hitui starts, which a tool rewriting it on every
/// wallpaper change will eventually do.
fn read_overlay(path: &Path) -> Option<toml::Table> {
    let text = std::fs::read_to_string(path).ok()?;

    // toml::from_str, not str::parse: FromStr for Value parses a single
    // *value*, so a document starting with [colors] is read as an array and
    // then errors on the rest of the file. bluetui was caught by exactly this,
    // and it fails invisibly here by design — which is right for a missing
    // overlay and very wrong for one being parsed the wrong way.
    let overlay: toml::Value = toml::from_str(&text).ok()?;
    Some(overlay.get("colors")?.as_table()?.clone())
}

/// Merge the overlay's `[colors]` entries over the config's `theme.colors`,
/// key by key.
///
/// Per key rather than per file, so an overlay naming only `border` leaves the
/// other fourteen as the config has them. The `theme` and `theme.colors`
/// tables are created if the config omitted them.
///
/// The overlay's table is top level — `[colors]`, not `[theme.colors]` —
/// because every file in that wallust template directory uses a top-level
/// `[colors]` table, and uniformity across the generated set is worth more
/// than matching hitui's own nesting.
///
/// A key that is not a colour slot is dropped and recorded, rather than being
/// fatal as the same key in `[theme.colors]` would be. The asymmetry is
/// deliberate: this file is machine-generated by a wallpaper tool, and a
/// wallpaper must not be able to stop you from reading your hardware. Dropping
/// it silently would be the usual worse option, so `--theme` names what it
/// dropped — a generated typo is permanent and will not fix itself, so it has
/// to be discoverable somewhere.
fn merge_colors_overlay(config: &mut toml::Value, path: PathBuf, overlay: toml::Table) -> Overlay {
    let mut report = Overlay {
        path,
        applied: Vec::new(),
        ignored: Vec::new(),
    };

    let Some(root) = config.as_table_mut() else {
        return report;
    };
    let theme = root
        .entry("theme".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(theme) = theme.as_table_mut() else {
        return report;
    };
    let colors = theme
        .entry("colors".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(colors) = colors.as_table_mut() else {
        return report;
    };

    for (key, value) in overlay {
        if !Palette::SLOTS.contains(&key.as_str()) {
            report.ignored.push(key);
            continue;
        }
        colors.insert(key.clone(), value);
        report.applied.push(key);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::combo::KeyCombo;
    use crate::keys::{Action, Resolution};
    use theme::Preset;

    /// A config document as a `toml::Value`, ready for the merge.
    fn value(text: &str) -> toml::Value {
        toml::from_str(text).expect("fixture is valid TOML")
    }

    fn table(text: &str) -> toml::Table {
        value(text)
            .get("colors")
            .and_then(|colors| colors.as_table())
            .cloned()
            .expect("fixture has a [colors] table")
    }

    /// A temporary file holding `text`, named after the calling test.
    fn overlay_file(name: &str, text: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hitui-overlay-{name}.toml"));
        std::fs::write(&path, text).expect("writing the fixture");
        path
    }

    #[test]
    fn an_overlay_merges_per_key_and_leaves_the_rest() {
        // The property the contract rests on: an overlay naming one slot must
        // not reset the other fourteen to the preset.
        let mut config = value(
            r##"
            [theme]
            preset = "nord"
            [theme.colors]
            accent = "#fabd2f"
            border = "blue"
            "##,
        );
        let report = merge_colors_overlay(
            &mut config,
            PathBuf::from("/nowhere"),
            table(
                r##"[colors]
                border = "#ff0000"
            "##,
            ),
        );

        let config: Config = config.try_into().expect("merged config deserialises");
        let palette = config.palette().expect("palette resolves");
        assert_eq!(palette.border, ratatui::style::Color::Rgb(0xff, 0, 0));
        // Untouched by the overlay, so still the config's own value.
        assert_eq!(palette.accent, ratatui::style::Color::Rgb(0xfa, 0xbd, 0x2f));
        // And untouched by either, so still the preset's.
        assert_eq!(palette.heading, Preset::Nord.palette().heading);

        assert_eq!(report.applied, vec!["border".to_string()]);
        assert!(report.ignored.is_empty());
    }

    #[test]
    fn an_overlay_reaches_a_config_with_no_theme_table() {
        // The tables have to be created: most configs that take an overlay
        // will not mention colours at all.
        let mut config = value("[keys.browser]\nextra = []\n");
        merge_colors_overlay(
            &mut config,
            PathBuf::from("/nowhere"),
            table("[colors]\nheading = \"red\"\n"),
        );
        let config: Config = config.try_into().expect("merged config deserialises");
        assert_eq!(
            config.palette().unwrap().heading,
            ratatui::style::Color::Red
        );
    }

    #[test]
    fn an_overlay_key_that_is_not_a_slot_is_dropped_and_named() {
        // Deliberately not fatal, unlike the same key in [theme.colors]: this
        // file is written by a wallpaper tool, which must not be able to stop
        // hitui from starting. Dropped silently would be the usual worse
        // option, so it is reported — `--theme` prints `ignored`.
        let mut config = value("");
        let report = merge_colors_overlay(
            &mut config,
            PathBuf::from("/nowhere"),
            table(
                r##"[colors]
                accent = "red"
                bogus_slot = "#00ff00"
                colour7 = "blue"
            "##,
            ),
        );

        let config: Config = config.try_into().expect("the unknown key was dropped");
        assert_eq!(config.palette().unwrap().accent, ratatui::style::Color::Red);
        assert_eq!(report.applied, vec!["accent".to_string()]);
        assert_eq!(
            report.ignored,
            vec!["bogus_slot".to_string(), "colour7".to_string()]
        );
    }

    #[test]
    fn a_missing_or_malformed_overlay_is_silently_nothing() {
        // This is the normal state for the 23 palettes that are not derived
        // from a wallpaper, so it cannot warn and it cannot fail.
        assert!(read_overlay(Path::new("/nonexistent/hitui/overlay.toml")).is_none());

        let truncated = overlay_file("truncated", "[colors\nborder = oops\n");
        assert!(read_overlay(&truncated).is_none());

        let no_table = overlay_file("no-table", "border = \"red\"\n");
        assert!(
            read_overlay(&no_table).is_none(),
            "a top-level key is not the [colors] table"
        );

        let _ = std::fs::remove_file(truncated);
        let _ = std::fs::remove_file(no_table);
    }

    #[test]
    fn an_overlay_is_read_as_a_document_not_as_a_value() {
        // The trap: FromStr for toml::Value parses a single value, so a
        // document starting with [colors] reads as an array and then errors on
        // the rest of the file. Because a failed parse is silent by design,
        // getting this wrong means the overlay never applies and nothing says
        // so. The assertion is that a perfectly ordinary overlay parses.
        let path = overlay_file(
            "document",
            "[colors]\nborder = \"#ff0000\"\ndim = \"#808080\"\n",
        );
        let colors = read_overlay(&path).expect("an ordinary overlay should parse");
        assert_eq!(colors.len(), 2);

        // And the shape it would be if read as a value is in fact rejected.
        assert!(
            "[colors]\nborder = \"#ff0000\"\n"
                .parse::<toml::Value>()
                .is_err()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_overlay_path_prefers_the_override_then_xdg_then_home() {
        use std::ffi::OsStr;

        let path = |a: Option<&str>, b: Option<&str>, c: Option<&str>| {
            overlay_path_from(a.map(OsStr::new), b.map(OsStr::new), c.map(OsStr::new))
        };

        assert_eq!(
            path(Some("/tmp/mine.toml"), Some("/cache"), Some("/home/u")),
            Some(PathBuf::from("/tmp/mine.toml"))
        );
        assert_eq!(
            path(None, Some("/cache"), Some("/home/u")),
            Some(PathBuf::from("/cache/wallust/colors-hitui.toml"))
        );
        assert_eq!(
            path(None, None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/wallust/colors-hitui.toml"))
        );
        // A relative XDG_CACHE_HOME is not a cache directory.
        assert_eq!(
            path(None, Some("relative"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/wallust/colors-hitui.toml"))
        );
        // An empty override is the shell saying nothing, not the root.
        assert_eq!(
            path(Some(""), Some("/cache"), None),
            Some(PathBuf::from("/cache/wallust/colors-hitui.toml"))
        );
        assert_eq!(path(None, None, None), None);
    }

    #[test]
    fn from_file_names_a_path_it_cannot_read() {
        // `--config` must not fall back to the defaults: that would answer a
        // question about a different config than the one asked about.
        let error = Config::from_file(Path::new("/nonexistent/hitui/config.toml"))
            .expect_err("a missing config file is an error");
        assert!(
            error.to_string().contains("/nonexistent/hitui/config.toml"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_config_is_the_defaults() {
        let config = Config::from_str("").expect("an empty config is valid");
        assert_eq!(config.theme.preset, theme::Preset::Terminal);
        assert!(config.keys.is_empty());
        assert!(config.keymaps().unreachable().is_empty());
    }

    #[test]
    fn theme_and_keys_load_together() {
        let config = Config::from_str(
            r##"
            [theme]
            preset = "gruvbox-dark"

            [theme.colors]
            accent = "#fabd2f"

            [keys.browser]
            set = [["MoveDown", "e"]]
            "##,
        )
        .expect("valid config");

        assert_eq!(config.theme.preset, theme::Preset::GruvboxDark);
        let palette = config.palette().unwrap();
        assert_eq!(palette.accent, ratatui::style::Color::Rgb(0xfa, 0xbd, 0x2f));

        let maps = config.keymaps();
        assert_eq!(
            maps.get(Mode::Browser).resolve(&KeyCombo::key("e").0),
            Resolution::Action(Action::MoveDown)
        );
    }

    #[test]
    fn an_unknown_top_level_key_is_rejected() {
        // A typo in a section name would otherwise be silently ignored, and
        // the setting it was meant to carry would do nothing.
        let error = Config::from_str("[thheme]\npreset = \"nord\"").expect_err("unknown table");
        assert!(error.to_string().contains("thheme"), "{error}");
    }

    #[test]
    fn an_unknown_keymap_layer_is_rejected() {
        let error = Config::from_str(
            r##"
            [keys.browser]
            sett = [["MoveDown", "e"]]
            "##,
        )
        .expect_err("unknown layer");
        let message = error.to_string();
        assert!(
            message.contains("sett") || message.contains("unknown"),
            "{message}"
        );
    }

    #[test]
    fn an_unknown_action_is_rejected_with_its_name() {
        let error = Config::from_str(
            r##"
            [keys.browser]
            set = [["MoveDwon", "e"]]
            "##,
        )
        .expect_err("unknown action");
        assert!(error.to_string().contains("MoveDwon"), "{error}");
    }

    #[test]
    fn a_malformed_colour_fails_at_load_not_at_draw() {
        // The terminal has already been taken over by the time the first
        // frame is drawn, so a late failure would leave it in raw mode.
        let error = Config::from_str(
            r##"
            [theme.colors]
            accent = "#12345"
            "##,
        )
        .expect_err("five hex digits");
        assert!(error.to_string().contains("12345"), "{error}");
    }

    #[test]
    fn an_unknown_colour_slot_fails_at_load() {
        let error = Config::from_str(
            r##"
            [theme.colors]
            acent = "#fabd2f"
            "##,
        )
        .expect_err("misspelled slot");
        assert!(error.to_string().contains("acent"), "{error}");
    }

    #[test]
    fn the_config_path_follows_xdg() {
        // Only the shape is asserted; the environment is process-global and
        // this runs alongside other tests.
        if let Some(path) = Config::path() {
            assert!(path.ends_with("hitui/config.toml"), "{}", path.display());
            assert!(path.is_absolute());
        }
    }

    #[test]
    fn the_shipped_example_is_a_valid_config() {
        // The example doubles as documentation, so a stale option in it would
        // be advice that does not work.
        let example = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let text = std::fs::read_to_string(example).expect("config.example.toml is present");
        let config = Config::from_str(&text).expect("the example should load");

        // And it should describe the defaults, so copying it changes nothing.
        assert_eq!(config.theme.preset, theme::Preset::Terminal);
        assert!(
            config.keymaps().unreachable().is_empty(),
            "the example leaves an action unreachable"
        );
    }
}

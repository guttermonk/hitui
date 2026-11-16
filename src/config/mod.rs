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
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::keys::{KeymapSection, Keymaps, Mode};
use theme::{Palette, Theme};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: Theme,
    /// Per-mode keybinding layers.
    pub keys: BTreeMap<Mode, KeymapSection>,
}

impl Config {
    /// Load the config, or the defaults if there is no file.
    pub fn load() -> Result<Self> {
        match Self::path() {
            Some(path) if path.is_file() => Self::from_file(&path),
            _ => Ok(Self::default()),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_str(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn from_str(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        // Resolve the palette now rather than at first draw, so an unknown
        // colour slot is reported before the terminal is taken over.
        config.palette()?;
        Ok(config)
    }

    /// Where the config is read from, if a home directory can be determined.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("hitui").join("config.toml"))
    }

    pub fn palette(&self) -> Result<Palette> {
        Ok(self.theme.palette()?)
    }

    pub fn keymaps(&self) -> Keymaps {
        Keymaps::resolve(&self.keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::combo::KeyCombo;
    use crate::keys::{Action, Resolution};

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

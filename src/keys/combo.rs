//! Key combinations as written in the config.
//!
//! Three spellings, matching the convention the other tools here use:
//!
//! ```toml
//! "j"                            # a plain key
//! { key = "d", mod = ["Ctrl"] }  # with modifiers
//! ["g", "g"]                     # a chord: g then g
//! ```

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A single key, named independently of crossterm so the config spelling is
/// ours rather than a dependency's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Space,
    F(u8),
}

impl Key {
    fn parse(text: &str) -> Option<Self> {
        let lower = text.to_ascii_lowercase();
        Some(match lower.as_str() {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" | "page-up" | "pgup" => Key::PageUp,
            "pagedown" | "page-down" | "pgdn" => Key::PageDown,
            "enter" | "return" | "cr" => Key::Enter,
            "esc" | "escape" => Key::Esc,
            "tab" => Key::Tab,
            "backtab" | "back-tab" | "shift-tab" => Key::BackTab,
            "backspace" | "bs" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "insert" | "ins" => Key::Insert,
            "space" => Key::Space,
            _ => {
                if let Some(number) = lower.strip_prefix('f')
                    && let Ok(n) = number.parse::<u8>()
                    && (1..=12).contains(&n)
                {
                    return Some(Key::F(n));
                }
                // A single character, taken from the original rather than the
                // lowercased copy so "S" stays distinct from "s".
                let mut chars = text.chars();
                let only = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Key::Char(only)
            }
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(c) => write!(f, "{c}"),
            Key::Up => f.write_str("up"),
            Key::Down => f.write_str("down"),
            Key::Left => f.write_str("left"),
            Key::Right => f.write_str("right"),
            Key::Home => f.write_str("home"),
            Key::End => f.write_str("end"),
            Key::PageUp => f.write_str("pageup"),
            Key::PageDown => f.write_str("pagedown"),
            Key::Enter => f.write_str("enter"),
            Key::Esc => f.write_str("esc"),
            Key::Tab => f.write_str("tab"),
            Key::BackTab => f.write_str("backtab"),
            Key::Backspace => f.write_str("backspace"),
            Key::Delete => f.write_str("delete"),
            Key::Insert => f.write_str("insert"),
            Key::Space => f.write_str("space"),
            Key::F(n) => write!(f, "f{n}"),
        }
    }
}

/// Modifier set, as a small bitflag rather than a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const ALT: Mods = Mods(1);
    pub const CTRL: Mods = Mods(2);
    pub const SHIFT: Mods = Mods(4);
    pub const SUPER: Mods = Mods(8);

    pub fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    fn with(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }

    fn parse(name: &str) -> Option<Mods> {
        Some(match name.to_ascii_lowercase().as_str() {
            "alt" | "meta" => Mods::ALT,
            "ctrl" | "control" => Mods::CTRL,
            "shift" => Mods::SHIFT,
            "super" | "cmd" | "win" => Mods::SUPER,
            _ => return None,
        })
    }
}

impl fmt::Display for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in [
            (Mods::CTRL, "ctrl"),
            (Mods::ALT, "alt"),
            (Mods::SHIFT, "shift"),
            (Mods::SUPER, "super"),
        ] {
            if self.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        Ok(())
    }
}

/// A key with its modifiers — one press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModifiedKey {
    pub key: Key,
    pub mods: Mods,
}

impl ModifiedKey {
    /// Translate a terminal event into our own spelling.
    ///
    /// Shift is dropped for character keys: the terminal already reports the
    /// shifted character, so keeping the modifier as well would make `S` fail
    /// to match a binding written as `"S"`.
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        let key = match event.code {
            KeyCode::Char(' ') => Key::Space,
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::BackTab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Insert => Key::Insert,
            KeyCode::F(n) => Key::F(n),
            _ => return None,
        };

        let mut mods = Mods::NONE;
        if event.modifiers.contains(KeyModifiers::ALT) {
            mods = mods.with(Mods::ALT);
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            mods = mods.with(Mods::CTRL);
        }
        if event.modifiers.contains(KeyModifiers::SUPER) {
            mods = mods.with(Mods::SUPER);
        }
        // Shift only counts for keys that have no shifted character of their
        // own, such as the arrows and Tab.
        if event.modifiers.contains(KeyModifiers::SHIFT) && !matches!(key, Key::Char(_)) {
            mods = mods.with(Mods::SHIFT);
        }
        Some(Self { key, mods })
    }
}

impl fmt::Display for ModifiedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.mods, self.key)
    }
}

/// One binding: a single press, or a sequence for a chord.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyCombo(pub Vec<ModifiedKey>);

impl KeyCombo {
    /// Whether this combo begins with `prefix`, for chord matching.
    pub fn starts_with(&self, prefix: &[ModifiedKey]) -> bool {
        self.0.len() >= prefix.len() && self.0[..prefix.len()] == *prefix
    }

    /// A bare arrow key, which `set` does not retire.
    ///
    /// Modified arrows are excluded: `ctrl+down` is somebody's deliberate
    /// binding rather than the key everyone reaches for, so it is replaced
    /// like any other.
    pub fn is_bare_arrow(&self) -> bool {
        matches!(
            self.0.as_slice(),
            [ModifiedKey {
                key: Key::Up | Key::Down | Key::Left | Key::Right,
                mods: Mods::NONE,
            }]
        )
    }

    /// Parse the single-key spelling, for the defaults table.
    pub fn key(text: &str) -> Self {
        Self::parse_one(text).unwrap_or_else(|| panic!("built-in binding {text:?} does not parse"))
    }

    fn parse_one(text: &str) -> Option<Self> {
        // "ctrl+d" is accepted as well as the table form, since it is what
        // people write without thinking about it.
        let mut mods = Mods::NONE;
        let mut rest = text;
        while let Some((head, tail)) = rest.split_once('+') {
            // A literal "+" binding has nothing after it to be a key.
            if tail.is_empty() {
                break;
            }
            match Mods::parse(head) {
                Some(m) => {
                    mods = mods.with(m);
                    rest = tail;
                }
                None => break,
            }
        }
        Some(KeyCombo(vec![ModifiedKey {
            key: Key::parse(rest)?,
            mods,
        }]))
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|k| k.to_string()).collect();
        f.write_str(&parts.join(" "))
    }
}

/// The three config spellings of a combo.
#[derive(Deserialize)]
#[serde(untagged)]
enum ComboSpec {
    Plain(String),
    Modified {
        key: String,
        #[serde(default, rename = "mod")]
        mods: Vec<String>,
    },
    Chord(Vec<ComboSpec>),
}

impl<'de> Deserialize<'de> for KeyCombo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spec = ComboSpec::deserialize(deserializer)?;
        convert(spec).map_err(serde::de::Error::custom)
    }
}

impl Serialize for KeyCombo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

fn convert(spec: ComboSpec) -> Result<KeyCombo, String> {
    match spec {
        ComboSpec::Plain(text) => {
            KeyCombo::parse_one(&text).ok_or_else(|| format!("unrecognised key {text:?}"))
        }
        ComboSpec::Modified { key, mods } => {
            let mut set = Mods::NONE;
            for name in &mods {
                let parsed = Mods::parse(name).ok_or_else(|| {
                    format!("unrecognised modifier {name:?} (expected Alt, Ctrl, Shift or Super)")
                })?;
                set = set.with(parsed);
            }
            Ok(KeyCombo(vec![ModifiedKey {
                key: Key::parse(&key).ok_or_else(|| format!("unrecognised key {key:?}"))?,
                mods: set,
            }]))
        }
        ComboSpec::Chord(parts) => {
            if parts.is_empty() {
                return Err("an empty chord binds nothing".to_string());
            }
            let mut keys = Vec::with_capacity(parts.len());
            for part in parts {
                // A chord of chords has no meaning; flatten one level only.
                let combo = convert(part)?;
                keys.extend(combo.0);
            }
            Ok(KeyCombo(keys))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> KeyCombo {
        KeyCombo::parse_one(text).expect("parses")
    }

    #[test]
    fn plain_keys_parse() {
        assert_eq!(combo("j").0[0].key, Key::Char('j'));
        assert_eq!(combo("enter").0[0].key, Key::Enter);
        assert_eq!(combo("pgdn").0[0].key, Key::PageDown);
        assert_eq!(combo("f5").0[0].key, Key::F(5));
    }

    #[test]
    fn case_is_preserved_for_characters_but_not_names() {
        // "S" and "s" are different bindings; "ESC" and "esc" are not.
        assert_eq!(combo("S").0[0].key, Key::Char('S'));
        assert_eq!(combo("s").0[0].key, Key::Char('s'));
        assert_eq!(combo("ESC").0[0].key, Key::Esc);
    }

    #[test]
    fn modifiers_parse_in_the_inline_spelling() {
        let c = combo("ctrl+d");
        assert_eq!(c.0[0].key, Key::Char('d'));
        assert!(c.0[0].mods.contains(Mods::CTRL));

        let c = combo("ctrl+alt+left");
        assert_eq!(c.0[0].key, Key::Left);
        assert!(c.0[0].mods.contains(Mods::CTRL));
        assert!(c.0[0].mods.contains(Mods::ALT));
    }

    #[test]
    fn a_literal_plus_is_still_a_key() {
        // "+" has to survive the modifier splitting.
        assert_eq!(combo("+").0[0].key, Key::Char('+'));
    }

    #[test]
    fn unrecognised_keys_are_rejected() {
        assert!(KeyCombo::parse_one("").is_none());
        assert!(KeyCombo::parse_one("nonsense").is_none());
        assert!(KeyCombo::parse_one("f99").is_none());
    }

    #[test]
    fn the_three_config_spellings_all_work() {
        #[derive(Deserialize)]
        struct Holder {
            binding: KeyCombo,
        }

        let plain: Holder = toml::from_str(r#"binding = "j""#).unwrap();
        assert_eq!(plain.binding.0.len(), 1);

        let modified: Holder =
            toml::from_str(r#"binding = { key = "d", mod = ["Ctrl"] }"#).unwrap();
        assert!(modified.binding.0[0].mods.contains(Mods::CTRL));

        let chord: Holder = toml::from_str(r#"binding = ["g", "g"]"#).unwrap();
        assert_eq!(chord.binding.0.len(), 2, "a chord keeps both presses");
    }

    #[test]
    fn shift_is_dropped_for_characters_so_capitals_match() {
        // The terminal reports Shift+s as 'S' *and* sets the shift modifier.
        // Keeping it would stop "S" in a config from ever matching.
        let event = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        let pressed = ModifiedKey::from_event(event).unwrap();
        assert_eq!(pressed.key, Key::Char('S'));
        assert_eq!(pressed.mods, Mods::NONE);
        assert_eq!(pressed, combo("S").0[0]);
    }

    #[test]
    fn shift_is_kept_for_keys_with_no_shifted_character() {
        let event = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
        let pressed = ModifiedKey::from_event(event).unwrap();
        assert!(pressed.mods.contains(Mods::SHIFT));
    }

    #[test]
    fn space_is_a_named_key_not_a_character() {
        // Otherwise `"space"` in a config and the spacebar would disagree.
        let event = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(ModifiedKey::from_event(event).unwrap().key, Key::Space);
        assert_eq!(combo("space").0[0].key, Key::Space);
    }

    #[test]
    fn bare_arrows_are_recognised_for_the_set_rule() {
        assert!(combo("down").is_bare_arrow());
        assert!(combo("left").is_bare_arrow());
        assert!(!combo("j").is_bare_arrow());
        // A deliberately modified arrow is an ordinary binding.
        assert!(!combo("ctrl+down").is_bare_arrow());
    }

    #[test]
    fn prefix_matching_supports_chords() {
        let gg = KeyCombo(vec![combo("g").0[0], combo("g").0[0]]);
        assert!(gg.starts_with(&[combo("g").0[0]]));
        assert!(!gg.starts_with(&[combo("d").0[0]]));
        assert!(gg.starts_with(&[]), "every combo starts with nothing");
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        for text in ["j", "enter", "ctrl+d", "f5", "space", "down"] {
            let rendered = combo(text).to_string();
            assert_eq!(combo(&rendered), combo(text), "{text} -> {rendered}");
        }
    }
}

//! Keybindings.
//!
//! Defaults are vim-style — `h`/`j`/`k`/`l` to move and switch panes — and
//! every one of them is replaceable from the config. Four tables per mode,
//! applied in this order:
//!
//! - `base` — start from nothing but this, replacing the defaults entirely.
//! - `set` — these *actions* get exactly these keys, losing the ones they
//!   had. The common case, and the one `extra` cannot express.
//! - `extra` — these *keys* get these actions, on top of whatever is there.
//! - `unbind` — these keys do nothing. Applied last, so it is a veto.

pub mod combo;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use combo::{KeyCombo, ModifiedKey};

/// Something the interface can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    /// Move down the sidebar, or scroll the detail pane down.
    MoveDown,
    MoveUp,
    /// Focus the sidebar, which is the left pane.
    FocusSidebar,
    /// Focus the detail pane, on the right.
    FocusDetail,
    /// Swap focus between the two.
    TogglePane,
    PageDown,
    PageUp,
    Top,
    Bottom,
    /// Start typing a field filter.
    Search,
    /// Drop the current filter.
    ClearFilter,
    /// Re-collect the current page.
    Refresh,
    /// Run the page's action, where it has one.
    Activate,
    Help,
    Quit,
}

impl Action {
    /// Every action, for the `keys` listing and for checking coverage.
    pub const ALL: [Action; 15] = [
        Action::MoveDown,
        Action::MoveUp,
        Action::FocusSidebar,
        Action::FocusDetail,
        Action::TogglePane,
        Action::PageDown,
        Action::PageUp,
        Action::Top,
        Action::Bottom,
        Action::Search,
        Action::ClearFilter,
        Action::Refresh,
        Action::Activate,
        Action::Help,
        Action::Quit,
    ];

    /// One line on what it does, for `hitui keys`.
    pub fn describe(self) -> &'static str {
        match self {
            Action::MoveDown => "move down the sidebar, or scroll the detail pane",
            Action::MoveUp => "move up the sidebar, or scroll the detail pane",
            Action::FocusSidebar => "focus the sidebar",
            Action::FocusDetail => "focus the detail pane",
            Action::TogglePane => "swap focus between the panes",
            Action::PageDown => "scroll a screen down",
            Action::PageUp => "scroll a screen up",
            Action::Top => "jump to the top",
            Action::Bottom => "jump to the bottom",
            Action::Search => "filter fields on this page",
            Action::ClearFilter => "clear the filter",
            Action::Refresh => "re-collect this page",
            Action::Activate => "run this page's action, where it has one",
            Action::Help => "show the key help",
            Action::Quit => "quit",
        }
    }
}

/// Which keymap applies. `j` moves in the browser and is literal text in the
/// search field, so they cannot share one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Browser,
    Search,
    Help,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Browser, Mode::Search, Mode::Help];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Browser => "browser",
            Mode::Search => "search",
            Mode::Help => "help",
        }
    }
}

/// Bindings for one mode, in priority order: the first match wins, so a
/// user's `extra` can shadow a default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Keymap(pub Vec<(Action, KeyCombo)>);

/// What a sequence of presses amounts to.
#[derive(Debug, PartialEq)]
pub enum Resolution {
    /// A complete binding.
    Action(Action),
    /// A prefix of at least one binding — wait for more.
    Pending,
    /// Nothing starts with these keys.
    None,
}

impl Keymap {
    pub fn iter(&self) -> std::slice::Iter<'_, (Action, KeyCombo)> {
        self.0.iter()
    }

    /// What the keys pressed so far mean.
    pub fn resolve(&self, pressed: &[ModifiedKey]) -> Resolution {
        if pressed.is_empty() {
            return Resolution::None;
        }
        // An exact match wins over a longer binding it prefixes, because
        // otherwise a short binding could never fire.
        if let Some((action, _)) = self
            .0
            .iter()
            .find(|(_, combo)| combo.0.as_slice() == pressed)
        {
            return Resolution::Action(*action);
        }
        if self.0.iter().any(|(_, combo)| combo.starts_with(pressed)) {
            return Resolution::Pending;
        }
        Resolution::None
    }

    /// Keys bound to an action, for the help listing.
    pub fn combos_for(&self, action: Action) -> Vec<&KeyCombo> {
        self.0
            .iter()
            .filter(|(a, _)| *a == action)
            .map(|(_, combo)| combo)
            .collect()
    }
}

/// The `[keys.<mode>]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeymapSection {
    /// Replaces the defaults for this mode entirely.
    pub base: Option<Keymap>,
    /// Moves whole actions onto new keys. See [`apply_set`].
    pub set: Option<Keymap>,
    /// Layered on top, winning any collision.
    pub extra: Option<Keymap>,
    /// Removed last, so nothing reinstates them. Prefix-based: unbinding `g`
    /// also removes the `gg` chord, or `g` would still be swallowed while
    /// hitui waited for the rest of a chord.
    pub unbind: Option<Vec<KeyCombo>>,
}

/// Resolved bindings for every mode.
#[derive(Debug, Clone)]
pub struct Keymaps(BTreeMap<Mode, Keymap>);

impl Default for Keymaps {
    fn default() -> Self {
        Self::resolve(&BTreeMap::new())
    }
}

impl Keymaps {
    /// Apply the config's layers over the defaults.
    pub fn resolve(sections: &BTreeMap<Mode, KeymapSection>) -> Self {
        let mut maps = BTreeMap::new();

        for mode in Mode::ALL {
            let section = sections.get(&mode);
            let mut map = section
                .and_then(|s| s.base.clone())
                .unwrap_or_else(|| defaults(mode));

            if let Some(set) = section.and_then(|s| s.set.as_ref()) {
                apply_set(&mut map, set);
            }
            if let Some(extra) = section.and_then(|s| s.extra.as_ref()) {
                apply_extra(&mut map, extra);
            }
            if let Some(unbind) = section.and_then(|s| s.unbind.as_ref()) {
                apply_unbind(&mut map, unbind);
            }
            maps.insert(mode, map);
        }
        Self(maps)
    }

    pub fn get(&self, mode: Mode) -> &Keymap {
        self.0.get(&mode).expect("every mode is resolved")
    }

    pub fn iter(&self) -> impl Iterator<Item = (Mode, &Keymap)> {
        self.0.iter().map(|(mode, map)| (*mode, map))
    }

    /// Actions in the browser that no key reaches, which `base`, `set` and
    /// `unbind` can all cause and which is otherwise silent.
    pub fn unreachable(&self) -> Vec<Action> {
        let browser = self.get(Mode::Browser);
        Action::ALL
            .into_iter()
            .filter(|action| browser.combos_for(*action).is_empty())
            .collect()
    }
}

/// Move whole actions onto new keys.
///
/// Two things are dropped before the new bindings go on: every existing
/// binding *of a listed action*, which is the point, and every binding whose
/// combo the new keys make unreachable, which is the same prefix rule
/// `apply_extra` needs and for the same reason.
///
/// **Arrow keys survive.** `set = [["MoveDown", "e"]]` means "move down is on
/// `e` now", and nobody means by that "and the Down arrow should stop
/// working". The arrows are not part of the layout being chosen — they are
/// the keys that work when you have not learned the layout, including for
/// whoever else sits at the keyboard. Retiring one is still possible, and is
/// something you say rather than something you cause: `unbind = ["down"]`
/// runs afterwards.
fn apply_set(map: &mut Keymap, set: &Keymap) {
    let rebound: Vec<Action> = set.iter().map(|(action, _)| *action).collect();

    map.0.retain(|(action, combo)| {
        if combo.is_bare_arrow() {
            return true;
        }
        !rebound.contains(action)
    });
    apply_extra(map, set);
}

/// Layer bindings on top, winning any collision.
///
/// A new binding removes anything it would make unreachable: binding `d`
/// displaces an existing `dd`, since `d` would otherwise fire before the
/// chord could complete.
fn apply_extra(map: &mut Keymap, extra: &Keymap) {
    for (action, combo) in extra.iter() {
        map.0.retain(|(_, existing)| {
            !existing.starts_with(&combo.0) && !combo.starts_with(&existing.0)
        });
        // Pushed to the front so it wins over anything equivalent left behind.
        map.0.insert(0, (*action, combo.clone()));
    }
}

/// Remove bindings, prefix-first.
fn apply_unbind(map: &mut Keymap, unbind: &[KeyCombo]) {
    map.0
        .retain(|(_, combo)| !unbind.iter().any(|dead| combo.starts_with(&dead.0)));
}

/// The built-in bindings for a mode.
pub fn defaults(mode: Mode) -> Keymap {
    use Action::*;
    let key = KeyCombo::key;

    let bindings: Vec<(Action, KeyCombo)> = match mode {
        Mode::Browser => vec![
            // vim motion, and the arrows alongside it.
            (MoveDown, key("j")),
            (MoveDown, key("down")),
            (MoveUp, key("k")),
            (MoveUp, key("up")),
            (FocusSidebar, key("h")),
            (FocusSidebar, key("left")),
            (FocusDetail, key("l")),
            (FocusDetail, key("right")),
            (TogglePane, key("tab")),
            (TogglePane, key("backtab")),
            (PageDown, key("pagedown")),
            (PageDown, key("ctrl+d")),
            (PageUp, key("pageup")),
            (PageUp, key("ctrl+u")),
            (Top, key("g")),
            (Top, key("home")),
            (Bottom, key("G")),
            (Bottom, key("end")),
            (Search, key("/")),
            (ClearFilter, key("esc")),
            (Refresh, key("r")),
            (Activate, key("enter")),
            (Help, key("?")),
            (Quit, key("q")),
            (Quit, key("ctrl+c")),
        ],
        // In the search field printable keys are literal text, so only the
        // keys that cannot be typed are bound.
        Mode::Search => vec![
            (Activate, key("enter")),
            (ClearFilter, key("esc")),
            (Quit, key("ctrl+c")),
        ],
        // Any key dismisses the help overlay; these are the ones that also
        // do something.
        Mode::Help => vec![(Help, key("esc")), (Quit, key("q")), (Quit, key("ctrl+c"))],
    };
    Keymap(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(toml: &str) -> BTreeMap<Mode, KeymapSection> {
        #[derive(Deserialize)]
        struct Holder {
            keys: BTreeMap<Mode, KeymapSection>,
        }
        let holder: Holder = toml::from_str(toml).expect("valid keys config");
        holder.keys
    }

    fn pressed(text: &str) -> Vec<ModifiedKey> {
        KeyCombo::key(text).0
    }

    #[test]
    fn hjkl_works_out_of_the_box() {
        let maps = Keymaps::default();
        let browser = maps.get(Mode::Browser);

        assert_eq!(
            browser.resolve(&pressed("j")),
            Resolution::Action(Action::MoveDown)
        );
        assert_eq!(
            browser.resolve(&pressed("k")),
            Resolution::Action(Action::MoveUp)
        );
        assert_eq!(
            browser.resolve(&pressed("h")),
            Resolution::Action(Action::FocusSidebar)
        );
        assert_eq!(
            browser.resolve(&pressed("l")),
            Resolution::Action(Action::FocusDetail)
        );
    }

    #[test]
    fn arrows_work_alongside_the_letters() {
        let browser = Keymaps::default();
        let browser = browser.get(Mode::Browser);
        assert_eq!(
            browser.resolve(&pressed("down")),
            Resolution::Action(Action::MoveDown)
        );
        assert_eq!(
            browser.resolve(&pressed("right")),
            Resolution::Action(Action::FocusDetail)
        );
    }

    #[test]
    fn every_action_is_reachable_by_default() {
        // A default keymap that left an action unbound would be a feature
        // nobody can reach.
        assert_eq!(
            Keymaps::default().unreachable(),
            Vec::new(),
            "some action has no default key"
        );
    }

    #[test]
    fn set_moves_an_action_to_a_colemak_layout() {
        // The case from the brief: n/e/i/o instead of h/j/k/l.
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            set = [
                ["MoveDown",     "e"],
                ["MoveUp",       "i"],
                ["FocusSidebar", "n"],
                ["FocusDetail",  "o"],
            ]
            "#,
        ));
        let browser = maps.get(Mode::Browser);

        assert_eq!(
            browser.resolve(&pressed("e")),
            Resolution::Action(Action::MoveDown)
        );
        assert_eq!(
            browser.resolve(&pressed("i")),
            Resolution::Action(Action::MoveUp)
        );
        assert_eq!(
            browser.resolve(&pressed("n")),
            Resolution::Action(Action::FocusSidebar)
        );
        assert_eq!(
            browser.resolve(&pressed("o")),
            Resolution::Action(Action::FocusDetail)
        );

        // The old keys are retired, which is what distinguishes set from extra.
        assert_eq!(browser.resolve(&pressed("j")), Resolution::None);
        assert_eq!(browser.resolve(&pressed("h")), Resolution::None);

        // Everything else is untouched.
        assert_eq!(
            browser.resolve(&pressed("r")),
            Resolution::Action(Action::Refresh)
        );
        assert_eq!(
            browser.resolve(&pressed("q")),
            Resolution::Action(Action::Quit)
        );
    }

    #[test]
    fn set_leaves_the_arrows_alone() {
        // Rebinding j to e is not a statement about the Down arrow.
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            set = [["MoveDown", "e"]]
            "#,
        ));
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("down")),
            Resolution::Action(Action::MoveDown),
            "the Down arrow should still move down"
        );
    }

    #[test]
    fn unbind_can_retire_an_arrow_when_asked() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            unbind = ["down"]
            "#,
        ));
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("down")),
            Resolution::None
        );
        // And only that one.
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("up")),
            Resolution::Action(Action::MoveUp)
        );
    }

    #[test]
    fn extra_adds_without_removing() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            extra = [["MoveDown", "e"]]
            "#,
        ));
        let browser = maps.get(Mode::Browser);

        assert_eq!(
            browser.resolve(&pressed("e")),
            Resolution::Action(Action::MoveDown)
        );
        assert_eq!(
            browser.resolve(&pressed("j")),
            Resolution::Action(Action::MoveDown),
            "extra is additive, so j keeps working"
        );
    }

    #[test]
    fn base_replaces_the_defaults_entirely() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            base = [["Quit", "x"]]
            "#,
        ));
        let browser = maps.get(Mode::Browser);

        assert_eq!(
            browser.resolve(&pressed("x")),
            Resolution::Action(Action::Quit)
        );
        assert_eq!(browser.resolve(&pressed("j")), Resolution::None);
        // And the gap is reportable rather than silent.
        assert!(maps.unreachable().contains(&Action::MoveDown));
    }

    #[test]
    fn chords_wait_for_their_second_key() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            extra = [["Quit", ["Z", "Z"]]]
            "#,
        ));
        let browser = maps.get(Mode::Browser);

        assert_eq!(browser.resolve(&pressed("Z")), Resolution::Pending);
        let zz = vec![pressed("Z")[0], pressed("Z")[0]];
        assert_eq!(browser.resolve(&zz), Resolution::Action(Action::Quit));
    }

    #[test]
    fn a_new_binding_displaces_a_chord_it_would_shadow() {
        // Binding `g` alone must remove `gg`, or `g` fires first and the
        // chord can never complete.
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            extra = [["Bottom", ["g", "g"]]]
            "#,
        ));
        let browser = maps.get(Mode::Browser);

        // The default `g` for Top is gone, displaced by the chord.
        assert_eq!(browser.resolve(&pressed("g")), Resolution::Pending);
        assert!(
            browser
                .combos_for(Action::Top)
                .iter()
                .all(|c| c.0.len() != 1 || c.0[0] != pressed("g")[0])
        );
    }

    #[test]
    fn unbind_is_prefix_based() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            extra = [["Quit", ["Z", "Z"]]]
            unbind = ["Z"]
            "#,
        ));
        // Removing Z must also remove the ZZ chord, or Z would still be
        // swallowed while hitui waited for a chord that can never fire.
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("Z")),
            Resolution::None
        );
    }

    #[test]
    fn unbind_runs_after_everything_else() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            extra = [["MoveDown", "e"]]
            unbind = ["e"]
            "#,
        ));
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("e")),
            Resolution::None
        );
    }

    #[test]
    fn rebinding_an_action_to_a_key_it_already_had_keeps_one_binding() {
        // Actions are cleared before the new bindings go on, so this must not
        // end with zero.
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            set = [["MoveDown", "j"]]
            "#,
        ));
        let browser = maps.get(Mode::Browser);
        assert_eq!(
            browser.resolve(&pressed("j")),
            Resolution::Action(Action::MoveDown)
        );
        // j plus the surviving Down arrow.
        assert_eq!(browser.combos_for(Action::MoveDown).len(), 2);
    }

    #[test]
    fn search_mode_binds_only_untypable_keys() {
        // Printable keys have to stay available as literal text.
        let search = Keymaps::default();
        let search = search.get(Mode::Search);
        assert_eq!(search.resolve(&pressed("j")), Resolution::None);
        assert_eq!(
            search.resolve(&pressed("esc")),
            Resolution::Action(Action::ClearFilter)
        );
    }

    #[test]
    fn modes_are_configured_independently() {
        let maps = Keymaps::resolve(&section(
            r#"
            [keys.browser]
            set = [["Quit", "x"]]
            "#,
        ));
        // The browser changed; search kept its own default.
        assert_eq!(
            maps.get(Mode::Browser).resolve(&pressed("x")),
            Resolution::Action(Action::Quit)
        );
        assert_eq!(
            maps.get(Mode::Search).resolve(&pressed("x")),
            Resolution::None
        );
    }
}

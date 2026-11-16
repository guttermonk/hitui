//! Application state and the event loop.

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::collect::{Collector, Group, registry};
use crate::config::Config;
use crate::config::theme::{self, Palette};
use crate::keys::combo::ModifiedKey;
use crate::keys::{Action, Keymaps, Mode, Resolution};
use crate::model::{Node, Value};
use crate::ui;

/// How long to block waiting for input before re-rendering. Also sets the
/// granularity of auto-refresh for live pages.
const TICK: Duration = Duration::from_millis(250);

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Sidebar,
    Detail,
}

/// One flattened line of a report tree.
///
/// Keeps the typed value rather than a pre-formatted string so the renderer can
/// draw gauges and sparklines as graphics instead of text.
pub struct Row {
    pub depth: usize,
    pub label: String,
    pub value: Option<Value>,
    /// External tool this came from, shown beside group headings.
    pub source: Option<String>,
    pub is_group: bool,
}

impl Row {
    /// A blank spacer between top-level sections.
    fn spacer() -> Self {
        Self {
            depth: 0,
            label: String::new(),
            value: None,
            source: None,
            is_group: false,
        }
    }

    pub fn is_spacer(&self) -> bool {
        self.label.is_empty() && self.value.is_none() && !self.is_group
    }

    fn matches(&self, needle: &str) -> bool {
        self.label.to_lowercase().contains(needle)
            || self
                .value
                .as_ref()
                .is_some_and(|v| v.to_string().to_lowercase().contains(needle))
    }
}

/// Sidebar entries: group headings are not selectable, pages are.
pub enum SidebarItem {
    Header(Group),
    /// Index into [`App::pages`].
    Page(usize),
}

pub struct Page {
    collector: Box<dyn Collector>,
    pub rows: Vec<Row>,
    pub error: Option<String>,
    collected_at: Option<Instant>,
    /// Scroll offset in *rendered* lines, clamped during draw.
    pub scroll: usize,
    /// Rendered line count and viewport height, written back by the renderer so
    /// that key handling can page and clamp without knowing the layout.
    pub total_lines: usize,
    pub viewport: usize,
}

impl Page {
    fn new(collector: Box<dyn Collector>) -> Self {
        Self {
            collector,
            rows: Vec::new(),
            error: None,
            collected_at: None,
            scroll: 0,
            total_lines: 0,
            viewport: 0,
        }
    }

    pub fn title(&self) -> &'static str {
        self.collector.title()
    }

    pub fn id(&self) -> &'static str {
        self.collector.id()
    }

    pub fn group(&self) -> Group {
        self.collector.group()
    }

    pub fn is_live(&self) -> bool {
        self.collector.refresh_interval().is_some()
    }

    /// Label for this page's Enter action, if it has one.
    pub fn action(&self) -> Option<&'static str> {
        self.collector.action()
    }

    /// Re-run the collector, preserving the scroll position.
    pub fn reload(&mut self) {
        match self.collector.collect() {
            Ok(nodes) => {
                let mut rows = Vec::new();
                for (i, node) in nodes.iter().enumerate() {
                    if i > 0 {
                        rows.push(Row::spacer());
                    }
                    flatten(std::slice::from_ref(node), 0, &mut rows);
                }
                self.rows = rows;
                self.error = None;
            }
            Err(err) => {
                self.rows.clear();
                self.error = Some(format!("{err:#}"));
            }
        }
        self.collected_at = Some(Instant::now());
    }

    fn needs_reload(&self) -> bool {
        match self.collected_at {
            None => true,
            Some(at) => self
                .collector
                .refresh_interval()
                .is_some_and(|interval| at.elapsed() >= interval),
        }
    }

    fn max_scroll(&self) -> usize {
        self.total_lines.saturating_sub(self.viewport)
    }
}

fn flatten(nodes: &[Node], depth: usize, out: &mut Vec<Row>) {
    for node in nodes {
        out.push(Row {
            depth,
            label: node.label.clone(),
            value: node.value.clone(),
            source: node.source.clone(),
            is_group: node.is_group(),
        });
        flatten(&node.children, depth + 1, out);
    }
}

/// Rows to show for a given filter.
///
/// A matching row keeps its enclosing groups for context, and a group whose own
/// label matches brings its whole subtree along — searching "cache" should show
/// the cache section, not just its heading.
pub fn visible_rows<'a>(rows: &'a [Row], filter: &str) -> Vec<&'a Row> {
    if filter.is_empty() {
        return rows.iter().collect();
    }
    let needle = filter.to_lowercase();
    let mut out = Vec::new();
    // Ancestor groups at each depth, with whether they have been emitted yet.
    let mut ancestors: Vec<(&Row, bool)> = Vec::new();
    // Depth of the matched group whose subtree we are currently inside.
    let mut subtree: Option<usize> = None;

    for row in rows {
        if row.is_spacer() {
            continue;
        }
        if let Some(depth) = subtree
            && row.depth <= depth
        {
            subtree = None;
        }
        ancestors.truncate(row.depth);

        if subtree.is_some() {
            out.push(row);
            if row.is_group {
                ancestors.push((row, true));
            }
            continue;
        }

        if row.matches(&needle) {
            for (ancestor, emitted) in ancestors.iter_mut() {
                if !*emitted {
                    out.push(ancestor);
                    *emitted = true;
                }
            }
            out.push(row);
            if row.is_group {
                ancestors.push((row, true));
                subtree = Some(row.depth);
            }
        } else if row.is_group {
            ancestors.push((row, false));
        }
    }
    out
}

pub struct App {
    pub pages: Vec<Page>,
    pub sidebar: Vec<SidebarItem>,
    /// Index into [`Self::sidebar`]; always points at a `Page`.
    pub selected: usize,
    pub focus: Focus,
    pub show_help: bool,
    /// Row filter. Typed live when `searching`, then kept until cleared.
    pub filter: String,
    pub searching: bool,
    /// Resolved colours, from `[theme]`.
    pub palette: Palette,
    /// Resolved bindings, from `[keys.*]`.
    pub keymaps: Keymaps,
    /// Keys pressed so far towards a chord. Empty unless one is in progress.
    pending: Vec<ModifiedKey>,
    quit: bool,
}

impl App {
    /// Build with the defaults.
    ///
    /// Only tests need this: the binary always has a `Config`, defaulted
    /// when there is no file.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_config(&Config::default())
    }

    pub fn with_config(config: &Config) -> Self {
        let pages: Vec<Page> = registry().into_iter().map(Page::new).collect();

        // Build the sidebar by walking the groups in display order, emitting a
        // heading only for groups that actually have pages.
        let mut sidebar = Vec::new();
        for group in Group::ALL {
            let members: Vec<usize> = pages
                .iter()
                .enumerate()
                .filter(|(_, p)| p.group() == group)
                .map(|(i, _)| i)
                .collect();
            if members.is_empty() {
                continue;
            }
            sidebar.push(SidebarItem::Header(group));
            sidebar.extend(members.into_iter().map(SidebarItem::Page));
        }

        let selected = sidebar
            .iter()
            .position(|item| matches!(item, SidebarItem::Page(_)))
            .unwrap_or(0);

        Self {
            pages,
            sidebar,
            selected,
            focus: Focus::Sidebar,
            show_help: false,
            filter: String::new(),
            searching: false,
            // Already validated when the config loaded, so this cannot fail
            // here; the default palette is a safe fallback regardless.
            palette: config
                .palette()
                .unwrap_or_else(|_| theme::Preset::Terminal.palette()),
            keymaps: config.keymaps(),
            pending: Vec::new(),
            quit: false,
        }
    }

    /// Which keymap applies right now.
    fn mode(&self) -> Mode {
        if self.searching {
            Mode::Search
        } else if self.show_help {
            Mode::Help
        } else {
            Mode::Browser
        }
    }

    /// Select the page with this collector id. Returns false if there is none.
    ///
    /// Page ids index [`Self::pages`], but `selected` indexes [`Self::sidebar`],
    /// which interleaves group headings — so callers must go through here
    /// rather than assigning `selected` directly.
    pub fn select_page_by_id(&mut self, id: &str) -> bool {
        let found = self.sidebar.iter().position(|item| match item {
            SidebarItem::Page(i) => self.pages[*i].id() == id,
            SidebarItem::Header(_) => false,
        });
        if let Some(index) = found {
            self.selected = index;
        }
        found.is_some()
    }

    fn page_index(&self) -> usize {
        match self.sidebar[self.selected] {
            SidebarItem::Page(i) => i,
            // `selected` is maintained to always land on a page.
            SidebarItem::Header(_) => 0,
        }
    }

    pub fn page(&self) -> &Page {
        &self.pages[self.page_index()]
    }

    pub fn page_mut(&mut self) -> &mut Page {
        let index = self.page_index();
        &mut self.pages[index]
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            if self.page().needs_reload() {
                self.page_mut().reload();
            }
            terminal.draw(|frame| ui::draw(frame, &mut self))?;

            if event::poll(TICK)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        let Some(pressed) = ModifiedKey::from_event(key) else {
            return;
        };
        let mode = self.mode();

        // Accumulate towards a chord, then ask the keymap what it means.
        self.pending.push(pressed);
        match self.keymaps.get(mode).resolve(&self.pending) {
            // A prefix of something longer: wait for the rest.
            Resolution::Pending => return,
            Resolution::Action(action) => {
                self.pending.clear();
                self.dispatch(action);
                return;
            }
            Resolution::None => self.pending.clear(),
        }

        // Nothing bound. What an unbound key does depends on the mode.
        match mode {
            // Printable keys are the filter text, which is why so little is
            // bound in this mode.
            Mode::Search => {
                match key.code {
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.filter.clear();
                    }
                    KeyCode::Char(c) => self.filter.push(c),
                    KeyCode::Backspace => {
                        self.filter.pop();
                    }
                    _ => {}
                }
                // Keep the top of the matches in view as the query narrows.
                self.page_mut().scroll = 0;
            }
            // Any key dismisses the overlay.
            Mode::Help => self.show_help = false,
            Mode::Browser => {
                // Digits jump to a page by ordinal. Checked *after* the
                // keymap, so a digit somebody binds themselves wins.
                if let KeyCode::Char(c @ '1'..='9') = key.code {
                    self.jump_to_page(c as usize - '1' as usize);
                }
            }
        }
    }

    /// Carry out a resolved action.
    fn dispatch(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::MoveDown => self.step(1),
            Action::MoveUp => self.step(-1),
            Action::FocusSidebar => self.focus = Focus::Sidebar,
            Action::FocusDetail => self.focus = Focus::Detail,
            Action::TogglePane => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Detail,
                    Focus::Detail => Focus::Sidebar,
                }
            }
            Action::PageDown => self.page_by(1),
            Action::PageUp => self.page_by(-1),
            Action::Top => match self.focus {
                Focus::Sidebar => self.select_first(),
                Focus::Detail => self.page_mut().scroll = 0,
            },
            Action::Bottom => match self.focus {
                Focus::Sidebar => self.select_last(),
                Focus::Detail => {
                    let page = self.page_mut();
                    page.scroll = page.max_scroll();
                }
            },
            Action::Search => {
                self.searching = true;
                self.focus = Focus::Detail;
            }
            // One key, doing the least destructive thing available: leave the
            // filter field, else drop the filter, else close the help, else
            // quit. That ordering is why Esc is safe to press twice.
            Action::ClearFilter => {
                if self.searching {
                    self.searching = false;
                    self.filter.clear();
                    self.page_mut().scroll = 0;
                } else if self.show_help {
                    self.show_help = false;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                } else {
                    self.quit = true;
                }
            }
            Action::Refresh => self.page_mut().reload(),
            Action::Activate => {
                if self.searching {
                    // Commit the filter and leave the field.
                    self.searching = false;
                    return;
                }
                // Only pages that advertise an action respond, so this cannot
                // start work on a page the user merely scrolled onto.
                let page = self.page_mut();
                if page.collector.action().is_some() {
                    page.collector.activate();
                    // Re-collect at once so the page reflects the start rather
                    // than waiting for the next tick.
                    page.reload();
                }
            }
            Action::Help => self.show_help = !self.show_help,
        }
    }

    /// Select the nth page, skipping group headings.
    fn jump_to_page(&mut self, ordinal: usize) {
        if let Some(index) = self
            .sidebar
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, SidebarItem::Page(_)))
            .nth(ordinal)
            .map(|(i, _)| i)
        {
            self.selected = index;
        }
    }

    fn step(&mut self, delta: isize) {
        match self.focus {
            Focus::Sidebar => self.move_selection(delta),
            Focus::Detail => self.scroll_by(delta),
        }
    }

    /// Move to the next or previous selectable page, skipping group headings.
    fn move_selection(&mut self, delta: isize) {
        let mut index = self.selected as isize;
        loop {
            index += delta;
            if index < 0 || index as usize >= self.sidebar.len() {
                return; // Already at an end; leave the selection where it is.
            }
            if matches!(self.sidebar[index as usize], SidebarItem::Page(_)) {
                self.selected = index as usize;
                return;
            }
        }
    }

    fn select_first(&mut self) {
        if let Some(index) = self
            .sidebar
            .iter()
            .position(|item| matches!(item, SidebarItem::Page(_)))
        {
            self.selected = index;
        }
    }

    fn select_last(&mut self) {
        if let Some(index) = self
            .sidebar
            .iter()
            .rposition(|item| matches!(item, SidebarItem::Page(_)))
        {
            self.selected = index;
        }
    }

    fn page_by(&mut self, direction: isize) {
        match self.focus {
            Focus::Sidebar => self.step(direction),
            Focus::Detail => {
                let jump = self.page().viewport.saturating_sub(1).max(1) as isize;
                self.scroll_by(direction * jump);
            }
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let page = self.page_mut();
        let max = page.max_scroll() as isize;
        page.scroll = (page.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row {
                depth: 0,
                label: "Cache".into(),
                value: None,
                source: None,
                is_group: true,
            },
            Row {
                depth: 1,
                label: "L1 Data".into(),
                value: Some(Value::Text("4 × 32.00 KiB".into())),
                source: None,
                is_group: false,
            },
            Row {
                depth: 0,
                label: "Clocks".into(),
                value: None,
                source: None,
                is_group: true,
            },
            Row {
                depth: 1,
                label: "Governor".into(),
                value: Some(Value::Text("powersave".into())),
                source: None,
                is_group: false,
            },
        ]
    }

    #[test]
    fn empty_filter_keeps_everything() {
        assert_eq!(visible_rows(&rows(), "").len(), 4);
    }

    #[test]
    fn matching_row_keeps_its_group_for_context() {
        let rows = rows();
        let visible = visible_rows(&rows, "powersave");
        let labels: Vec<&str> = visible.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Clocks", "Governor"]);
    }

    #[test]
    fn matching_group_brings_its_subtree() {
        let rows = rows();
        let visible = visible_rows(&rows, "cache");
        let labels: Vec<&str> = visible.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Cache", "L1 Data"]);
    }

    #[test]
    fn sidebar_groups_pages_under_headings() {
        let app = App::new();
        // Memory Usage sits under Computer, Processor under Devices.
        assert!(matches!(
            app.sidebar[0],
            SidebarItem::Header(Group::Computer)
        ));
        assert!(matches!(app.sidebar[1], SidebarItem::Page(_)));
        // The initial selection must be a page, never a heading.
        assert!(matches!(app.sidebar[app.selected], SidebarItem::Page(_)));
    }

    #[test]
    fn select_page_by_id_lands_on_that_page() {
        // Regression: `selected` indexes the sidebar, which interleaves group
        // headings, so a page index is not interchangeable with it.
        let mut app = App::new();
        assert!(app.select_page_by_id("cpu"));
        assert_eq!(app.page().id(), "cpu");
        assert!(app.select_page_by_id("memory"));
        assert_eq!(app.page().id(), "memory");

        assert!(!app.select_page_by_id("nope"));
        assert_eq!(
            app.page().id(),
            "memory",
            "a failed lookup must leave the selection alone"
        );
    }

    #[test]
    fn navigation_skips_group_headings() {
        let mut app = App::new();
        let first = app.selected;
        app.move_selection(1);
        assert_ne!(app.selected, first);
        assert!(matches!(app.sidebar[app.selected], SidebarItem::Page(_)));
    }
}

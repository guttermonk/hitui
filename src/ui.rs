//! Rendering. Knows nothing about where data comes from — it draws whatever
//! tree of rows the selected collector produced.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState,
};

use crate::app::{self, App, Focus, Row, SidebarItem};
use crate::config::theme::Palette;
use crate::keys::{Action, Keymaps, Mode};
use crate::model::{Polarity, Value};
use crate::sysfs;

/// Wide enough for the longest page title ("Environment Variables") plus its
/// indent, ordinal and borders.
const SIDEBAR_WIDTH: u16 = 28;
/// Clamp for the label column so a single long key cannot squeeze out values.
const MAX_LABEL_WIDTH: usize = 38;

/// The palette in force for this frame.
///
/// Threaded through rather than read from a global, so a test can render a
/// frame under any theme.
fn accent(p: &Palette) -> Style {
    Style::default().fg(p.accent)
}

fn dim(p: &Palette) -> Style {
    Style::default().fg(p.dim)
}

fn heading(p: &Palette) -> Style {
    Style::default().fg(p.heading)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, &app.palette.clone());

    let [sidebar, detail] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
    draw_sidebar(frame, sidebar, app);
    draw_detail(frame, detail, app);

    draw_footer(frame, footer, app);

    if app.show_help {
        draw_help(frame, &app.palette.clone(), &app.keymaps);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, p: &Palette) {
    let mut spans = vec![
        Span::styled(" hitui ", accent(p).add_modifier(Modifier::BOLD)),
        Span::styled("· ", dim(p)),
    ];
    if let Some(host) = sysfs::read("/etc/hostname") {
        spans.push(Span::raw(host));
        spans.push(Span::styled(" · ", dim(p)));
    }
    if let Some(kernel) = sysfs::read("/proc/sys/kernel/osrelease") {
        spans.push(Span::styled(format!("Linux {kernel}"), dim(p)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.palette;
    let p = &palette;
    let focused = app.focus == Focus::Sidebar;

    // Pages are numbered across the whole sidebar, independent of grouping, so
    // the digit shortcuts match what is on screen.
    let mut ordinal = 0;
    let items: Vec<ListItem> = app
        .sidebar
        .iter()
        .map(|item| match item {
            SidebarItem::Header(group) => ListItem::new(Line::from(Span::styled(
                group.title(),
                heading(p).add_modifier(Modifier::BOLD),
            ))),
            SidebarItem::Page(index) => {
                let page = &app.pages[*index];
                ordinal += 1;
                let key = if ordinal <= 9 {
                    format!("{ordinal} ")
                } else {
                    "  ".to_string()
                };
                // Clip with an ellipsis rather than letting the pane cut a
                // word mid-character on a narrow terminal.
                let room = area.width.saturating_sub(6) as usize;
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(key, dim(p)),
                    Span::raw(ellipsise(page.title(), room)),
                ];
                if page.error.is_some() {
                    spans.push(Span::styled(" !", Style::default().fg(p.danger)));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    let list = List::new(items)
        .block(pane_block("Categories", focused, p))
        .highlight_style(
            Style::default()
                .fg(p.cursor_fg)
                .bg(p.cursor_bg)
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = app.palette;
    let p = &palette;
    let focused = app.focus == Focus::Detail;
    let title = {
        let page = app.page();
        if page.is_live() {
            format!("{} (live)", page.title())
        } else {
            page.title().to_string()
        }
    };
    let block = pane_block(&title, focused, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(error) = app.page().error.clone() {
        let text = Paragraph::new(vec![
            Line::from(Span::styled(
                "Could not collect this page",
                Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(Span::raw(error)),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(text, inner);
        return;
    }

    // Reserve the rightmost column for the scrollbar.
    let content_width = inner.width.saturating_sub(1) as usize;
    let rows = app::visible_rows(&app.page().rows, &app.filter);
    if rows.is_empty() {
        // Zero the geometry so scroll keys cannot run off stale line counts.
        let page = app.page_mut();
        page.total_lines = 0;
        page.scroll = 0;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("no field matches `{}`", app.filter),
                dim(p),
            ))),
            inner,
        );
        return;
    }
    let lines = build_lines(&rows, content_width, p);

    // Hand the geometry back to the app so key handling can page and clamp.
    let page = app.page_mut();
    page.total_lines = lines.len();
    page.viewport = inner.height as usize;
    page.scroll = page
        .scroll
        .min(page.total_lines.saturating_sub(page.viewport));
    let scroll = page.scroll;

    let visible: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .take(inner.height as usize)
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);

    let page = app.page();
    if page.total_lines > page.viewport {
        let mut state = ScrollbarState::new(page.total_lines.saturating_sub(page.viewport))
            .position(page.scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(dim(p)),
            // Drawn over the block's right border, inset so the corners survive.
            Rect {
                x: area.right().saturating_sub(1),
                y: area.y + 1,
                width: 1,
                height: area.height.saturating_sub(2),
            },
            &mut state,
        );
    }
}

/// Turn logical rows into rendered lines, wrapping long values into the value
/// column so that flag lists and mitigation strings stay readable.
fn build_lines(rows: &[&Row], width: usize, p: &Palette) -> Vec<Line<'static>> {
    let label_width = rows
        .iter()
        .filter(|r| !r.is_spacer() && r.value.is_some())
        .map(|r| r.depth * 2 + r.label.chars().count())
        .max()
        .unwrap_or(16)
        .clamp(12, MAX_LABEL_WIDTH);

    // +2 for the gap between columns.
    let value_width = width.saturating_sub(label_width + 2).max(8);
    let mut lines = Vec::new();

    for row in rows {
        if row.is_spacer() {
            lines.push(Line::raw(""));
            continue;
        }

        let indent = " ".repeat(row.depth * 2);

        if row.is_group {
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(row.label.clone(), accent(p).add_modifier(Modifier::BOLD)),
            ];
            // Name the tool a section's data came from, when it was not the
            // kernel, so the reader can judge the numbers.
            if let Some(source) = &row.source {
                spans.push(Span::styled(format!("  via {source}"), dim(p)));
            }
            lines.push(Line::from(spans));
            continue;
        }

        let Some(value) = &row.value else { continue };
        let key = format!("{indent}{}", row.label);
        let pad = label_width.saturating_sub(key.chars().count());
        let label_span = Span::styled(format!("{key}{} ", " ".repeat(pad)), dim(p));

        match value {
            Value::Gauge {
                percent,
                label,
                polarity,
            } => {
                let mut spans = vec![label_span];
                spans.extend(gauge_spans(
                    *percent,
                    label.as_deref(),
                    *polarity,
                    value_width,
                    p,
                ));
                lines.push(Line::from(spans));
            }
            _ => {
                for (i, chunk) in wrap(&value.to_string(), value_width).iter().enumerate() {
                    let label_span = if i == 0 {
                        label_span.clone()
                    } else {
                        // Continuation lines leave the label column blank.
                        Span::raw(" ".repeat(label_width + 1))
                    };
                    lines.push(Line::from(vec![label_span, Span::raw(chunk.clone())]));
                }
            }
        }
    }
    lines
}

/// A bar, its percentage, and an optional caption spelling out the absolutes.
///
/// Colour tracks severity so a nearly-full disk or a worn-out SSD is visible
/// without reading the number. Severity depends on which end is bad: 95 % disk
/// used is alarming, 95 % battery remaining is not.
fn gauge_spans(
    percent: f64,
    label: Option<&str>,
    polarity: Polarity,
    width: usize,
    p: &Palette,
) -> Vec<Span<'static>> {
    const MIN_BAR: usize = 8;
    const MAX_BAR: usize = 24;

    let percent = percent.clamp(0.0, 100.0);
    let caption = match label {
        Some(label) => format!(" {percent:>5.1} %  {label}"),
        None => format!(" {percent:>5.1} %"),
    };

    // Give the caption its space first; the bar takes what is left.
    let bar_width = width
        .saturating_sub(caption.chars().count())
        .clamp(MIN_BAR, MAX_BAR);
    let filled = (percent / 100.0 * bar_width as f64).round() as usize;

    // How far along the bad direction this reading sits.
    let severity = match polarity {
        Polarity::HighIsBad => percent,
        Polarity::HighIsGood => 100.0 - percent,
    };
    let style = Style::default().fg(if severity >= 90.0 {
        p.danger
    } else if severity >= 70.0 {
        p.warn
    } else {
        p.ok
    });

    vec![
        Span::styled("█".repeat(filled), style),
        Span::styled(
            "░".repeat(bar_width - filled),
            Style::default().fg(p.gauge_track),
        ),
        Span::raw(caption),
    ]
}

/// Shorten `text` to `width` columns, marking that it was cut.
fn ellipsise(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    text.chars().take(width - 1).chain(['…']).collect()
}

/// Greedy word wrap, hard-breaking any single word longer than `width`.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let mut word = word;
        // A word that cannot fit on its own line gets split across lines.
        while word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let split = word
                .char_indices()
                .nth(width)
                .map(|(i, _)| i)
                .unwrap_or(word.len());
            lines.push(word[..split].to_string());
            word = &word[split..];
        }
        if current.is_empty() {
            current = word.to_string();
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut current, word.to_string()));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.palette;
    let p = &palette;
    // While typing, the footer is the search prompt.
    if app.searching {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", accent(p).add_modifier(Modifier::BOLD)),
                Span::raw(app.filter.clone()),
                Span::styled("▏", accent(p)),
                Span::styled("   Enter keep · Esc clear", dim(p)),
            ])),
            area,
        );
        return;
    }

    if !app.filter.is_empty() {
        let matched = app::visible_rows(&app.page().rows, &app.filter)
            .iter()
            .filter(|r| !r.is_group)
            .count();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" filter ", accent(p).add_modifier(Modifier::BOLD)),
                Span::raw(app.filter.clone()),
                Span::styled(
                    if matched == 1 {
                        "   1 field".to_string()
                    } else {
                        format!("   {matched} fields")
                    },
                    dim(p),
                ),
                Span::styled("   Esc clear · / edit", dim(p)),
            ])),
            area,
        );
        return;
    }

    // Every hint below names the key actually bound, looked up rather than
    // written out — a rebound keymap would otherwise make the footer lie.
    let browser = app.keymaps.get(Mode::Browser);
    let hint = |action: Action| -> Option<String> {
        browser
            .combos_for(action)
            // The first binding is the primary one; listing all of them would
            // not fit on one line.
            .first()
            .map(|combo| combo.to_string())
    };

    // A page with an action advertises it ahead of the generic bindings:
    // nothing else on screen says that Enter does anything.
    if let Some(label) = app.page().action() {
        let mut spans = vec![Span::raw(" ")];
        for (action, text) in [
            (Action::Activate, label),
            (Action::MoveDown, "scroll"),
            (Action::FocusSidebar, "pages"),
            (Action::Help, "help"),
            (Action::Quit, "quit"),
        ] {
            if let Some(key) = hint(action) {
                spans.push(Span::styled(key, accent(p).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" {text}   "), dim(p)));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    // Movement is shown as a pair, since the two halves are always bound
    // together and "j/k" reads better than two separate hints.
    let movement = match (hint(Action::MoveDown), hint(Action::MoveUp)) {
        (Some(down), Some(up)) => Some(format!("{down}/{up}")),
        (one, other) => one.or(other),
    };
    let extremes = match (hint(Action::Top), hint(Action::Bottom)) {
        (Some(top), Some(bottom)) => Some(format!("{top}/{bottom}")),
        (one, other) => one.or(other),
    };

    let mut hints: Vec<(Option<String>, &str)> = vec![(
        movement,
        if app.focus == Focus::Sidebar {
            "page"
        } else {
            "scroll"
        },
    )];
    match app.focus {
        Focus::Sidebar => hints.push((hint(Action::FocusDetail), "details")),
        Focus::Detail => {
            hints.push((extremes, "top/bottom"));
            hints.push((hint(Action::FocusSidebar), "pages"));
        }
    }
    hints.extend([
        (hint(Action::Search), "search"),
        (hint(Action::Refresh), "refresh"),
        (hint(Action::Help), "help"),
        (hint(Action::Quit), "quit"),
    ]);

    let mut spans = vec![Span::raw(" ")];
    for (key, label) in hints {
        // An action nobody bound gets no hint, rather than a blank one.
        if let Some(key) = key {
            spans.push(Span::styled(key, accent(p).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!(" {label}   "), dim(p)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The key help, generated from the resolved keymap.
///
/// Listing the defaults here instead would be a lie the moment anybody
/// rebinds anything — which is the whole point of the config.
fn draw_help(frame: &mut Frame, p: &Palette, keymaps: &Keymaps) {
    let browser = keymaps.get(Mode::Browser);

    let mut rows: Vec<(String, &str)> = Vec::new();
    for action in Action::ALL {
        let combos = browser.combos_for(action);
        if combos.is_empty() {
            continue;
        }
        let keys: Vec<String> = combos.iter().map(|c| c.to_string()).collect();
        rows.push((keys.join(", "), action.describe()));
    }
    // Not an action, since which page each digit reaches depends on how many
    // pages there are — but it is still a binding worth documenting.
    rows.push(("1 – 9".to_string(), "jump to a page by number"));

    let width = rows
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(8);

    let mut lines = vec![Line::raw("")];
    for (key, action) in &rows {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{key:<width$}  "),
                accent(p).add_modifier(Modifier::BOLD),
            ),
            Span::raw(*action),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  any other key closes this",
        dim(p),
    )));

    // Sized to the content rather than fixed: the rows are generated from the
    // keymap, so their width depends on how many keys an action carries and
    // a fixed 54 columns truncated the descriptions.
    let longest = rows
        .iter()
        .map(|(key, action)| key.chars().count() + action.chars().count())
        .max()
        .unwrap_or(40);
    let height = lines.len() as u16 + 2;
    let area = centered(frame.area(), longest as u16 + 8, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("Keys", true, p)),
        area,
    );
}

fn pane_block(title: &str, focused: bool, p: &Palette) -> Block<'static> {
    let border = if focused { accent(p) } else { dim(p) };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(Span::styled(
            format!(" {title} "),
            if focused {
                accent(p).add_modifier(Modifier::BOLD)
            } else {
                dim(p)
            },
        )))
        .title_alignment(Alignment::Left)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::theme::Preset;
    use ratatui::style::Color;

    #[test]
    fn long_titles_are_marked_as_clipped() {
        assert_eq!(ellipsise("Summary", 20), "Summary");
        // The failure this fixes: "Environment Variab" read as a real name.
        assert_eq!(ellipsise("Environment Variables", 10), "Environme…");
        assert_eq!(
            ellipsise("Environment Variables", 21),
            "Environment Variables"
        );
        // Degenerate widths must not panic.
        assert_eq!(ellipsise("Summary", 1), "…");
        assert_eq!(ellipsise("Summary", 0), "");
    }

    #[test]
    fn every_page_title_fits_the_sidebar() {
        // The sidebar is sized for the longest title; if a new page exceeds
        // it, widen SIDEBAR_WIDTH rather than shipping a clipped label.
        let app = App::new();
        let room = SIDEBAR_WIDTH as usize - 6;
        for page in &app.pages {
            assert!(
                page.title().chars().count() <= room,
                "{:?} needs {} columns, sidebar allows {room}",
                page.title(),
                page.title().chars().count()
            );
        }
    }

    #[test]
    fn wraps_on_word_boundaries() {
        assert_eq!(wrap("short", 20), vec!["short"]);
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
    }

    #[test]
    fn hard_breaks_overlong_words() {
        assert_eq!(wrap("aaaaaaaa", 3), vec!["aaa", "aaa", "aa"]);
    }

    /// The colour of a gauge's filled portion.
    fn gauge_colour(percent: f64, polarity: Polarity) -> Option<Color> {
        gauge_spans(percent, None, polarity, 60, &Preset::Terminal.palette())
            .first()
            .and_then(|span| span.style.fg)
    }

    #[test]
    fn gauge_colour_follows_polarity() {
        // Consumption: a full bar is the alarm.
        assert_eq!(gauge_colour(5.0, Polarity::HighIsBad), Some(Color::Green));
        assert_eq!(gauge_colour(75.0, Polarity::HighIsBad), Some(Color::Yellow));
        assert_eq!(gauge_colour(95.0, Polarity::HighIsBad), Some(Color::Red));

        // Reserves read the other way round. This is the bug being guarded:
        // 100 % spare blocks and a full battery were being painted red.
        assert_eq!(
            gauge_colour(100.0, Polarity::HighIsGood),
            Some(Color::Green)
        );
        assert_eq!(
            gauge_colour(25.0, Polarity::HighIsGood),
            Some(Color::Yellow)
        );
        assert_eq!(gauge_colour(5.0, Polarity::HighIsGood), Some(Color::Red));
    }

    #[test]
    fn gauge_bar_never_overflows_its_column() {
        // 0 % and 100 % are the off-by-one candidates.
        for percent in [0.0, 0.4, 50.0, 99.6, 100.0] {
            for polarity in [Polarity::HighIsBad, Polarity::HighIsGood] {
                let spans = gauge_spans(
                    percent,
                    Some("caption"),
                    polarity,
                    48,
                    &Preset::Terminal.palette(),
                );
                let rendered: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                assert!(
                    rendered <= 48,
                    "{percent} % rendered {rendered} cells in 48"
                );
            }
        }
    }

    /// Render a frame off-screen. Run with `--nocapture` to eyeball the layout.
    fn render(width: u16, height: u16, app: &mut App) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        let text = rendered.join("\n");
        println!("{text}");
        text
    }

    /// The style of the first cell matching `needle`, for theme assertions.
    fn style_of(app: &mut App, needle: &str) -> Option<ratatui::style::Style> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(96, 40)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();

        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(column) = row.find(needle) {
                return Some(buffer[(column as u16, y)].style());
            }
        }
        None
    }

    #[test]
    fn a_theme_preset_reaches_the_rendered_frame() {
        // The whole point of the config: a preset must actually change what
        // is drawn, not merely parse.
        let config = crate::config::Config::from_str(
            r#"
            [theme]
            preset = "gruvbox-dark"
            "#,
        )
        .expect("valid config");

        let mut app = App::with_config(&config);
        app.page_mut().reload();

        let style = style_of(&mut app, "hitui").expect("the title is drawn");
        assert_eq!(
            style.fg,
            Some(Color::Rgb(0xfa, 0xbd, 0x2f)),
            "the title should use gruvbox's accent"
        );
    }

    #[test]
    fn a_colour_override_beats_the_preset_on_screen() {
        let config = crate::config::Config::from_str(
            r##"
            [theme]
            preset = "nord"
            [theme.colors]
            accent = "#ff0000"
            "##,
        )
        .expect("valid config");

        let mut app = App::with_config(&config);
        app.page_mut().reload();

        let style = style_of(&mut app, "hitui").expect("the title is drawn");
        assert_eq!(style.fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
    }

    #[test]
    fn the_default_theme_leaves_the_terminal_palette_alone() {
        let mut app = App::new();
        app.page_mut().reload();

        // Reset means "whatever the terminal uses", which is what keeps a
        // transparent background transparent.
        let palette = app.palette;
        assert_eq!(palette.bg, Color::Reset);
        assert_eq!(palette.fg, Color::Reset);
    }

    #[test]
    fn the_footer_names_the_keys_that_are_actually_bound() {
        // A rebound keymap must not leave the footer advertising j/k.
        let config = crate::config::Config::from_str(
            r#"
            [keys.browser]
            set = [["MoveDown", "e"], ["MoveUp", "i"], ["FocusDetail", "o"]]
            "#,
        )
        .expect("valid config");

        let mut app = App::with_config(&config);
        app.page_mut().reload();
        let text = render(96, 40, &mut app);

        let footer = text.lines().last().expect("a footer line");
        assert!(footer.contains("e/i"), "footer should show e/i: {footer}");
        assert!(!footer.contains("j/k"), "footer still shows j/k: {footer}");
    }

    #[test]
    fn the_help_overlay_lists_the_resolved_bindings() {
        let config = crate::config::Config::from_str(
            r#"
            [keys.browser]
            set = [["Quit", "x"]]
            "#,
        )
        .expect("valid config");

        let mut app = App::with_config(&config);
        app.page_mut().reload();
        app.show_help = true;
        let text = render(96, 40, &mut app);

        // The overlay is generated from the keymap, so it shows x for quit.
        assert!(text.contains("quit"), "the quit row should appear");
        let quit_row = text
            .lines()
            .find(|l| l.contains("quit") && !l.contains("Categories"))
            .expect("a quit row");
        assert!(quit_row.contains('x'), "{quit_row}");
    }

    #[test]
    fn draws_the_summary_inventory() {
        let mut app = App::new();
        assert_eq!(app.page().id(), "summary", "summary should open first");
        app.page_mut().reload();
        let text = render(96, 40, &mut app);

        for section in ["System", "Processor", "Memory"] {
            assert!(text.contains(section), "{section} section missing");
        }
        assert!(text.contains("Kernel"), "kernel field missing");
        // Memory in use is a gauge, so a bar should be drawn.
        assert!(text.contains('█') || text.contains('░'), "no gauge drawn");
    }

    #[test]
    fn draws_grouped_sidebar_and_detail() {
        let mut app = App::new();
        assert!(app.select_page_by_id("memory"), "memory page registered");
        app.page_mut().reload();
        let text = render(96, 32, &mut app);

        assert!(text.contains("hitui"), "header missing");
        assert!(text.contains("Computer"), "group heading missing");
        assert!(text.contains("Devices"), "group heading missing");
        assert!(text.contains("Memory Usage"), "page entry missing");
        assert!(text.contains("Overview"), "detail section missing");
    }

    #[test]
    fn wraps_long_values_in_the_value_column() {
        let mut app = App::new();
        // Jump to Processor, whose feature-flag list is far wider than a pane.
        assert!(app.select_page_by_id("cpu"), "cpu page registered");
        app.page_mut().reload();
        app.filter = "flags".to_string();
        let text = render(96, 32, &mut app);

        assert!(text.contains("Flags"), "flags field missing");
        // Wrapping means no rendered row runs past the pane; the renderer never
        // emits a line wider than the terminal, so check the flag list spilled
        // onto more than one line rather than being truncated to one.
        let flag_lines = text
            .lines()
            .filter(|l| l.contains("fpu") || l.contains("sse"))
            .count();
        assert!(flag_lines > 1, "expected the flag list to wrap");
    }

    #[test]
    fn filter_narrows_the_detail_pane() {
        let mut app = App::new();
        assert!(app.select_page_by_id("memory"), "memory page registered");
        app.page_mut().reload();
        app.filter = "swappiness".to_string();
        let text = render(96, 32, &mut app);

        assert!(text.contains("Swappiness"), "matching field missing");
        assert!(
            !text.contains("MemTotal"),
            "non-matching field should be filtered out"
        );
    }
}

//! Rendering. Knows nothing about where data comes from — it draws whatever
//! tree of rows the selected collector produced.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState,
};

use crate::app::{self, App, Focus, Row, SidebarItem};
use crate::model::{Polarity, Value};
use crate::sysfs;

/// Wide enough for the longest page title ("Environment Variables") plus its
/// indent, ordinal and borders.
const SIDEBAR_WIDTH: u16 = 28;
/// Clamp for the label column so a single long key cannot squeeze out values.
const MAX_LABEL_WIDTH: usize = 38;

fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header);

    let [sidebar, detail] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
    draw_sidebar(frame, sidebar, app);
    draw_detail(frame, detail, app);

    draw_footer(frame, footer, app);

    if app.show_help {
        draw_help(frame);
    }
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let mut spans = vec![
        Span::styled(" hitui ", accent().add_modifier(Modifier::BOLD)),
        Span::styled("· ", dim()),
    ];
    if let Some(host) = sysfs::read("/etc/hostname") {
        spans.push(Span::raw(host));
        spans.push(Span::styled(" · ", dim()));
    }
    if let Some(kernel) = sysfs::read("/proc/sys/kernel/osrelease") {
        spans.push(Span::styled(format!("Linux {kernel}"), dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
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
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
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
                    Span::styled(key, dim()),
                    Span::raw(ellipsise(page.title(), room)),
                ];
                if page.error.is_some() {
                    spans.push(Span::styled(" !", Style::default().fg(Color::Red)));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    let list = List::new(items)
        .block(pane_block("Categories", focused))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Detail;
    let title = {
        let page = app.page();
        if page.is_live() {
            format!("{} (live)", page.title())
        } else {
            page.title().to_string()
        }
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(error) = app.page().error.clone() {
        let text = Paragraph::new(vec![
            Line::from(Span::styled(
                "Could not collect this page",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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
                dim(),
            ))),
            inner,
        );
        return;
    }
    let lines = build_lines(&rows, content_width);

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
                .style(dim()),
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
fn build_lines(rows: &[&Row], width: usize) -> Vec<Line<'static>> {
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
                Span::styled(row.label.clone(), accent().add_modifier(Modifier::BOLD)),
            ];
            // Name the tool a section's data came from, when it was not the
            // kernel, so the reader can judge the numbers.
            if let Some(source) = &row.source {
                spans.push(Span::styled(format!("  via {source}"), dim()));
            }
            lines.push(Line::from(spans));
            continue;
        }

        let Some(value) = &row.value else { continue };
        let key = format!("{indent}{}", row.label);
        let pad = label_width.saturating_sub(key.chars().count());
        let label_span = Span::styled(format!("{key}{} ", " ".repeat(pad)), dim());

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
        Color::Red
    } else if severity >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    });

    vec![
        Span::styled("█".repeat(filled), style),
        Span::styled("░".repeat(bar_width - filled), dim()),
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
    // While typing, the footer is the search prompt.
    if app.searching {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", accent().add_modifier(Modifier::BOLD)),
                Span::raw(app.filter.clone()),
                Span::styled("▏", accent()),
                Span::styled("   Enter keep · Esc clear", dim()),
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
                Span::styled(" filter ", accent().add_modifier(Modifier::BOLD)),
                Span::raw(app.filter.clone()),
                Span::styled(
                    if matched == 1 {
                        "   1 field".to_string()
                    } else {
                        format!("   {matched} fields")
                    },
                    dim(),
                ),
                Span::styled("   Esc clear · / edit", dim()),
            ])),
            area,
        );
        return;
    }

    let keys: &[(&str, &str)] = match app.focus {
        Focus::Sidebar => &[
            ("j/k", "page"),
            ("l", "details"),
            ("/", "search"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Focus::Detail => &[
            ("j/k", "scroll"),
            ("g/G", "top/bottom"),
            ("h", "pages"),
            ("/", "search"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ],
    };

    let mut spans = vec![Span::raw(" ")];
    for (key, action) in keys {
        spans.push(Span::styled(*key, accent().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {action}   "), dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(frame: &mut Frame) {
    let rows = [
        ("j / k, ↓ / ↑", "move or scroll"),
        ("h / l, ← / →", "switch pane"),
        ("Tab", "toggle pane focus"),
        ("1 – 9", "jump to page"),
        ("/", "filter fields on this page"),
        ("g / G, Home / End", "top / bottom"),
        ("PgUp / PgDn", "scroll a screen"),
        ("r", "re-collect this page"),
        ("?", "this help"),
        ("q, Esc, Ctrl-C", "quit"),
    ];

    let mut lines = vec![Line::raw("")];
    for (key, action) in rows {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{key:<20}"), accent().add_modifier(Modifier::BOLD)),
            Span::raw(action),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  any other key closes this",
        dim(),
    )));

    let height = lines.len() as u16 + 2;
    let area = centered(frame.area(), 54, height);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(pane_block("Keys", true)), area);
}

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let border = if focused { accent() } else { dim() };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(Span::styled(
            format!(" {title} "),
            if focused {
                accent().add_modifier(Modifier::BOLD)
            } else {
                dim()
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
        gauge_spans(percent, None, polarity, 60)
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
                let spans = gauge_spans(percent, Some("caption"), polarity, 48);
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

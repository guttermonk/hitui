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
use crate::config::theme::Palette;
use crate::keys::{Action, Keymaps, Mode};
use crate::model::{Polarity, Severity, Value};
use crate::sysfs;

/// Wide enough for the longest page title ("Environment Variables") plus its
/// indent, ordinal and borders.
const SIDEBAR_WIDTH: u16 = 28;
/// Clamp for the label column so a single long key cannot squeeze out values.
const MAX_LABEL_WIDTH: usize = 38;
/// Most columns the detail pane will split a page into. Past four, the label
/// column costs more than the extra column buys on any real terminal.
const MAX_COLUMNS: usize = 4;
/// Blank cells between adjacent columns.
const GUTTER: usize = 3;
/// Narrowest column worth having: below this a label and its value cannot both
/// fit, and every value would wrap.
const MIN_COLUMN: usize = 26;
/// How much shorter an extra column has to make the page to be worth taking.
/// Equal height in narrower columns is a loss, not a draw.
const COLUMN_GAIN: f64 = 0.85;
/// Shortest bar still worth drawing.
const MIN_BAR: usize = 8;
/// Longest bar; past this the bar reads no more precisely and crowds out the
/// caption.
const MAX_BAR: usize = 24;

/// The palette in force for this frame.
///
/// Threaded through rather than read from a global, so a test can render a
/// frame under any theme.
fn accent(p: &Palette) -> Style {
    Style::default().fg(p.accent)
}

fn dim(p: &Palette) -> Style {
    quiet(p.dim, p.faint.dim)
}

/// A colour, subordinated to the terminal's own foreground when the palette
/// asks for the dim attribute instead of a colour of its own. See
/// [`crate::config::theme::Faint`] for why the default preset does.
fn quiet(color: Color, faint: bool) -> Style {
    let style = Style::default().fg(color);
    if faint {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    }
}

/// A pane's border. The focused one names a colour; the others defer, since
/// their job is to be present without being read.
fn border(p: &Palette, focused: bool) -> Style {
    if focused {
        Style::default().fg(p.border_focused)
    } else {
        quiet(p.border, p.faint.border)
    }
}

/// The unfilled part of a gauge bar.
fn gauge_track(p: &Palette) -> Style {
    quiet(p.gauge_track, p.faint.gauge_track)
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
    let lines = columnise(
        &rows,
        content_width,
        inner.height as usize,
        app.max_columns,
        p,
    );

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

/// Width of the label column: the longest label on the page, bounded so that
/// one long key cannot squeeze out the values, and so that a narrow column
/// still leaves room for a value at all.
fn label_width(rows: &[&Row], width: usize) -> usize {
    let cap = MAX_LABEL_WIDTH.min(width.saturating_sub(10)).max(12);
    rows.iter()
        .filter(|r| !r.is_spacer() && r.value.is_some())
        .map(|r| r.depth * 2 + r.label.chars().count())
        .max()
        .unwrap_or(16)
        .clamp(12, cap)
}

/// A page rendered at one candidate column width, with what the packer needs
/// to break it into columns without orphaning a field from its heading.
#[derive(Default)]
struct Flow {
    lines: Vec<Line<'static>>,
    /// Indices of lines that begin a top-level section — the places where a
    /// column break costs nothing at all.
    starts: Vec<usize>,
    /// Indices of lines that begin a group of any depth. Second-choice break
    /// points: breaking between two USB devices is far better than breaking
    /// through the middle of one.
    group_starts: Vec<usize>,
    /// The chain of group headings enclosing each line, outermost first, so a
    /// column that has to begin inside a group can name every group its first
    /// field belongs to.
    owner: Vec<Vec<usize>>,
    /// Group heading lines, indexed by `owner`.
    headings: Vec<Line<'static>>,
}

impl Flow {
    fn push(&mut self, line: Line<'static>, owner: &[usize]) {
        self.lines.push(line);
        self.owner.push(owner.to_vec());
    }
}

/// Render `rows` into a single column `width` cells wide, wrapping long values
/// into the value column so flag lists and mitigation strings stay readable.
fn flow_page(rows: &[&Row], width: usize, p: &Palette) -> Flow {
    let label_width = label_width(rows, width);
    // +2 for the gap between the label and value columns.
    let value_width = width.saturating_sub(label_width + 2).max(8);

    let mut flow = Flow::default();
    // Headings enclosing the walk's current position, one per depth.
    let mut chain: Vec<usize> = Vec::new();

    for row in rows {
        // A row's enclosing groups are exactly the ones shallower than it.
        chain.truncate(row.depth);

        if row.is_spacer() {
            flow.push(Line::raw(""), &chain);
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
            let line = Line::from(spans);
            if row.depth == 0 {
                flow.starts.push(flow.lines.len());
            }
            flow.group_starts.push(flow.lines.len());
            flow.headings.push(line.clone());
            let heading = flow.headings.len() - 1;
            // Pushed with the chain that encloses it, then added to the chain
            // so its own children name it.
            flow.push(line, &chain);
            chain.push(heading);
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
                flow.push(Line::from(spans), &chain);
            }
            // The value in the colour its verdict earned, and the advice
            // under it rather than beside it: a recommendation is a sentence,
            // and putting it on the same line either pushes the value off the
            // left of the reader's attention or wraps mid-advice.
            Value::Verdict {
                value,
                severity,
                advice,
            } => {
                let colour = match severity {
                    Severity::Warn => p.warn,
                    Severity::Danger => p.danger,
                };
                for (i, chunk) in wrap(value, value_width).iter().enumerate() {
                    let label_span = if i == 0 {
                        label_span.clone()
                    } else {
                        Span::raw(" ".repeat(label_width + 1))
                    };
                    flow.push(
                        Line::from(vec![
                            label_span,
                            Span::styled(chunk.clone(), Style::default().fg(colour)),
                        ]),
                        &chain,
                    );
                }
                if let Some(advice) = advice {
                    for chunk in wrap(&format!("→ {advice}"), value_width) {
                        flow.push(
                            Line::from(vec![
                                Span::raw(" ".repeat(label_width + 1)),
                                Span::styled(chunk, dim(p)),
                            ]),
                            &chain,
                        );
                    }
                }
            }
            _ => {
                for (i, chunk) in wrap(&value.to_string(), value_width).iter().enumerate() {
                    let label_span = if i == 0 {
                        label_span.clone()
                    } else {
                        // Continuation lines leave the label column blank.
                        Span::raw(" ".repeat(label_width + 1))
                    };
                    flow.push(
                        Line::from(vec![label_span, Span::raw(chunk.clone())]),
                        &chain,
                    );
                }
            }
        }
    }
    flow
}

/// Lay a page out across however many columns make it shortest.
///
/// Most of these pages are a narrow list of short values — memory counters,
/// sensor readings, socket statistics — and in one column they run off the
/// bottom of the screen while two thirds of the pane sits empty.
///
/// Two things decide the split, and both are measured rather than configured
/// per page. A page is only split if it does not fit the screen in one column:
/// columns are here to stop a page overflowing, and cutting up a page that
/// already fits only makes it harder to read. Then each candidate column count
/// is laid out and the resulting heights compared, which is what keeps pages
/// of long values (CPU flag lists, environment variables) in one wide column —
/// wrapping those into narrow columns makes the page *taller*, so they never
/// clear the bar.
fn columnise(
    rows: &[&Row],
    width: usize,
    viewport: usize,
    max_columns: usize,
    p: &Palette,
) -> Vec<Line<'static>> {
    let flow = flow_page(rows, width, p);
    // Clipped even in one column: a section heading is one long line that
    // nothing wraps, and the pane edge would otherwise cut it off silently.
    let mut best: Vec<Line<'static>> = flow
        .lines
        .iter()
        .map(|line| Line::from(clip(line, width)))
        .collect();
    let mut best_height = best.len();
    if best_height <= viewport {
        return best;
    }
    // A gauge cannot wrap, so it sets a hard floor on the column width.
    let floor = MIN_COLUMN.max(gauge_floor(rows, width));

    for columns in 2..=max_columns.min(MAX_COLUMNS) {
        let gutters = (columns - 1) * GUTTER;
        let column_width = width.saturating_sub(gutters) / columns;
        if column_width < floor {
            break;
        }

        let flow = flow_page(rows, column_width, p);
        let packed = cut(&flow, columns);
        let height = packed.iter().map(Vec::len).max().unwrap_or(0);

        // An extra column has to earn its keep. Equal height in narrower
        // columns is a loss, and a marginal gain is not worth values that wrap
        // where they used to fit.
        if height as f64 > best_height as f64 * COLUMN_GAIN {
            break;
        }
        best_height = height;
        best = zip_columns(&packed, column_width);
    }
    best
}

/// Break a flowed page into `columns` of roughly equal height, preferring to
/// break between sections.
fn cut(flow: &Flow, columns: usize) -> Vec<Vec<Line<'static>>> {
    let target = flow.lines.len().div_ceil(columns);
    // How far from the ideal break a section boundary may be and still be
    // worth snapping to. A third of a column keeps the columns visibly even.
    let slack = (target / 3).max(2);

    let mut bounds = vec![0usize];
    for column in 1..columns {
        let previous = *bounds.last().expect("bounds starts non-empty");
        let ideal = (column * target).max(previous);
        // A section boundary first; failing that, any group boundary; failing
        // that, wherever the arithmetic lands.
        let nearest = |candidates: &[usize]| {
            candidates
                .iter()
                .copied()
                .filter(|&start| start > previous && start.abs_diff(ideal) <= slack)
                .min_by_key(|&start| start.abs_diff(ideal))
        };
        let at = nearest(&flow.starts)
            .or_else(|| nearest(&flow.group_starts))
            .unwrap_or(ideal)
            .min(flow.lines.len());
        bounds.push(at);
    }
    bounds.push(flow.lines.len());

    let mut packed = Vec::with_capacity(columns);
    for pair in bounds.windows(2) {
        let (mut from, to) = (pair[0], pair[1]);
        // A spacer that happened to land on a break would otherwise push the
        // column's first row down a line.
        while from < to && is_blank(&flow.lines[from]) {
            from += 1;
        }

        let mut column = Vec::new();
        // A column that does not start a whole section repeats every heading
        // above its first row. One heading is not enough: a bare "Interfaces"
        // belongs to a USB device whose name was left in the previous column,
        // and "Releases ahead 2" to a firmware device in the same position.
        if from > 0 && from < to && !flow.starts.contains(&from) {
            let chain = flow.owner.get(from).cloned().unwrap_or_default();
            for (depth, heading) in chain.iter().enumerate() {
                let line = &flow.headings[*heading];
                // Only the innermost one is interrupted; the outer ones are
                // simply context.
                if depth + 1 == chain.len() {
                    column.push(continued(line));
                } else {
                    column.push(line.clone());
                }
            }
        }
        column.extend(flow.lines[from..to].iter().cloned());
        while column.last().is_some_and(is_blank) {
            column.pop();
        }
        packed.push(column);
    }
    packed
}

/// A section heading, marked as carrying on from the previous column.
fn continued(heading: &Line<'static>) -> Line<'static> {
    let mut spans = heading.spans.clone();
    let style = spans.last().map(|s| s.style).unwrap_or_default();
    spans.push(Span::styled(
        " (cont.)",
        style.remove_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

/// Clip a line to `width` cells, marking the cut with an ellipsis.
///
/// A column has a neighbour to its right, so a line that overran would be
/// drawing in someone else's space. Silent truncation is not an option either:
/// a cut-off device name reads as a real, shorter name.
fn clip(line: &Line<'static>, width: usize) -> Vec<Span<'static>> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return line.spans.clone();
    }

    let mut out = Vec::new();
    let mut used = 0;
    for span in &line.spans {
        let room = width - used;
        if room == 0 {
            break;
        }
        let count = span.content.chars().count();
        if count < room {
            out.push(span.clone());
            used += count;
            continue;
        }
        // Marked even when this span fits exactly, because the line does not:
        // there is more of it after this span.
        let text: String = span
            .content
            .chars()
            .take(room.saturating_sub(1))
            .chain(['…'])
            .collect();
        out.push(Span::styled(text, span.style));
        break;
    }
    out
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

/// Lay columns side by side, padding each to its width so the next one starts
/// where it should, and clipping anything that would bleed into its neighbour.
fn zip_columns(packed: &[Vec<Line<'static>>], column_width: usize) -> Vec<Line<'static>> {
    let height = packed.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|y| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (i, column) in packed.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" ".repeat(GUTTER)));
                }
                let mut used = 0;
                if let Some(line) = column.get(y) {
                    for span in clip(line, column_width) {
                        used += span.content.chars().count();
                        spans.push(span);
                    }
                }
                // The last column needs no padding, and padding it would paint
                // the pane's full width on every line.
                if i + 1 < packed.len() {
                    spans.push(Span::raw(" ".repeat(column_width - used)));
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// The narrowest column this page's gauges can survive.
///
/// A gauge is a bar plus a caption spelling out the absolutes. Unlike text it
/// cannot wrap, so a column too narrow for one does not reflow — it cuts the
/// numbers off.
fn gauge_floor(rows: &[&Row], width: usize) -> usize {
    let label_width = label_width(rows, width);
    rows.iter()
        .filter_map(|row| match row.value.as_ref() {
            Some(Value::Gauge { percent, label, .. }) => Some(
                label_width
                    + 2
                    + MIN_BAR
                    + gauge_caption(*percent, label.as_deref()).chars().count(),
            ),
            _ => None,
        })
        .max()
        .unwrap_or(0)
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
    let percent = percent.clamp(0.0, 100.0);
    let caption = gauge_caption(percent, label);

    // Give the caption its space first; the bar takes what is left.
    let bar_width = width
        .saturating_sub(caption.chars().count())
        .clamp(MIN_BAR, MAX_BAR)
        // A column too narrow for even the shortest bar loses the bar rather
        // than overflowing into whatever is drawn to its right.
        .min(width);
    let caption = ellipsise(&caption, width.saturating_sub(bar_width));
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
        Span::styled("░".repeat(bar_width - filled), gauge_track(p)),
        Span::raw(caption),
    ]
}

/// The text beside a gauge's bar: the percentage, and the absolutes behind it
/// where the collector supplied them.
fn gauge_caption(percent: f64, label: Option<&str>) -> String {
    match label {
        Some(label) => format!(" {percent:>5.1} %  {label}"),
        None => format!(" {percent:>5.1} %"),
    }
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
    Block::bordered()
        .border_type(BorderType::Rounded)
        // The `border` and `border_focused` slots, which until now were
        // documented, settable, printed by --theme, and drawn by nothing: this
        // reached for `accent` and `dim` instead, so two of the fifteen slots
        // silently did nothing.
        .border_style(border(p, focused))
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
        style_of_from(app, needle, 0)
    }

    /// Style of the first cell where `needle` begins, searching only at or
    /// after column `from`.
    ///
    /// The bound is not optional for anything looking at page content: the
    /// sidebar lists page titles, so "Kernel" matches "Kernel Modules" there —
    /// drawn with `Span::raw`, hence `Reset` — several rows before it reaches
    /// the Summary field of the same name. Passed 0 for the header, which is
    /// drawn to the left of everything.
    fn style_of_from(app: &mut App, needle: &str, from: u16) -> Option<ratatui::style::Style> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(96, 40)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();

        for y in 0..buffer.area.height {
            let row: String = (from..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(column) = column_of(&row, needle) {
                return Some(buffer[(from + column as u16, y)].style());
            }
        }
        None
    }

    /// Where `needle` starts in a rendered row, counted in cells.
    ///
    /// `str::find` returns a byte offset, and a frame is full of box-drawing
    /// characters that are three bytes each — so a byte offset into a row that
    /// crosses a pane border is not a column, and indexing the buffer with one
    /// reads a cell several places to the right of the match.
    fn column_of(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|byte| row[..byte].chars().count())
    }

    #[test]
    fn a_rendered_row_is_searched_by_cell_not_by_byte() {
        // Three box-drawing characters: three cells, nine bytes.
        assert_eq!(column_of("│─│Kernel", "Kernel"), Some(3));
        assert_eq!(column_of("Kernel", "Kernel"), Some(0));
        assert_eq!(column_of("│─│", "Kernel"), None);
    }

    #[test]
    fn the_quiet_slots_are_drawn_faint_under_the_default_theme() {
        // The fix for the default preset being unreadable on a light terminal:
        // the quiet roles ask the terminal to subordinate its own foreground
        // rather than naming a grey that only works on a dark background.
        let config = crate::config::Config::from_str("").expect("the defaults are valid");
        let mut app = App::with_config(&config);
        app.page_mut().reload();

        let style =
            style_of_from(&mut app, "Kernel", SIDEBAR_WIDTH).expect("a field label is drawn");
        assert_eq!(
            style.fg,
            Some(Color::Reset),
            "a field label should keep the terminal's foreground"
        );
        assert!(
            style.add_modifier.contains(Modifier::DIM),
            "a field label should be drawn faint, got {:?}",
            style.add_modifier
        );
    }

    #[test]
    fn an_explicit_quiet_colour_is_drawn_as_chosen_not_faded() {
        let config = crate::config::Config::from_str(
            r##"
            [theme.colors]
            dim = "#ff0000"
            "##,
        )
        .expect("valid config");
        let mut app = App::with_config(&config);
        app.page_mut().reload();

        let style =
            style_of_from(&mut app, "Kernel", SIDEBAR_WIDTH).expect("a field label is drawn");
        assert_eq!(style.fg, Some(Color::Rgb(0xff, 0, 0)));
        assert!(
            !style.add_modifier.contains(Modifier::DIM),
            "a colour chosen explicitly should not also be faded"
        );
    }

    #[test]
    fn the_border_slots_are_the_ones_that_draw_the_borders() {
        // These two slots were documented, settable and printed by --theme
        // while being drawn by nothing: pane_block reached for `accent` and
        // `dim` instead, so setting them did nothing at all.
        let config = crate::config::Config::from_str(
            r##"
            [theme.colors]
            border = "#00ff00"
            border_focused = "#0000ff"
            accent = "#ff0000"
            "##,
        )
        .expect("valid config");
        let mut app = App::with_config(&config);
        app.page_mut().reload();

        // The sidebar has focus on startup, so its border is the focused one
        // and the detail pane's is not.
        assert!(matches!(app.focus, Focus::Sidebar));
        let (sidebar, detail) = border_colours(&mut app);
        assert_eq!(sidebar, Some(Color::Rgb(0, 0, 0xff)), "focused border");
        assert_eq!(detail, Some(Color::Rgb(0, 0xff, 0)), "unfocused border");

        // And the other way round once focus moves, so neither is hardcoded.
        app.focus = Focus::Detail;
        let (sidebar, detail) = border_colours(&mut app);
        assert_eq!(sidebar, Some(Color::Rgb(0, 0xff, 0)), "unfocused border");
        assert_eq!(detail, Some(Color::Rgb(0, 0, 0xff)), "focused border");
    }

    /// The colour of each pane's top border, taken from the horizontal run
    /// rather than a corner so it cannot be confused with the title.
    fn border_colours(app: &mut App) -> (Option<Color>, Option<Color>) {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(96, 40)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();

        // Row 1 is the top edge of both panes: the header occupies row 0.
        let colour_at = |x: u16| buffer[(x, 1u16)].style().fg;
        // Inside the sidebar's title run, and inside the detail pane's.
        (
            colour_at(SIDEBAR_WIDTH - 2),
            colour_at(buffer.area.width - 3),
        )
    }

    #[test]
    fn a_flagged_value_is_drawn_in_its_severity_colour() {
        // The whole point of the feature: a poorly configured setting has to
        // be visible without reading every row.
        let palette = Preset::GruvboxDark.palette();
        let rows = [
            Row {
                depth: 0,
                label: "Hardening".to_string(),
                value: None,
                source: None,
                is_group: true,
            },
            Row {
                depth: 1,
                label: "Weak".to_string(),
                value: Some(Value::warn("0 — readable by everyone", "set it to 1")),
                source: None,
                is_group: false,
            },
            Row {
                depth: 1,
                label: "Open".to_string(),
                value: Some(Value::danger("0 — permitted", "set it to 2")),
                source: None,
                is_group: false,
            },
            Row {
                depth: 1,
                label: "Fine".to_string(),
                value: Some(Value::Text("2 — disabled".to_string())),
                source: None,
                is_group: false,
            },
        ];
        let rows: Vec<&Row> = rows.iter().collect();
        let lines = flow_page(&rows, 70, &palette).lines;

        let colour_of = |needle: &str| -> Option<Color> {
            lines.iter().find_map(|line| {
                line.spans
                    .iter()
                    .find(|span| span.content.contains(needle))
                    .and_then(|span| span.style.fg)
            })
        };

        assert_eq!(colour_of("readable by everyone"), Some(palette.warn));
        assert_eq!(colour_of("0 — permitted"), Some(palette.danger));
        // A value that is already right carries no colour at all — not even
        // the foreground, since it is drawn unstyled and inherits the pane.
        // That is what makes colour on this page always mean "act on this".
        assert_eq!(colour_of("2 — disabled"), None);

        // The advice is drawn under the value, dim, and marked as advice
        // rather than running on as if it were part of the reading.
        let advice = lines
            .iter()
            .find(|line| line.to_string().contains("set it to 1"))
            .expect("the recommendation is drawn");
        assert!(advice.to_string().contains('→'), "{}", advice.to_string());
        assert_eq!(
            advice.spans.last().and_then(|span| span.style.fg),
            Some(palette.dim)
        );
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
    /// A heading row, or a labelled value row when `value` is given.
    fn row(depth: usize, label: &str, value: Option<&str>) -> Row {
        Row {
            depth,
            label: label.to_string(),
            value: value.map(|v| Value::Text(v.to_string())),
            source: None,
            is_group: value.is_none(),
        }
    }

    /// A page of `sections` sections, each `fields` fields of `value` deep.
    fn page(sections: usize, fields: usize, value: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        for s in 0..sections {
            if s > 0 {
                rows.push(row(0, "", None));
                // A spacer is a blank row, not a group.
                rows.last_mut().unwrap().is_group = false;
            }
            rows.push(row(0, &format!("Section {s}"), None));
            for f in 0..fields {
                rows.push(row(1, &format!("Field {f}"), Some(value)));
            }
        }
        rows
    }

    fn width_of(line: &Line<'_>) -> usize {
        line.spans.iter().map(|s| s.content.chars().count()).sum()
    }

    /// Lines of a page's real collected rows, in one column and as laid out.
    fn both(id: &str, width: usize, viewport: usize) -> (usize, usize) {
        let p = &Preset::Terminal.palette();
        let mut app = App::new();
        app.select_page_by_id(id);
        app.page_mut().reload();
        let rows = app::visible_rows(&app.page().rows, "");
        (
            flow_page(&rows, width, p).lines.len(),
            columnise(&rows, width, viewport, MAX_COLUMNS, p).len(),
        )
    }

    #[test]
    fn a_page_that_overflows_is_split_into_columns() {
        // The complaint this fixes: Memory Usage is 90 lines of 30-character
        // rows, so in one column it scrolls while two thirds of the pane is
        // blank.
        let (single, columns) = both("memory", 139, 44);
        assert!(single > 44, "memory should overflow a 44-line viewport");
        assert!(
            columns < single / 2,
            "memory: {single} lines in one column, {columns} in several"
        );
    }

    #[test]
    fn a_page_that_already_fits_is_left_alone() {
        // Columns exist to stop a page running off the bottom. Cutting up a
        // page that already fits buys nothing and costs readability.
        let p = &Preset::Terminal.palette();
        let rows = page(3, 4, "value");
        let rows: Vec<&Row> = rows.iter().collect();
        let single = flow_page(&rows, 139, p).lines;
        let laid_out = columnise(&rows, 139, 44, MAX_COLUMNS, p);
        assert_eq!(single.len(), laid_out.len());
    }

    #[test]
    fn long_values_keep_their_page_in_one_column() {
        // Environment Variables and the CPU flag list are the real cases:
        // narrow columns make them wrap, and wrapping makes them *taller*.
        let p = &Preset::Terminal.palette();
        let rows = page(4, 20, &"word ".repeat(40));
        let rows: Vec<&Row> = rows.iter().collect();
        let single = flow_page(&rows, 139, p).lines.len();
        let laid_out = columnise(&rows, 139, 20, MAX_COLUMNS, p).len();
        assert_eq!(
            single, laid_out,
            "a page of long values should stay in one column"
        );
    }

    #[test]
    fn a_narrow_terminal_gets_one_column() {
        let p = &Preset::Terminal.palette();
        let rows = page(6, 6, "42");
        let rows: Vec<&Row> = rows.iter().collect();
        let single = flow_page(&rows, 44, p).lines.len();
        assert_eq!(single, columnise(&rows, 44, 10, MAX_COLUMNS, p).len());
    }

    #[test]
    fn one_column_can_be_forced() {
        let p = &Preset::Terminal.palette();
        let rows = page(6, 6, "42");
        let rows: Vec<&Row> = rows.iter().collect();
        let single = flow_page(&rows, 139, p).lines.len();
        assert_eq!(single, columnise(&rows, 139, 10, 1, p).len());
        // And without the ceiling, this page would have been split.
        assert!(columnise(&rows, 139, 10, 4, p).len() < single);
    }

    #[test]
    fn no_column_bleeds_into_its_neighbour() {
        // The invariant that keeps the layout from corrupting itself: columns
        // are drawn as one line each, so an overlong line would overwrite the
        // column to its right.
        let p = &Preset::Terminal.palette();
        let mut app = App::new();
        for i in 0..app.pages.len() {
            app.pages[i].reload();
            let id = app.pages[i].id();
            let rows = app::visible_rows(&app.pages[i].rows, "");
            if rows.is_empty() {
                continue;
            }
            for width in [40usize, 80, 139, 200] {
                for viewport in [10usize, 44] {
                    for line in columnise(&rows, width, viewport, MAX_COLUMNS, p) {
                        assert!(
                            width_of(&line) <= width,
                            "{id} at {width}x{viewport}: a line is {} cells wide",
                            width_of(&line)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_column_that_breaks_mid_section_repeats_the_heading() {
        // /proc/meminfo is one 58-row section; split across columns, the
        // second column would otherwise be an anonymous block of numbers.
        let p = &Preset::Terminal.palette();
        let rows = page(1, 120, "42");
        let rows: Vec<&Row> = rows.iter().collect();
        let lines = columnise(&rows, 139, 30, MAX_COLUMNS, p);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.contains("Section 0 (cont.)"),
            "a continued section should name itself"
        );
    }

    #[test]
    fn a_break_prefers_a_section_boundary() {
        // Sections of equal size should break between sections, not inside
        // them — no "(cont.)" should be needed at all.
        let p = &Preset::Terminal.palette();
        let rows = page(4, 14, "42");
        let rows: Vec<&Row> = rows.iter().collect();
        let lines = columnise(&rows, 139, 30, MAX_COLUMNS, p);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(!text.contains("(cont.)"), "{text}");
        // Each section heading still appears exactly once.
        for s in 0..4 {
            assert_eq!(
                text.matches(&format!("Section {s}")).count(),
                1,
                "Section {s} should appear once"
            );
        }
    }

    #[test]
    fn clipping_a_line_marks_the_cut() {
        // Silent truncation is the bug: "Intel Corp. Bluetooth wireless inter"
        // reads as a device name, not as a cut-off one.
        let line = Line::from(vec![Span::raw("abc"), Span::raw("defgh")]);
        assert_eq!(clip(&line, 8).len(), 2, "an exact fit is not clipped");
        let clipped: String = clip(&line, 6).iter().map(|s| s.content.clone()).collect();
        assert_eq!(clipped, "abcde…");
        // A span that fills the width exactly is still marked when more
        // follows it.
        let clipped: String = clip(&line, 3).iter().map(|s| s.content.clone()).collect();
        assert_eq!(clipped, "ab…");
        assert!(clip(&line, 0).is_empty());
    }

    #[test]
    fn a_gauge_never_outgrows_its_column() {
        // Gauges cannot wrap, so a column too narrow for one would cut the
        // numbers off rather than reflow them.
        for width in [8usize, 12, 20, 31, 60] {
            let spans = gauge_spans(
                62.5,
                Some("7.74 GiB of 31.07 GiB"),
                Polarity::HighIsBad,
                width,
                &Preset::Terminal.palette(),
            );
            let rendered: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(rendered <= width, "{rendered} cells drawn in {width}");
        }
    }

    #[test]
    fn a_bar_is_never_drawn_without_its_number() {
        // Filesystems and Battery are the pages with gauges, and a gauge
        // cannot wrap: if its column is too narrow the caption is what gets
        // cut, leaving a bar with no figure beside it.
        let p = &Preset::Terminal.palette();
        let mut rows = Vec::new();
        for s in 0..6 {
            if s > 0 {
                let mut spacer = row(0, "", None);
                spacer.is_group = false;
                rows.push(spacer);
            }
            rows.push(row(0, &format!("/dev/sd{s}"), None));
            for f in 0..5 {
                rows.push(Row {
                    depth: 1,
                    label: format!("Usage {f}"),
                    value: Some(Value::Gauge {
                        percent: 62.5,
                        label: Some("7.74 GiB of 31.07 GiB".to_string()),
                        polarity: Polarity::HighIsBad,
                    }),
                    source: None,
                    is_group: false,
                });
            }
        }
        let rows: Vec<&Row> = rows.iter().collect();

        for width in [40usize, 80, 139, 200] {
            for line in columnise(&rows, width, 12, MAX_COLUMNS, p) {
                let text = line.to_string();
                if text.contains('█') || text.contains('░') {
                    assert!(
                        text.contains('%'),
                        "at width {width}: a bar lost its number: {text:?}"
                    );
                }
            }
        }
    }
}

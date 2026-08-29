//! Terminal rendering of presentation-owned rows; no diff structure is inferred here.

use crate::diff::{
    DiffMark, LineEnding, PresentedFile, ReviewRow, SourceRow, SyntaxClass, WordDiff,
};
use crate::review::{FileNotice, ReviewItem};
use anyhow::{Context, Result, ensure};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
};
use std::io::{self, Stdout};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Practical source viewport; gutter geometry is measured separately from content.
/// Smallest useful source viewport after the file-derived gutter.
const MIN_SOURCE_COLUMNS: usize = 62;
/// Path header plus its breathing row.
const HEADER_ROWS: u16 = 2;
/// Smallest viewport that still makes scrolling useful.
const MIN_BODY_ROWS: u16 = 16;
const MIN_TERMINAL_HEIGHT: u16 = HEADER_ROWS + MIN_BODY_ROWS;
const VERTICAL_ELLIPSIS: &str = "⋮";
const RIBBON_MARGIN: &str = "  ";
const RIBBON_SEPARATOR: &str = " │ ";
const RIBBON_OMISSION: &str = "…";
const GENERATED_BADGE: &str = " @generated";
/// Stable source-local tab geometry, independent of gutter width.
const SOURCE_TAB_STOP: usize = 4;

/// File-wide prefix geometry shared by every unified row style.
#[derive(Clone, Copy, Debug, Default)]
struct GutterLayout {
    label_columns: usize,
}

/// Minimal review facts needed to lay out the file ribbon.
#[derive(Clone, Copy, Debug)]
struct RibbonItem<'a> {
    path: &'a str,
    generated: bool,
}

/// Source-row marker whose glyph and foreground must remain one semantic choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceMarker {
    Reflow,
    Removed,
    Added,
}

impl SourceMarker {
    const fn glyph(self) -> char {
        match self {
            Self::Reflow => '~',
            Self::Removed => '-',
            Self::Added => '+',
        }
    }

    const fn foreground(self) -> Color {
        match self {
            Self::Reflow => Palette::FAINT,
            Self::Removed => Palette::GHOST,
            Self::Added => Palette::CURRENT,
        }
    }
}

impl GutterLayout {
    fn new(diff: &PresentedFile) -> Self {
        let label_columns = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .map(row_label_columns)
            .max()
            .unwrap_or(0);
        Self { label_columns }
    }

    fn padding(self, label: &str) -> String {
        " ".repeat(
            self.label_columns
                .saturating_sub(UnicodeWidthStr::width(label)),
        )
    }

    fn width(self) -> usize {
        self.label_columns + UnicodeWidthStr::width(" │ ")
    }
}

fn row_label_columns(row: &ReviewRow) -> usize {
    match row {
        ReviewRow::Current(line) => {
            UnicodeWidthStr::width(source_label(line.number, None).as_str())
        }
        ReviewRow::Reflow(line) => {
            UnicodeWidthStr::width(source_label(line.number, Some(SourceMarker::Reflow)).as_str())
        }
        ReviewRow::Removed(line) => {
            UnicodeWidthStr::width(source_label(line.number, Some(SourceMarker::Removed)).as_str())
        }
        ReviewRow::Added(line) => {
            UnicodeWidthStr::width(source_label(line.number, Some(SourceMarker::Added)).as_str())
        }
        ReviewRow::LineEnding { .. } | ReviewRow::FileBoundary => 0,
        ReviewRow::Moved { before, after } => {
            UnicodeWidthStr::width(moved_label(*before, after.number).as_str())
        }
        ReviewRow::Wordwise(word) => word
            .after_line
            .or(word.before_line)
            .map(|number| source_label(number, None))
            .map(|label| UnicodeWidthStr::width(label.as_str()))
            .unwrap_or(0),
        ReviewRow::Elision(_) => UnicodeWidthStr::width(VERTICAL_ELLIPSIS),
    }
}

fn source_label(number: usize, marker: Option<SourceMarker>) -> String {
    let Some(marker) = marker else {
        return number.to_string();
    };
    format!("{} {number}", marker.glyph())
}

fn moved_label(before: Option<usize>, after: usize) -> String {
    let Some(before) = before else {
        return after.to_string();
    };
    format!("{before} → {after}")
}

/// Open one or more presented file reviews or retained input notices.
pub fn run(reviews: Vec<ReviewItem>) -> Result<()> {
    ensure!(!reviews.is_empty(), "cannot open an empty review");
    let mut session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;
    let mut app = App::new(reviews);

    let event_result = run_event_loop(&mut terminal, &mut app);

    // All cleanup attempts run even when drawing or input failed.
    let session_result = session.restore();
    event_result?;
    session_result
}

/// Terminal modes that unwind independently after errors and panics.
struct TerminalSession {
    raw_mode: bool,
    alternate_screen: bool,
    cursor_hidden: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let mut session = Self {
            raw_mode: false,
            alternate_screen: false,
            cursor_hidden: false,
        };

        enable_raw_mode().context("failed to enable raw mode")?;
        session.raw_mode = true;

        // Mark first so Drop still attempts recovery after a partial terminal write.
        session.alternate_screen = true;
        execute!(io::stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;

        session.cursor_hidden = true;
        execute!(io::stdout(), Hide).context("failed to hide cursor")?;

        Ok(session)
    }

    fn restore(&mut self) -> Result<()> {
        let mut first_error = None;

        if self.cursor_hidden {
            match execute!(io::stdout(), Show) {
                Ok(()) => self.cursor_hidden = false,
                Err(error) => first_error = Some(anyhow::Error::from(error)),
            }
        }
        if self.alternate_screen {
            match execute!(io::stdout(), LeaveAlternateScreen) {
                Ok(()) => self.alternate_screen = false,
                Err(error) if first_error.is_none() => {
                    first_error = Some(anyhow::Error::from(error));
                }
                Err(_) => {}
            }
        }
        if self.raw_mode {
            match disable_raw_mode() {
                Ok(()) => self.raw_mode = false,
                Err(error) if first_error.is_none() => {
                    first_error = Some(anyhow::Error::from(error));
                }
                Err(_) => {}
            }
        }

        let Some(error) = first_error else {
            return Ok(());
        };
        Err(error.context("failed to restore terminal"))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Viewport over one file inside a path-ordered review.
#[derive(Debug)]
struct App {
    reviews: Vec<ReviewItem>,
    file_index: usize,
    scroll: usize,
    viewport_rows: usize,
    total_rows: usize,
}

impl App {
    fn new<T>(reviews: Vec<T>) -> Self
    where
        T: Into<ReviewItem>,
    {
        assert!(!reviews.is_empty(), "review needs at least one file");
        let reviews = reviews.into_iter().map(Into::into).collect();
        Self {
            reviews,
            file_index: 0,
            scroll: 0,
            viewport_rows: 1,
            total_rows: 0,
        }
    }

    fn current_review(&self) -> &ReviewItem {
        &self.reviews[self.file_index]
    }

    #[cfg(test)]
    fn current_diff(&self) -> &PresentedFile {
        let ReviewItem::Presented(diff) = self.current_review() else {
            panic!("current review is not a diff");
        };
        diff
    }

    fn ribbon_items(&self) -> Vec<RibbonItem<'_>> {
        self.reviews
            .iter()
            .map(|review| RibbonItem {
                path: review.path(),
                generated: review.is_generated(),
            })
            .collect()
    }

    fn max_scroll(&self) -> usize {
        self.total_rows.saturating_sub(self.viewport_rows.max(1))
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount).min(self.max_scroll());
    }

    fn page_size(&self) -> usize {
        self.viewport_rows.max(1)
    }

    fn previous_file(&mut self) {
        if self.file_index == 0 {
            return;
        }

        self.file_index -= 1;
        self.scroll = 0;
    }

    fn next_file(&mut self) {
        if self.file_index + 1 >= self.reviews.len() {
            return;
        }

        self.file_index += 1;
        self.scroll = 0;
    }
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    terminal
        .draw(|frame| render(frame, app))
        .context("failed to draw structural review")?;

    loop {
        let event = event::read().context("failed to read terminal input")?;
        match event {
            Event::Key(key) => {
                if handle_key(app, key) == KeyOutcome::Exit {
                    break;
                }
                terminal
                    .draw(|frame| render(frame, app))
                    .context("failed to redraw structural review")?;
            }
            Event::Resize(_, _) => {
                terminal
                    .draw(|frame| render(frame, app))
                    .context("failed to redraw resized structural review")?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyOutcome {
    Continue,
    Exit,
}

fn handle_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    if key.kind == KeyEventKind::Release {
        return KeyOutcome::Continue;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyOutcome::Exit;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return KeyOutcome::Exit,
        KeyCode::Left | KeyCode::Char('h' | '[') => app.previous_file(),
        KeyCode::Right | KeyCode::Char('l' | ']') => app.next_file(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(1),
        KeyCode::PageUp | KeyCode::Char('u') => app.scroll_up(app.page_size()),
        KeyCode::PageDown | KeyCode::Char(' ' | 'd') => app.scroll_down(app.page_size()),
        KeyCode::Home => app.scroll = 0,
        KeyCode::End => app.scroll = app.max_scroll(),
        _ => {}
    }
    KeyOutcome::Continue
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let gutter = match app.current_review() {
        ReviewItem::Presented(diff) => GutterLayout::new(diff),
        ReviewItem::Notice(_) => GutterLayout::default(),
    };
    let required_width = gutter.width() + MIN_SOURCE_COLUMNS;
    if usize::from(area.width) < required_width || area.height < MIN_TERMINAL_HEIGHT {
        render_too_small(frame, area, required_width);
        return;
    }

    let body = Rect::new(
        area.x,
        area.y + HEADER_ROWS,
        area.width,
        area.height.saturating_sub(HEADER_ROWS),
    );
    let rows = compose_file_review(app.current_review(), gutter, body.width as usize);
    app.total_rows = rows.len();
    app.viewport_rows = body.height as usize;
    app.clamp_scroll();

    let ribbon_items = app.ribbon_items();
    render_file_ribbon(frame, area, &ribbon_items, app.file_index);
    let visible = rows
        .into_iter()
        .skip(app.scroll)
        .take(app.viewport_rows)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), body);
}

fn render_file_ribbon(frame: &mut Frame<'_>, area: Rect, items: &[RibbonItem<'_>], active: usize) {
    let line = file_ribbon(items, active, usize::from(area.width));
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

/// Contiguous ribbon run around the active review, with explicit hidden edges.
fn file_ribbon(items: &[RibbonItem<'_>], active: usize, width: usize) -> Line<'static> {
    if items.is_empty() || active >= items.len() || width == 0 {
        return Line::from("");
    }

    let margin_width = UnicodeWidthStr::width(RIBBON_MARGIN).min(width.saturating_sub(1));
    let available = width.saturating_sub(margin_width);
    let mut start = active;
    let mut end = active + 1;

    // Grow evenly from the active item. A contiguous run keeps ordering legible.
    loop {
        let left_first = active - start < end - active;
        let sides = if left_first {
            [RibbonSide::Left, RibbonSide::Right]
        } else {
            [RibbonSide::Right, RibbonSide::Left]
        };
        let mut grew = false;
        for side in sides {
            let candidate = match side {
                RibbonSide::Left if start > 0 => Some((start - 1, end)),
                RibbonSide::Right if end < items.len() => Some((start, end + 1)),
                RibbonSide::Left | RibbonSide::Right => None,
            };
            let Some((candidate_start, candidate_end)) = candidate else {
                continue;
            };
            let candidate_width = ribbon_run_width(items, candidate_start, candidate_end);
            if candidate_width > available {
                continue;
            }

            start = candidate_start;
            end = candidate_end;
            grew = true;
            break;
        }
        if !grew {
            break;
        }
    }

    let mut spans = vec![Span::raw(" ".repeat(margin_width))];
    let hidden_left = start > 0;
    let hidden_right = end < items.len();
    if hidden_left {
        spans.push(Span::styled(RIBBON_OMISSION, muted()));
        spans.push(Span::styled(RIBBON_SEPARATOR, muted()));
    }

    let edge_width = ribbon_edge_width(hidden_left) + ribbon_edge_width(hidden_right);
    let item_widths = (start..end)
        .map(|index| ribbon_item_width(items[index]))
        .collect::<Vec<_>>();
    let separator_width = UnicodeWidthStr::width(RIBBON_SEPARATOR);
    let separators_width = separator_width * item_widths.len().saturating_sub(1);
    let items_width = available.saturating_sub(edge_width + separators_width);
    let full_items_width = item_widths.iter().sum::<usize>();

    for (offset, index) in (start..end).enumerate() {
        if offset > 0 {
            spans.push(Span::styled(RIBBON_SEPARATOR, muted()));
        }
        let budget = if full_items_width <= items_width || index != active {
            item_widths[offset]
        } else {
            // Only the active-only fallback can overflow: keep its tail and badge visible.
            items_width
        };
        spans.extend(ribbon_item_spans(items[index], index == active, budget));
    }

    if hidden_right {
        spans.push(Span::styled(RIBBON_SEPARATOR, muted()));
        spans.push(Span::styled(RIBBON_OMISSION, muted()));
    }
    clip_line(spans, width)
}

#[derive(Clone, Copy)]
enum RibbonSide {
    Left,
    Right,
}

fn ribbon_run_width(items: &[RibbonItem<'_>], start: usize, end: usize) -> usize {
    let item_width = (start..end)
        .map(|index| ribbon_item_width(items[index]))
        .sum::<usize>();
    let separators =
        end.saturating_sub(start + 1) + usize::from(start > 0) + usize::from(end < items.len());
    item_width
        + separators * UnicodeWidthStr::width(RIBBON_SEPARATOR)
        + usize::from(start > 0) * UnicodeWidthStr::width(RIBBON_OMISSION)
        + usize::from(end < items.len()) * UnicodeWidthStr::width(RIBBON_OMISSION)
}

fn ribbon_edge_width(hidden: bool) -> usize {
    usize::from(hidden)
        * (UnicodeWidthStr::width(RIBBON_OMISSION) + UnicodeWidthStr::width(RIBBON_SEPARATOR))
}

fn ribbon_item_width(item: RibbonItem<'_>) -> usize {
    UnicodeWidthStr::width(item.path)
        + usize::from(item.generated) * UnicodeWidthStr::width(GENERATED_BADGE)
}

fn ribbon_item_spans(item: RibbonItem<'_>, active: bool, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let active_style = Style::default()
        .fg(Palette::PATH)
        .add_modifier(Modifier::BOLD);
    let path_style = if active { active_style } else { muted() };
    let badge = if item.generated { GENERATED_BADGE } else { "" };
    let reserved = UnicodeWidthStr::width(badge);
    if reserved > width {
        let label = format!("{}{badge}", item.path);
        let label = clip_text_start(&label, width);
        return vec![Span::styled(label, path_style)];
    }

    let mut spans = Vec::new();
    let path = clip_text_start(item.path, width - reserved);
    if !path.is_empty() {
        spans.push(Span::styled(path, path_style));
    }
    if !badge.is_empty() {
        let badge_style = Style::default().fg(Palette::WARNING);
        let badge_style = if active {
            badge_style.add_modifier(Modifier::BOLD)
        } else {
            badge_style
        };
        spans.push(Span::styled(badge, badge_style));
    }
    spans
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, required_width: usize) {
    let message = vec![
        Line::styled(
            "Mig needs a little more room.",
            Style::default().fg(Palette::WARNING),
        ),
        Line::from(""),
        Line::from(format!(
            "Need {required_width}x{MIN_TERMINAL_HEIGHT}; current {}x{}.",
            area.width, area.height
        )),
        Line::styled("Resize or press q.", Style::default().fg(Palette::MUTED)),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        centered_rect(area, area.width.saturating_sub(4).min(60), 6),
    );
}

fn compose_review(diff: &PresentedFile, gutter: GutterLayout, width: usize) -> Vec<Line<'static>> {
    if diff.hunks.is_empty() {
        return vec![Line::from(""), Line::styled("  no changes", muted())];
    }

    let mut rows = Vec::new();
    for (index, hunk) in diff.hunks.iter().enumerate() {
        if index > 0 {
            rows.push(Line::from(""));
        }
        for row in &hunk.rows {
            match row {
                ReviewRow::Current(line) => {
                    rows.push(source_line(line, None, gutter, width, !line.has_changes()));
                }
                ReviewRow::Reflow(line) => rows.push(source_line(
                    line,
                    Some(SourceMarker::Reflow),
                    gutter,
                    width,
                    false,
                )),
                // Historical rows stay ghosted without competing with current syntax.
                ReviewRow::Removed(line) => rows.push(source_line(
                    line,
                    Some(SourceMarker::Removed),
                    gutter,
                    width,
                    false,
                )),
                ReviewRow::Added(line) => rows.push(source_line(
                    line,
                    Some(SourceMarker::Added),
                    gutter,
                    width,
                    false,
                )),
                ReviewRow::LineEnding { before, after } => {
                    rows.push(line_ending_diff(*before, *after, gutter, width));
                }
                ReviewRow::Moved { before, after } => {
                    rows.push(moved_source_line(after, *before, gutter, width));
                }
                ReviewRow::Wordwise(word) => rows.push(word_diff_line(word, gutter, width)),
                ReviewRow::Elision(_) => rows.push(elision_line(gutter, width)),
                ReviewRow::FileBoundary => rows.push(eof_guardian_line(gutter)),
            }
        }
    }
    rows
}

fn compose_file_review(
    review: &ReviewItem,
    gutter: GutterLayout,
    width: usize,
) -> Vec<Line<'static>> {
    match review {
        ReviewItem::Presented(diff) => compose_review(diff, gutter, width),
        ReviewItem::Notice(notice) => compose_notice(notice, width),
    }
}

fn compose_notice(notice: &FileNotice, width: usize) -> Vec<Line<'static>> {
    match notice {
        FileNotice::TooLarge {
            before_bytes,
            after_bytes,
            limit_bytes,
            ..
        } => {
            let heading = format!(
                "  file not shown — exceeds the {} per-revision limit",
                format_bytes(*limit_bytes)
            );
            let mut details = vec![Span::raw("  ")];
            if let Some(bytes) = before_bytes {
                details.push(Span::styled("before ", muted()));
                details.push(Span::styled(format_bytes(*bytes), surrounding_style()));
            }
            if let Some(bytes) = after_bytes {
                if before_bytes.is_some() {
                    details.push(Span::styled("  ·  ", muted()));
                }
                details.push(Span::styled("current ", muted()));
                details.push(Span::styled(format_bytes(*bytes), surrounding_style()));
            }

            vec![
                Line::from(""),
                clip_line(
                    vec![Span::styled(heading, Style::default().fg(Palette::WARNING))],
                    width,
                ),
                clip_line(details, width),
            ]
        }
        FileNotice::TooManyLines {
            before_lines,
            after_lines,
            limit_lines,
            ..
        } => {
            let heading =
                format!("  file not shown — exceeds the {limit_lines}-line per-revision limit");
            let mut details = vec![Span::raw("  ")];
            if let Some(lines) = before_lines {
                details.push(Span::styled("before ", muted()));
                details.push(Span::styled(format!("{lines} lines"), surrounding_style()));
            }
            if let Some(lines) = after_lines {
                if before_lines.is_some() {
                    details.push(Span::styled("  ·  ", muted()));
                }
                details.push(Span::styled("current ", muted()));
                details.push(Span::styled(format!("{lines} lines"), surrounding_style()));
            }

            vec![
                Line::from(""),
                clip_line(
                    vec![Span::styled(heading, Style::default().fg(Palette::WARNING))],
                    width,
                ),
                clip_line(details, width),
            ]
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    for (unit, scale) in [("GiB", GIB), ("MiB", MIB), ("KiB", KIB)] {
        if bytes < scale {
            continue;
        }
        if bytes.is_multiple_of(scale) {
            return format!("{} {unit}", bytes / scale);
        }
        return format!("{:.1} {unit}", bytes as f64 / scale as f64);
    }

    format!("{bytes} B")
}

fn line_ending_diff(
    before: Option<LineEnding>,
    after: Option<LineEnding>,
    gutter: GutterLayout,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(gutter.width()))];
    match (before, after) {
        (Some(before), Some(after)) => {
            spans.push(Span::styled("line ending: ", muted()));
            spans.push(Span::styled(
                line_ending_label(before),
                change_emphasis_style(DiffMark::Removed),
            ));
            spans.push(Span::styled(" → ", Style::default().fg(Palette::MOVE)));
            spans.push(Span::styled(
                line_ending_label(after),
                change_emphasis_style(DiffMark::Added),
            ));
        }
        (Some(before), None) => {
            spans.push(Span::styled("before: ", muted()));
            spans.push(Span::styled(
                line_ending_label(before),
                change_emphasis_style(DiffMark::Removed),
            ));
        }
        (None, Some(after)) => {
            spans.push(Span::styled("after: ", muted()));
            spans.push(Span::styled(
                line_ending_label(after),
                change_emphasis_style(DiffMark::Added),
            ));
        }
        (None, None) => unreachable!("a line-ending row always has a source side"),
    }
    clip_line(spans, width)
}

fn line_ending_label(ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::Missing => "no newline at end of file",
        LineEnding::Lf => "LF",
        LineEnding::CrLf => "CRLF",
    }
}

fn source_line(
    line: &SourceRow,
    marker: Option<SourceMarker>,
    gutter: GutterLayout,
    width: usize,
    row_is_context: bool,
) -> Line<'static> {
    let mut spans = Vec::new();
    let row_is_ghost = marker == Some(SourceMarker::Removed);
    let label = source_label(line.number, marker);
    spans.push(Span::raw(gutter.padding(&label)));
    if let Some(marker) = marker {
        spans.push(Span::styled(
            format!("{} ", marker.glyph()),
            Style::default().fg(marker.foreground()),
        ));
    }
    let (gutter_color, dim_gutter) = if row_is_context {
        (Palette::FAINT, true)
    } else if row_is_ghost {
        (Palette::GHOST, false)
    } else {
        (Palette::GUTTER, false)
    };
    let gutter_style = Style::default().fg(gutter_color);
    let gutter_style = if dim_gutter {
        gutter_style.add_modifier(Modifier::DIM)
    } else {
        gutter_style
    };
    spans.push(Span::styled(format!("{} │ ", line.number), gutter_style));
    let mut used = gutter.width();
    let mut source_column = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let (text, text_width) = clip_source_text(&span.text, width - used, source_column);
        if text_width == 0 {
            continue;
        }
        let style = if row_is_ghost {
            ghost_line_style(span.mark)
        } else if span.mark == DiffMark::Context {
            softened_syntax_style(span.syntax)
        } else {
            change_emphasis_style(span.mark)
        };
        spans.push(Span::styled(text, style));
        used += text_width;
        source_column += text_width;
    }
    Line::from(spans)
}

fn moved_source_line(
    line: &SourceRow,
    before: Option<usize>,
    gutter: GutterLayout,
    width: usize,
) -> Line<'static> {
    let label = moved_label(before, line.number);
    let gutter_text = format!("{}{label} │ ", gutter.padding(&label));
    let mut spans = vec![Span::styled(
        gutter_text,
        Style::default().fg(Palette::MOVE),
    )];
    let mut used = gutter.width();
    let mut source_column = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let (text, text_width) = clip_source_text(&span.text, width - used, source_column);
        used += text_width;
        source_column += text_width;
        let style = if span.mark == DiffMark::Context {
            softened_syntax_style(span.syntax)
        } else {
            change_emphasis_style(span.mark)
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

fn elision_line(gutter: GutterLayout, width: usize) -> Line<'static> {
    let label = VERTICAL_ELLIPSIS;
    let gutter_text = format!("{}{label} │ ", gutter.padding(label));
    let mut spans = vec![Span::styled(
        gutter_text,
        Style::default()
            .fg(Palette::FAINT)
            .add_modifier(Modifier::DIM),
    )];
    if gutter.width() < width {
        spans.push(Span::styled(
            VERTICAL_ELLIPSIS,
            Style::default()
                .fg(Palette::FAINT)
                .add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// Aligned file boundary that becomes the final scrollable row of an EOF hunk.
fn eof_guardian_line(gutter: GutterLayout) -> Line<'static> {
    Line::styled(
        format!("{} │", gutter.padding("")),
        Style::default().fg(Palette::GUTTER),
    )
}

fn word_diff_line(diff: &WordDiff, gutter: GutterLayout, width: usize) -> Line<'static> {
    let number = diff.after_line.or(diff.before_line);
    let gutter = number
        .map(|number| {
            let label = number.to_string();
            format!("{}{label} │ ", gutter.padding(&label))
        })
        .unwrap_or_else(|| " ".repeat(gutter.width()));
    let mut spans = vec![Span::styled(gutter, Style::default().fg(Palette::GUTTER))];
    spans.push(Span::styled(diff.prefix.clone(), surrounding_style()));
    if !diff.removed.is_empty() {
        spans.push(Span::styled(
            diff.removed.clone(),
            change_emphasis_style(DiffMark::Removed),
        ));
    }
    if !diff.removed.is_empty() && !diff.added.is_empty() {
        spans.push(Span::styled(" → ", Style::default().fg(Palette::MOVE)));
    }
    if !diff.added.is_empty() {
        spans.push(Span::styled(
            diff.added.clone(),
            change_emphasis_style(DiffMark::Added),
        ));
    }
    spans.push(Span::styled(diff.suffix.clone(), surrounding_style()));
    clip_line(spans, width)
}

/// One emphasis palette for changed source across linewise, compact, and metadata rows.
fn change_emphasis_style(mark: DiffMark) -> Style {
    let foreground = match mark {
        DiffMark::Removed => Palette::GHOST_EMPHASIS,
        DiffMark::Added => Palette::CURRENT,
        DiffMark::Context => unreachable!("context source does not carry change emphasis"),
    };
    Style::default().fg(foreground).add_modifier(Modifier::BOLD)
}

fn softened_syntax_style(class: SyntaxClass) -> Style {
    let foreground = softened_syntax_foreground(class);
    Style::default().fg(foreground)
}

/// Monochrome body style for a rendered before-side `-` ghost line.
fn ghost_line_style(mark: DiffMark) -> Style {
    match mark {
        DiffMark::Removed => change_emphasis_style(mark),
        DiffMark::Context => Style::default().fg(Palette::GHOST),
        DiffMark::Added => unreachable!("before-side ghost lines cannot contain added source"),
    }
}

fn clip_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut clipped = Vec::new();
    let mut used = 0;
    for span in spans {
        if used >= width {
            break;
        }
        let text = clip_text(span.content.as_ref(), width - used);
        used += UnicodeWidthStr::width(text.as_str());
        clipped.push(Span::styled(text, span.style));
    }
    Line::from(clipped)
}

fn clip_text(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    output
}

/// Display source tabs before clipping so Ratatui cannot discard their indentation.
fn clip_source_text(text: &str, width: usize, start_column: usize) -> (String, usize) {
    let mut output = String::with_capacity(text.len());
    let mut used = 0;
    for character in text.chars() {
        if character == '\t' {
            let column = start_column.saturating_add(used);
            let tab_width = SOURCE_TAB_STOP - column % SOURCE_TAB_STOP;
            let available = width.saturating_sub(used);
            let displayed = tab_width.min(available);
            output.push_str(&" ".repeat(displayed));
            used += displayed;
            if displayed < tab_width {
                break;
            }
            continue;
        }

        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    (output, used)
}

/// Tail-preserving path clipping keeps the distinguishing file name on screen.
fn clip_text_start(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let omission_width = UnicodeWidthStr::width(RIBBON_OMISSION);
    if width <= omission_width {
        return clip_text(RIBBON_OMISSION, width);
    }

    let mut suffix = Vec::new();
    let mut used = omission_width;
    for character in text.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        suffix.push(character);
        used += character_width;
    }
    suffix.reverse();

    let mut output = String::from(RIBBON_OMISSION);
    output.extend(suffix);
    output
}

fn syntax_foreground(class: SyntaxClass) -> Color {
    match class {
        SyntaxClass::Plain | SyntaxClass::Punctuation | SyntaxClass::Identifier => Palette::TEXT,
        SyntaxClass::Keyword => Palette::KEYWORD,
        SyntaxClass::Type => Palette::TYPE,
        SyntaxClass::Literal => Palette::LITERAL,
        SyntaxClass::String => Palette::STRING,
        SyntaxClass::Comment => Palette::COMMENT,
    }
}

/// Two parts syntax color to one part neutral gray: recessed, but still legible.
fn softened_syntax_foreground(class: SyntaxClass) -> Color {
    blend_rgb(syntax_foreground(class), Palette::MUTED)
}

fn blend_rgb(color: Color, neutral: Color) -> Color {
    let (Color::Rgb(red, green, blue), Color::Rgb(nr, ng, nb)) = (color, neutral) else {
        return color;
    };
    let blend =
        |channel: u8, neutral: u8| ((u16::from(channel) * 2 + u16::from(neutral)) / 3) as u8;
    Color::Rgb(blend(red, nr), blend(green, ng), blend(blue, nb))
}

fn muted() -> Style {
    Style::default().fg(Palette::MUTED)
}

fn surrounding_style() -> Style {
    softened_syntax_style(SyntaxClass::Plain)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

/// Restrained foreground palette; the user's terminal owns every background.
struct Palette;

impl Palette {
    const TEXT: Color = Color::Rgb(205, 214, 226);
    const PATH: Color = Color::Rgb(139, 190, 255);
    const MUTED: Color = Color::Rgb(119, 132, 149);
    const FAINT: Color = Color::Rgb(77, 89, 103);
    const GUTTER: Color = Color::Rgb(87, 100, 116);
    const WARNING: Color = Color::Rgb(225, 174, 89);
    const MOVE: Color = Color::Rgb(101, 181, 190);
    const CURRENT: Color = Color::Rgb(100, 205, 144);
    /// Restrained violet-gray family reserved for rendered before-side ghost lines.
    const GHOST: Color = Color::Rgb(127, 110, 149);
    const GHOST_EMPHASIS: Color = Color::Rgb(136, 106, 153);

    const KEYWORD: Color = Color::Rgb(195, 148, 235);
    const TYPE: Color = Color::Rgb(105, 190, 199);
    const LITERAL: Color = Color::Rgb(224, 178, 112);
    const STRING: Color = Color::Rgb(151, 196, 130);
    const COMMENT: Color = Color::Rgb(139, 151, 167);
}

#[cfg(test)]
mod tests;

use crate::diff::{
    CodeLine, CodeRole, CodeSpan, DiffMark, DiffRow, FileDiff, LineEnding, SyntaxClass, WordDiff,
};
use crate::review::{FileNotice, FileReview};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceSide {
    Before,
    Current,
}

/// File-wide prefix geometry shared by every unified row treatment.
#[derive(Clone, Copy, Debug, Default)]
struct GutterLayout {
    label_columns: usize,
}

/// Minimal review projection needed to lay out the file ribbon.
#[derive(Clone, Copy, Debug)]
struct RibbonItem<'a> {
    path: &'a str,
    generated: bool,
}

impl GutterLayout {
    fn new(diff: &FileDiff) -> Self {
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

fn row_label_columns(row: &DiffRow) -> usize {
    match row {
        DiffRow::Code { line, role } => {
            let marker = (*role == CodeRole::Reflow).then_some('~');
            UnicodeWidthStr::width(source_label(line.number, marker).as_str())
        }
        DiffRow::Linewise { before, after } => {
            let before = before
                .as_ref()
                .map(|line| source_label(line.number, Some('-')))
                .map(|label| UnicodeWidthStr::width(label.as_str()))
                .unwrap_or(0);
            let after = after
                .as_ref()
                .map(|line| source_label(line.number, Some('+')))
                .map(|label| UnicodeWidthStr::width(label.as_str()))
                .unwrap_or(0);
            before.max(after)
        }
        DiffRow::LineEnding { .. } => 0,
        DiffRow::Moved { before, after } => {
            UnicodeWidthStr::width(moved_label(*before, after.number).as_str())
        }
        DiffRow::Wordwise(word) => word
            .after_line
            .or(word.before_line)
            .map(|number| source_label(number, None))
            .map(|label| UnicodeWidthStr::width(label.as_str()))
            .unwrap_or(0),
        DiffRow::Elision(_) => UnicodeWidthStr::width(VERTICAL_ELLIPSIS),
    }
}

fn source_label(number: usize, marker: Option<char>) -> String {
    let Some(marker) = marker else {
        return number.to_string();
    };
    format!("{marker} {number}")
}

fn moved_label(before: Option<usize>, after: usize) -> String {
    let Some(before) = before else {
        return after.to_string();
    };
    format!("{before} → {after}")
}

/// Opens one or more planned file reviews or retained input notices.
pub fn run(reviews: Vec<FileReview>) -> Result<()> {
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
    reviews: Vec<FileReview>,
    file_index: usize,
    scroll: usize,
    viewport_rows: usize,
    total_rows: usize,
}

impl App {
    fn new<T>(reviews: Vec<T>) -> Self
    where
        T: Into<FileReview>,
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

    fn current_review(&self) -> &FileReview {
        &self.reviews[self.file_index]
    }

    #[cfg(test)]
    fn current_diff(&self) -> &FileDiff {
        let FileReview::Diff(diff) = self.current_review() else {
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
        FileReview::Diff(diff) => GutterLayout::new(diff),
        FileReview::Notice(_) => GutterLayout::default(),
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

fn compose_review(diff: &FileDiff, gutter: GutterLayout, width: usize) -> Vec<Line<'static>> {
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
                DiffRow::Code { line, role } => {
                    let marker = (*role == CodeRole::Reflow).then_some(('~', Palette::FAINT));
                    let row_is_context = *role == CodeRole::Context;
                    rows.push(source_line(
                        line,
                        marker,
                        gutter,
                        width,
                        SourceSide::Current,
                        row_is_context,
                    ));
                }
                DiffRow::Linewise { before, after } => {
                    if let Some(before) = before {
                        rows.push(source_line(
                            before,
                            Some(('-', Palette::BEFORE)),
                            gutter,
                            width,
                            SourceSide::Before,
                            false,
                        ));
                    }
                    if let Some(after) = after {
                        rows.push(source_line(
                            after,
                            Some(('+', Palette::CURRENT)),
                            gutter,
                            width,
                            SourceSide::Current,
                            false,
                        ));
                    }
                }
                DiffRow::LineEnding { before, after } => {
                    rows.push(line_ending_diff(*before, *after, gutter, width));
                }
                DiffRow::Moved { before, after } => {
                    rows.push(moved_source_line(after, *before, gutter, width));
                }
                DiffRow::Wordwise(word) => rows.push(word_diff_line(word, gutter, width)),
                DiffRow::Elision(_) => rows.push(elision_line(gutter, width)),
            }
        }
        if hunk.ends_at_eof {
            rows.push(eof_guardian_line(gutter));
        }
    }
    rows
}

fn compose_file_review(
    review: &FileReview,
    gutter: GutterLayout,
    width: usize,
) -> Vec<Line<'static>> {
    match review {
        FileReview::Diff(diff) => compose_review(diff, gutter, width),
        FileReview::Notice(notice) => compose_notice(notice, width),
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
                word_diff_style(Palette::BEFORE),
            ));
            spans.push(Span::styled(" → ", Style::default().fg(Palette::MOVE)));
            spans.push(Span::styled(
                line_ending_label(after),
                word_diff_style(Palette::CURRENT),
            ));
        }
        (Some(before), None) => {
            spans.push(Span::styled("before: ", muted()));
            spans.push(Span::styled(
                line_ending_label(before),
                word_diff_style(Palette::BEFORE),
            ));
        }
        (None, Some(after)) => {
            spans.push(Span::styled("after: ", muted()));
            spans.push(Span::styled(
                line_ending_label(after),
                word_diff_style(Palette::CURRENT),
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
    line: &CodeLine,
    marker: Option<(char, Color)>,
    gutter: GutterLayout,
    width: usize,
    side: SourceSide,
    row_is_context: bool,
) -> Line<'static> {
    let mut spans = Vec::new();
    let label = source_label(line.number, marker.map(|(marker, _)| marker));
    spans.push(Span::raw(gutter.padding(&label)));
    if let Some((marker, color)) = marker {
        spans.push(Span::styled(
            format!("{marker} "),
            Style::default().fg(color),
        ));
    }
    let (gutter_color, dim_gutter) = if row_is_context {
        (Palette::FAINT, true)
    } else if side == SourceSide::Before {
        (Palette::PAST, false)
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
        let style = if span.mark == DiffMark::Context && side == SourceSide::Before {
            past_syntax_style(span.syntax)
        } else if span.mark == DiffMark::Context {
            softened_syntax_style(span.syntax)
        } else {
            code_style(span)
        };
        spans.push(Span::styled(text, style));
        used += text_width;
        source_column += text_width;
    }
    Line::from(spans)
}

fn moved_source_line(
    line: &CodeLine,
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
            code_style(span)
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
            word_diff_style(Palette::BEFORE),
        ));
    }
    if !diff.removed.is_empty() && !diff.added.is_empty() {
        spans.push(Span::styled(" → ", Style::default().fg(Palette::MOVE)));
    }
    if !diff.added.is_empty() {
        spans.push(Span::styled(
            diff.added.clone(),
            word_diff_style(Palette::CURRENT),
        ));
    }
    spans.push(Span::styled(diff.suffix.clone(), surrounding_style()));
    clip_line(spans, width)
}

fn code_style(span: &CodeSpan) -> Style {
    match span.mark {
        DiffMark::Removed => word_diff_style(Palette::BEFORE),
        DiffMark::Added => word_diff_style(Palette::CURRENT),
        DiffMark::Context => syntax_style(span.syntax, syntax_foreground(span.syntax)),
    }
}

/// Sole source of bold body text: an exact changed span.
fn word_diff_style(foreground: Color) -> Style {
    Style::default().fg(foreground).add_modifier(Modifier::BOLD)
}

fn softened_syntax_style(class: SyntaxClass) -> Style {
    let foreground = softened_syntax_foreground(class);
    syntax_style(class, foreground)
}

fn past_syntax_style(class: SyntaxClass) -> Style {
    let foreground = tint_rgb(syntax_foreground(class), Palette::PAST);
    syntax_style(class, foreground)
}

fn syntax_style(class: SyntaxClass, foreground: Color) -> Style {
    let style = Style::default().fg(foreground);
    match class {
        SyntaxClass::Comment => style.add_modifier(Modifier::ITALIC),
        _ => style,
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

fn tint_rgb(color: Color, tint: Color) -> Color {
    let (Color::Rgb(red, green, blue), Color::Rgb(tr, tg, tb)) = (color, tint) else {
        return color;
    };
    let tint = |channel: u8, tint: u8| ((u16::from(channel) + u16::from(tint) * 2) / 3) as u8;
    Color::Rgb(tint(red, tr), tint(green, tg), tint(blue, tb))
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
    const PAST: Color = Color::Rgb(88, 174, 198);
    const BEFORE: Color = Color::Rgb(105, 157, 235);
    const CURRENT: Color = Color::Rgb(100, 205, 144);

    const KEYWORD: Color = Color::Rgb(195, 148, 235);
    const TYPE: Color = Color::Rgb(105, 190, 199);
    const LITERAL: Color = Color::Rgb(224, 178, 112);
    const STRING: Color = Color::Rgb(151, 196, 130);
    const COMMENT: Color = Color::Rgb(139, 151, 167);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{Hunk, LineCoverage, diff_file},
        fixture::{AFTER, BEFORE, LABEL, web},
    };
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    #[test]
    fn fixture_renders_as_a_quiet_inline_file_diff() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let gutter = GutterLayout::new(&diff);
        assert_eq!(gutter.label_columns, 7);
        assert_eq!(gutter.width(), 10);
        assert_eq!(gutter.width() + MIN_SOURCE_COLUMNS, 72);

        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(100, 50);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render fixture");

        let buffer = terminal.backend().buffer();
        let screen = buffer_text(buffer);
        assert!(screen.contains(LABEL));
        assert!(screen.contains("fn load_profile"));
        assert!(screen.contains("cached.and_then(validate_profile)"));
        assert!(screen.contains("- 26 │     // Cached profiles are already trusted."));
        assert!(screen.contains("+ 19 │     // Cached profiles must be revalidated."));
        assert!(screen.contains(" 15 │"));
        assert!(screen.contains(" 24 │"));
        assert!(screen.contains("fn should_refresh"));
        assert!(screen.contains("Stale and legacy profiles need refreshing"));
        assert!(screen.contains("profile.schema < 4 || age > Duration::from_secs(300)"));
        assert!(screen.contains("fn display_label"));
        assert!(screen.contains(".to_owned().replace('\\n', \" \")"));
        assert!(screen.contains("16 → 38 │ fn cache_key"));
        assert_eq!(screen.matches("⋮ │ ⋮").count(), 3);
        assert!(!screen.contains("session.tenant()"));
        assert!(!screen.contains("key.push"));
        assert!(screen.contains("43 │ }"));
        assert_eq!(screen.matches("fn cache_key").count(), 1);
        assert!(screen.contains("legacy_counter → {Metric, ReviewMeter}"));
        assert!(screen.contains("25 │ fn render_response(profile: &Profile) -> Response {"));
        assert!(
            screen.contains("~ 26 │     Response::new(StatusCode::OK, profile.display_name())")
        );
        assert!(screen.contains("27 │ }"));

        let lines = screen.lines().collect::<Vec<_>>();
        let move_end = lines
            .iter()
            .position(|line| line.contains("43 │ }"))
            .expect("move must have a closing row");
        let import = lines
            .iter()
            .position(|line| line.contains("legacy_counter → {Metric, ReviewMeter}"))
            .expect("import must be rendered");
        assert_eq!(import, move_end + 2, "hunks need exactly one blank row");

        for annotation in [
            "LOGIC",
            "COMMENTARY",
            "MOVE",
            "SKIM",
            "BEFORE",
            "AFTER",
            "syntax tokens",
            "token-equivalent",
            "folded",
            "review ·",
        ] {
            assert!(
                !screen.contains(annotation),
                "unexpected annotation: {annotation}"
            );
        }
        let mut gutter_columns = screen
            .lines()
            .filter_map(|line| line.chars().position(|character| character == '│'))
            .collect::<Vec<_>>();
        gutter_columns.sort_unstable();
        gutter_columns.dedup();
        assert_eq!(gutter_columns, vec![8], "misaligned gutters");

        assert!(run_on_line_matches(buffer, LABEL, LABEL, |cell| {
            cell.modifier.contains(Modifier::BOLD)
        }));
        assert!(ascii_run_has_modifier(buffer, "16 │", Modifier::DIM));
        assert!(run_on_line_matches(
            buffer,
            "already trusted",
            "// Cached profiles ",
            |cell| {
                cell.fg == tint_rgb(syntax_foreground(SyntaxClass::Comment), Palette::PAST)
                    && !cell.modifier.contains(Modifier::DIM)
            }
        ));
        assert!(run_on_line_matches(
            buffer,
            "already trusted",
            "26 │",
            |cell| cell.fg == Palette::PAST && !cell.modifier.contains(Modifier::DIM)
        ));
        assert!(run_on_line_matches(
            buffer,
            "fn load_profile",
            "fn",
            |cell| {
                cell.fg == softened_syntax_foreground(SyntaxClass::Keyword)
                    && !cell.modifier.contains(Modifier::DIM)
                    && !cell.modifier.contains(Modifier::BOLD)
            }
        ));
        assert!(run_on_line_matches(
            buffer,
            "fn load_profile",
            "ProfileCache",
            |cell| {
                cell.fg == softened_syntax_foreground(SyntaxClass::Type)
                    && !cell.modifier.contains(Modifier::DIM)
            }
        ));
        for (line, gutter) in [
            ("already trusted", "26 │"),
            ("must be revalidated", "19 │"),
            ("cached.and_then", "20 │"),
            ("Response::new", "26 │"),
            ("legacy_counter", "3 │"),
        ] {
            assert!(run_on_line_matches(buffer, line, gutter, |cell| {
                !cell.modifier.contains(Modifier::DIM)
            }));
        }
        assert!(run_on_line_matches(
            buffer,
            "legacy_counter",
            "use crate::telemetry::",
            |cell| {
                cell.fg == softened_syntax_foreground(SyntaxClass::Plain)
                    && !cell.modifier.contains(Modifier::DIM)
            }
        ));
        assert!(run_on_line_matches(
            buffer,
            "fn render_response",
            "fn",
            |cell| {
                cell.fg == softened_syntax_foreground(SyntaxClass::Keyword)
                    && !cell.modifier.contains(Modifier::DIM)
            }
        ));
        assert!(run_on_line_matches(buffer, "fn cache_key", "fn", |cell| {
            cell.fg == softened_syntax_foreground(SyntaxClass::Keyword)
                && !cell.modifier.contains(Modifier::DIM)
        }));
        assert!(run_on_line_matches(
            buffer,
            "Response::new",
            "Response",
            |cell| {
                cell.fg == softened_syntax_foreground(SyntaxClass::Identifier)
                    && !cell.modifier.contains(Modifier::DIM)
            }
        ));
        assert!(run_on_line_matches(
            buffer,
            "cached.and_then",
            ".and_then(validate_profile)",
            |cell| { cell.fg == Palette::CURRENT && cell.modifier.contains(Modifier::BOLD) }
        ));
        assert!(run_on_line_matches(
            buffer,
            "already trusted",
            "already",
            |cell| cell.fg == Palette::BEFORE && cell.modifier.contains(Modifier::BOLD)
        ));
        for (line, marker) in [
            ("already trusted", "- "),
            ("must be revalidated", "+ "),
            ("Response::new", "~ "),
        ] {
            assert!(run_on_line_matches(buffer, line, marker, |cell| {
                !cell.modifier.contains(Modifier::BOLD)
            }));
        }
        assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
    }

    #[test]
    fn narrow_terminal_uses_a_plain_size_message() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(60, 14);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render narrow fallback");

        let buffer = terminal.backend().buffer();
        let screen = buffer_text(buffer);
        assert!(screen.contains("Mig needs a little more room"));
        assert!(screen.contains("72x18"));
        assert!(!screen.contains(LABEL));
        assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
    }

    #[test]
    fn gutter_follows_displayed_labels_instead_of_hunk_coverage() {
        let line = |number| CodeLine {
            number,
            spans: vec![CodeSpan {
                text: "content".to_owned(),
                syntax: SyntaxClass::Plain,
                mark: DiffMark::Context,
            }],
        };
        let diff = FileDiff {
            path: "src/large.rs".to_owned(),
            generated: false,
            hunks: vec![Hunk {
                coverage: LineCoverage {
                    before: Some(1..2),
                    after: Some(1..2),
                },
                ends_at_eof: false,
                rows: vec![
                    DiffRow::Code {
                        line: line(2),
                        role: CodeRole::Context,
                    },
                    DiffRow::Linewise {
                        before: Some(line(123_456)),
                        after: None,
                    },
                ],
            }],
        };

        let gutter = GutterLayout::new(&diff);
        assert_eq!(gutter.label_columns, UnicodeWidthStr::width("- 123456"));

        let rows = compose_review(&diff, gutter, 80);
        let rows = rows
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let pipe_columns = rows
            .iter()
            .filter_map(|line| line.chars().position(|character| character == '│'))
            .collect::<Vec<_>>();
        assert_eq!(pipe_columns, vec![gutter.label_columns + 1; 2]);
        assert!(rows.iter().any(|line| line.contains("- 123456 │")));
    }

    #[test]
    fn source_tab_stops_continue_across_styled_spans() {
        let line = CodeLine {
            number: 1,
            spans: vec![
                CodeSpan {
                    text: "ab".to_owned(),
                    syntax: SyntaxClass::Plain,
                    mark: DiffMark::Context,
                },
                CodeSpan {
                    text: "\tvalue".to_owned(),
                    syntax: SyntaxClass::Keyword,
                    mark: DiffMark::Added,
                },
            ],
        };

        let row = source_line(
            &line,
            None,
            GutterLayout { label_columns: 1 },
            80,
            SourceSide::Current,
            false,
        );

        assert_eq!(composed_line_text(&row), "1 │ ab  value");
    }

    #[test]
    fn eof_hunk_ends_with_an_aligned_guardian() {
        let diff = diff_file("notes.txt", "old\n", "new\n").expect("plain diff");
        let gutter = GutterLayout::new(&diff);

        let rows = compose_review(&diff, gutter, 80);
        let rows = rows.iter().map(composed_line_text).collect::<Vec<_>>();
        let guardian = rows.last().expect("EOF hunk needs a guardian");
        let expected = format!("{}│", " ".repeat(gutter.label_columns + 1));

        assert_eq!(rows.len(), 3);
        assert_eq!(guardian, &expected);
        assert_eq!(guardian.matches('│').count(), 1);
    }

    #[test]
    fn html_wrapper_surrounds_one_reflowed_image_with_additions() {
        let fixture = web::HTML;
        let diff = diff_file(fixture.path, fixture.before, fixture.after).expect("HTML diff");
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render HTML wrapper");

        let screen = buffer_text(terminal.backend().buffer());
        let opening = screen
            .find("+ 2 │   <div class=\"profile-card__portrait\">")
            .expect("added wrapper opening");
        let image = screen
            .find("~ 3 │     <img")
            .expect("retained reindented image");
        let closing = screen
            .find("+ 8 │   </div>")
            .expect("added wrapper closing");
        assert!(opening < image && image < closing);
        assert_eq!(screen.matches("src=\"/avatars/ada.webp\"").count(), 1);
        assert!(!screen.contains("- 2 │   <img"));
    }

    #[test]
    fn tabbed_html_reflow_renders_current_source_indentation() {
        let before = "<article>\n\t<img\n\t\tsrc=\"avatar.webp\"\n\t/>\n</article>\n";
        let after = concat!(
            "<article>\n",
            "\t<div>\n",
            "\t\t<img",
            "                           ",
            "\n",
            "\t\t\tsrc=\"avatar.webp\"\n",
            "\t\t/>\n",
            "\t</div>\n",
            "</article>\n",
        );
        let diff = diff_file("after.html", before, after).expect("HTML diff");
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render tabbed HTML wrapper");

        let screen = buffer_text(terminal.backend().buffer());
        for expected in [
            "+ 2 │     <div>",
            "~ 3 │         <img",
            "~ 4 │             src=\"avatar.webp\"",
            "~ 5 │         />",
            "+ 6 │     </div>",
        ] {
            assert!(
                screen.contains(expected),
                "missing rendered row {expected:?}"
            );
        }
    }

    #[test]
    fn navigation_stays_inside_the_composed_review() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let mut app = App::new(vec![diff]);
        let gutter = GutterLayout::new(app.current_diff());
        app.total_rows = compose_review(app.current_diff(), gutter, 80).len();
        app.viewport_rows = 8;

        assert_eq!(
            handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            KeyOutcome::Continue
        );
        let end = app.max_scroll();
        assert_eq!(app.scroll, end);

        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.scroll, end);

        handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        );
        assert_eq!(app.scroll, 8.min(end));

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        assert_eq!(app.scroll, 0);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(app.scroll, 8.min(end));
    }

    #[test]
    fn end_navigation_lands_on_the_eof_guardian() {
        let before = (1..=24)
            .map(|line| format!("old line {line}\n"))
            .collect::<String>();
        let after = (1..=24)
            .map(|line| format!("new line {line}\n"))
            .collect::<String>();
        let diff = diff_file("notes.txt", &before, &after).expect("plain diff");
        let gutter = GutterLayout::new(&diff);
        let expected = format!("{}│", " ".repeat(gutter.label_columns + 1));
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render top of review");
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render end of review");

        let final_row = buffer_row_text(terminal.backend().buffer(), 17);
        assert!(app.scroll > 0, "fixture must exercise scrolling");
        assert_eq!(final_row.trim_end(), expected);
    }

    #[test]
    fn generated_marker_survives_a_long_file_header() {
        let mut diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        diff.generated = true;
        diff.path = format!("{}generated.rs", "very-long-directory/".repeat(8));
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render generated file");

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("@generated"));
    }

    #[test]
    fn file_ribbon_lists_reviews_and_moves_the_bold_active_style() {
        let first = diff_file("src/first.rs", "before\n", "after\n").expect("first diff");
        let second = diff_file("notes.txt", "before\n", "after\n").expect("second diff");
        let mut generated =
            diff_file("build/generated.rs", "before\n", "after\n").expect("generated diff");
        generated.generated = true;
        let mut app = App::new(vec![first, second, generated]);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render first file");

        let buffer = terminal.backend().buffer();
        let header = buffer_row_text(buffer, 0);
        assert!(
            header
                .contains("src/first.rs \u{2502} notes.txt \u{2502} build/generated.rs @generated"),
            "unexpected ribbon: {header:?}"
        );
        assert!(run_on_line_matches(
            buffer,
            "src/first.rs",
            "src/first.rs",
            |cell| cell.fg == Palette::PATH && cell.modifier.contains(Modifier::BOLD)
        ));
        assert!(run_on_line_matches(
            buffer,
            "notes.txt",
            "notes.txt",
            |cell| cell.fg == Palette::MUTED && !cell.modifier.contains(Modifier::BOLD)
        ));
        assert!(run_on_line_matches(
            buffer,
            "build/generated.rs",
            "@generated",
            |cell| cell.fg == Palette::WARNING && !cell.modifier.contains(Modifier::BOLD)
        ));

        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render second file");

        let buffer = terminal.backend().buffer();
        assert!(run_on_line_matches(
            buffer,
            "notes.txt",
            "notes.txt",
            |cell| cell.fg == Palette::PATH && cell.modifier.contains(Modifier::BOLD)
        ));
        assert!(run_on_line_matches(
            buffer,
            "src/first.rs",
            "src/first.rs",
            |cell| cell.fg == Palette::MUTED && !cell.modifier.contains(Modifier::BOLD)
        ));
    }

    #[test]
    fn oversized_review_stays_in_the_ribbon_and_explains_the_limit() {
        let first = diff_file("src/first.rs", "before\n", "after\n").expect("first diff");
        let notice = FileNotice::too_large(
            "assets/large.txt",
            Some(2 * 1024 * 1024),
            Some(42 * 1024 * 1024),
            16 * 1024 * 1024,
        );
        let reviews = vec![FileReview::from(first), FileReview::Notice(notice)];
        let mut app = App::new(reviews);
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render oversized review");

        let buffer = terminal.backend().buffer();
        let screen = buffer_text(buffer);
        assert!(screen.contains("src/first.rs │ assets/large.txt"));
        assert!(screen.contains("file not shown — exceeds the 16 MiB per-revision limit"));
        assert!(screen.contains("before 2 MiB  ·  current 42 MiB"));
        assert!(!screen.contains("no changes"));
        assert!(run_on_line_matches(
            buffer,
            "assets/large.txt",
            "assets/large.txt",
            |cell| cell.fg == Palette::PATH && cell.modifier.contains(Modifier::BOLD)
        ));
    }

    #[test]
    fn narrow_ribbon_keeps_a_long_generated_active_path_visible() {
        let first = diff_file("src/before.rs", "before\n", "after\n").expect("first diff");
        let active_path = format!("{}active-generated.rs", "deep-directory/".repeat(12));
        let mut active = diff_file(&active_path, "before\n", "after\n").expect("active diff");
        active.generated = true;
        let last = diff_file("src/after.rs", "before\n", "after\n").expect("last diff");
        let mut app = App::new(vec![first, active, last]);
        app.file_index = 1;
        let backend = TestBackend::new(72, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render fitted ribbon");

        let buffer = terminal.backend().buffer();
        let header = buffer_row_text(buffer, 0);
        assert!(
            header.contains("active-generated.rs @generated"),
            "active tail and badge were clipped: {header:?}"
        );
        assert_eq!(header.matches(RIBBON_OMISSION).count(), 3);
        assert!(!header.contains("src/before.rs"));
        assert!(!header.contains("src/after.rs"));
        assert!(UnicodeWidthStr::width(header.trim_end()) <= 72);
        assert!(run_on_line_matches(
            buffer,
            "active-generated.rs",
            "active-generated.rs",
            |cell| cell.fg == Palette::PATH && cell.modifier.contains(Modifier::BOLD)
        ));
    }

    #[test]
    fn plain_diff_explains_a_missing_final_newline() {
        let diff = diff_file("notes.txt", "same\n", "same").expect("plain diff");
        let mut app = App::new(vec![diff]);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render line-ending change");

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("line ending: LF → no newline at end of file"));

        let gutter = GutterLayout::new(app.current_diff());
        let rows = compose_review(app.current_diff(), gutter, 100);
        let rows = rows.iter().map(composed_line_text).collect::<Vec<_>>();
        let ending = rows
            .iter()
            .position(|row| row.contains("no newline at end of file"))
            .expect("line-ending explanation");
        let expected = format!("{}│", " ".repeat(gutter.label_columns + 1));
        assert_eq!(ending + 2, rows.len());
        assert_eq!(rows.last(), Some(&expected));
    }

    #[test]
    fn file_navigation_clamps_and_resets_the_viewport() {
        let first = diff_file(
            "src/first.rs",
            "fn value() -> u8 { 1 }\n",
            "fn value() -> u8 { 2 }\n",
        )
        .expect("first diff");
        let second = diff_file(
            "src/second.rs",
            "fn value() -> u8 { 2 }\n",
            "fn value() -> u8 { 3 }\n",
        )
        .expect("second diff");
        let mut app = App::new(vec![first, second]);
        app.scroll = 4;

        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.current_diff().path, "src/second.rs");
        assert_eq!(app.scroll, 0);

        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.current_diff().path, "src/second.rs");

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render second file");
        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("src/first.rs \u{2502} src/second.rs"));

        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.current_diff().path, "src/first.rs");
    }

    fn buffer_text(buffer: &Buffer) -> String {
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn composed_line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn buffer_row_text(buffer: &Buffer, row: usize) -> String {
        buffer.content[row * buffer.area.width as usize..(row + 1) * buffer.area.width as usize]
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn ascii_run_has_modifier(buffer: &Buffer, text: &str, modifier: Modifier) -> bool {
        let symbols = text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .any(|row| {
                row.windows(symbols.len()).any(|cells| {
                    cells.iter().zip(&symbols).all(|(cell, symbol)| {
                        cell.symbol() == symbol && cell.modifier.contains(modifier)
                    })
                })
            })
    }

    fn run_on_line_matches(
        buffer: &Buffer,
        line_text: &str,
        run_text: &str,
        predicate: impl Fn(&ratatui::buffer::Cell) -> bool,
    ) -> bool {
        let line_symbols = line_text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let run_symbols = run_text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .filter(|row| {
                row.windows(line_symbols.len()).any(|cells| {
                    cells
                        .iter()
                        .zip(&line_symbols)
                        .all(|(cell, symbol)| cell.symbol() == symbol)
                })
            })
            .any(|row| {
                row.windows(run_symbols.len()).any(|cells| {
                    cells
                        .iter()
                        .zip(&run_symbols)
                        .all(|(cell, symbol)| cell.symbol() == symbol && predicate(cell))
                })
            })
    }
}

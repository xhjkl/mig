use crate::diff::{
    CodeLine, CodeRole, CodeSpan, DiffMark, DiffRow, FileDiff, SyntaxClass, WordDiff,
};
use anyhow::{Context, Result};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceSide {
    Before,
    Current,
}

/// File-wide prefix geometry shared by every unified row treatment.
#[derive(Clone, Copy, Debug)]
struct GutterLayout {
    label_columns: usize,
}

impl GutterLayout {
    fn new(diff: &FileDiff) -> Self {
        let label_columns = diff
            .windows
            .iter()
            .flat_map(|window| &window.rows)
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

/// Opens one planned unified review in the terminal.
pub fn run(diff: FileDiff) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;
    let mut app = App::new(diff);

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

/// Viewport over one file's inline review stream.
#[derive(Debug)]
struct App {
    diff: FileDiff,
    scroll: usize,
    viewport_rows: usize,
    total_rows: usize,
}

impl App {
    fn new(diff: FileDiff) -> Self {
        Self {
            diff,
            scroll: 0,
            viewport_rows: 1,
            total_rows: 0,
        }
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
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(1),
        KeyCode::PageUp => app.scroll_up(app.page_size()),
        KeyCode::PageDown | KeyCode::Char(' ') => app.scroll_down(app.page_size()),
        KeyCode::Home => app.scroll = 0,
        KeyCode::End => app.scroll = app.max_scroll(),
        _ => {}
    }
    KeyOutcome::Continue
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let gutter = GutterLayout::new(&app.diff);
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
    let rows = compose_review(&app.diff, gutter, body.width as usize);
    app.total_rows = rows.len();
    app.viewport_rows = body.height as usize;
    app.clamp_scroll();

    render_file_header(frame, area, &app.diff.path);
    let visible = rows
        .into_iter()
        .skip(app.scroll)
        .take(app.viewport_rows)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), body);
}

fn render_file_header(frame: &mut Frame<'_>, area: Rect, path: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                path.to_owned(),
                Style::default()
                    .fg(Palette::PATH)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
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
    if diff.windows.is_empty() {
        return vec![Line::from(""), Line::styled("  no changes", muted())];
    }

    let mut rows = Vec::new();
    for (index, window) in diff.windows.iter().enumerate() {
        if index > 0 {
            rows.push(Line::from(""));
        }
        for row in &window.rows {
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
                DiffRow::Moved { before, after } => {
                    rows.push(moved_source_line(after, *before, gutter, width));
                }
                DiffRow::Wordwise(word) => rows.push(word_diff_line(word, gutter, width)),
                DiffRow::Elision(_) => rows.push(elision_line(gutter, width)),
            }
        }
    }
    rows
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
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = clip_text(&span.text, width - used);
        let text_width = UnicodeWidthStr::width(text.as_str());
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
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = clip_text(&span.text, width - used);
        used += UnicodeWidthStr::width(text.as_str());
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
        diff::{DiffWindow, LineMapping, diff_file},
        fixture::{AFTER, BEFORE, LABEL},
    };
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    #[test]
    fn fixture_renders_as_a_quiet_inline_file_diff() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let gutter = GutterLayout::new(&diff);
        assert_eq!(gutter.label_columns, 7);
        assert_eq!(gutter.width(), 10);
        assert_eq!(gutter.width() + MIN_SOURCE_COLUMNS, 72);

        let mut app = App::new(diff);
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
        assert_eq!(import, move_end + 2, "windows need exactly one blank row");

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
        let mut app = App::new(diff);
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
    fn gutter_follows_displayed_labels_instead_of_window_mapping() {
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
            windows: vec![DiffWindow {
                mapping: LineMapping {
                    before: Some(1..2),
                    after: Some(1..2),
                },
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
    fn navigation_stays_inside_the_composed_review() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let mut app = App::new(diff);
        let gutter = GutterLayout::new(&app.diff);
        app.total_rows = compose_review(&app.diff, gutter, 80).len();
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
    }

    fn buffer_text(buffer: &Buffer) -> String {
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
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

use crate::fixtures::{inline_change_state, move_without_identity_state, whole_function_state};
use anyhow::Result;
use clap::ValueEnum;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mig::diff_model::{
    AlignedBlock, BlockKind, DiffLayout, DiffState, InlineAlignmentId, SideId, SideRole,
    TokenEditKind,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::collections::BTreeSet;
use std::io::{self, Stdout};
use std::ops::Range;
use std::time::Duration;

const MIN_TERMINAL_WIDTH: u16 = 60;
const MIN_TERMINAL_HEIGHT: u16 = 20;
const MIN_STACKED_CONTENT_WIDTH: u16 = 72;
const MIN_TIMELINE_CONTENT_WIDTH: u16 = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TryoutScene {
    Timeline,
    InlineChange,
    WholeFunction,
    MoveWithoutIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TuiTheme {
    Auto,
    Dark,
    Light,
}

impl TuiTheme {
    fn palette(self) -> Palette {
        match self {
            // TODO: Probe OSC 11 before entering alt screen and choose from luminance.
            Self::Auto | Self::Dark => Palette::dark_terminal(),
            Self::Light => Palette::light_terminal(),
        }
    }
}

pub(crate) fn run_tryout(scene: TryoutScene, theme: TuiTheme) -> Result<()> {
    let app = match scene {
        TryoutScene::Timeline => AppState::timeline_fixture(theme),
        TryoutScene::InlineChange => AppState::comparison(theme, inline_change_state()),
        TryoutScene::WholeFunction => AppState::comparison(theme, whole_function_state()),
        TryoutScene::MoveWithoutIdentity => {
            AppState::comparison(theme, move_without_identity_state())
        }
    };
    run_app(app)
}

pub(crate) fn run_state(state: &DiffState, theme: TuiTheme) -> Result<()> {
    run_app(AppState::comparison(theme, state.clone()))
}

fn run_app(app: AppState) -> Result<()> {
    run_tui(app)
}

fn run_tui(mut app: AppState) -> Result<()> {
    let mut session = TerminalSession::enter()?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_event_loop(&mut terminal, &mut app);
    terminal.show_cursor()?;
    session.leave()?;
    result
}

struct TerminalSession {
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let session = Self { active: true };
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        Ok(session)
    }

    fn leave(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let mut stdout = io::stdout();
        execute!(stdout, DisableMouseCapture, LeaveAlternateScreen)?;
        disable_raw_mode()?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[derive(Clone, Debug)]
struct AppState {
    mode: AppMode,
    theme: TuiTheme,
    timeline: Timeline,
    cursor: TimelineCursor,
    marks: BTreeSet<TimelineEntryId>,
    opened: Option<OpenedComparison>,
}

impl AppState {
    fn comparison(theme: TuiTheme, diff: DiffState) -> Self {
        let entry = TimelineEntry {
            id: TimelineEntryId(0),
            backing_rev: None,
            label: "comparison".to_owned(),
            summary: "opened diff".to_owned(),
            diff,
        };
        Self {
            mode: AppMode::Comparison,
            theme,
            timeline: Timeline {
                entries: vec![entry],
            },
            cursor: TimelineCursor { index: 0 },
            marks: BTreeSet::new(),
            opened: Some(OpenedComparison::Cursor(TimelineEntryId(0))),
        }
    }

    fn timeline_fixture(theme: TuiTheme) -> Self {
        let timeline = Timeline {
            entries: vec![
                TimelineEntry {
                    id: TimelineEntryId(0),
                    backing_rev: Some(BackingRev::git("HEAD~2")),
                    label: "HEAD~2".to_owned(),
                    summary: "baseline before Mig session".to_owned(),
                    diff: inline_change_state(),
                },
                TimelineEntry {
                    id: TimelineEntryId(1),
                    backing_rev: None,
                    label: "src/tui.rs+1-1".to_owned(),
                    summary: "rename inline token wording".to_owned(),
                    diff: inline_change_state(),
                },
                TimelineEntry {
                    id: TimelineEntryId(2),
                    backing_rev: None,
                    label: "src/lib.rs+3-1".to_owned(),
                    summary: "reshape summarize_turn".to_owned(),
                    diff: whole_function_state(),
                },
                TimelineEntry {
                    id: TimelineEntryId(3),
                    backing_rev: None,
                    label: "src/tui.rs+1-1".to_owned(),
                    summary: "move tracing without identity".to_owned(),
                    diff: move_without_identity_state(),
                },
                TimelineEntry {
                    id: TimelineEntryId(4),
                    backing_rev: Some(BackingRev::git("HEAD")),
                    label: "HEAD".to_owned(),
                    summary: "durable checkpoint".to_owned(),
                    diff: move_without_identity_state(),
                },
            ],
        };
        Self {
            mode: AppMode::Timeline,
            theme,
            timeline,
            cursor: TimelineCursor { index: 1 },
            marks: BTreeSet::new(),
            opened: None,
        }
    }

    fn palette(&self) -> Palette {
        self.theme.palette()
    }

    fn cursor_entry(&self) -> Option<&TimelineEntry> {
        self.timeline.entries.get(self.cursor.index)
    }

    fn cursor_id(&self) -> Option<TimelineEntryId> {
        self.cursor_entry().map(|entry| entry.id)
    }

    fn move_cursor_up(&mut self) {
        self.cursor.index = self.cursor.index.saturating_sub(1);
    }

    fn move_cursor_down(&mut self) {
        if self.cursor.index + 1 < self.timeline.entries.len() {
            self.cursor.index += 1;
        }
    }

    fn toggle_mark(&mut self) {
        let Some(id) = self.cursor_id() else {
            return;
        };
        if !self.marks.remove(&id) {
            self.marks.insert(id);
        }
    }

    fn toggle_mark_and_advance(&mut self) {
        self.toggle_mark();
        self.move_cursor_down();
    }

    fn open_comparison(&mut self) {
        let opened = if self.marks.is_empty() {
            self.cursor_id().map(OpenedComparison::Cursor)
        } else {
            Some(OpenedComparison::Marked(
                self.marks.iter().copied().collect(),
            ))
        };
        let Some(opened) = opened else {
            return;
        };
        self.opened = Some(opened);
        self.mode = AppMode::Comparison;
    }

    fn opened_diff(&self) -> Option<&DiffState> {
        match self.opened.as_ref()? {
            OpenedComparison::Cursor(id) => self.timeline.entry(*id).map(|entry| &entry.diff),
            OpenedComparison::Marked(ids) => ids
                .iter()
                .filter_map(|id| self.timeline.entry(*id))
                .next_back()
                .map(|entry| &entry.diff),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppMode {
    Timeline,
    Comparison,
}

#[derive(Clone, Debug)]
struct Timeline {
    entries: Vec<TimelineEntry>,
}

impl Timeline {
    fn entry(&self, id: TimelineEntryId) -> Option<&TimelineEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

#[derive(Clone, Debug)]
struct TimelineEntry {
    id: TimelineEntryId,
    backing_rev: Option<BackingRev>,
    label: String,
    summary: String,
    diff: DiffState,
}

impl TimelineEntry {
    fn identity(&self) -> &str {
        if let Some(backing_rev) = self.backing_rev.as_ref() {
            backing_rev.display_rev()
        } else {
            &self.label
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct TimelineEntryId(usize);

#[derive(Clone, Debug, Eq, PartialEq)]
struct BackingRev {
    system: VcsSystem,
    rev: String,
}

impl BackingRev {
    fn git(rev: &str) -> Self {
        Self {
            system: VcsSystem::Git,
            rev: rev.to_owned(),
        }
    }

    fn display_rev(&self) -> &str {
        match self.system {
            VcsSystem::Git => &self.rev,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VcsSystem {
    Git,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimelineCursor {
    index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OpenedComparison {
    Cursor(TimelineEntryId),
    Marked(Vec<TimelineEntryId>),
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut AppState,
) -> Result<()> {
    terminal.draw(|frame| render(frame, app))?;

    loop {
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    let is_ctrl_c = key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if is_ctrl_c {
                        break;
                    }
                    if key.kind == KeyEventKind::Press {
                        if handle_key(app, key.code) == KeyOutcome::Exit {
                            break;
                        }
                        terminal.draw(|frame| render(frame, app))?;
                    }
                }
                Event::Resize(_, _) => {
                    terminal.draw(|frame| render(frame, app))?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyOutcome {
    Continue,
    Exit,
}

fn handle_key(app: &mut AppState, code: KeyCode) -> KeyOutcome {
    match app.mode {
        AppMode::Timeline => match code {
            KeyCode::Up => app.move_cursor_up(),
            KeyCode::Down => app.move_cursor_down(),
            KeyCode::Char(' ') => app.toggle_mark_and_advance(),
            KeyCode::Enter => app.open_comparison(),
            KeyCode::Esc | KeyCode::Char('q') => return KeyOutcome::Exit,
            _ => {}
        },
        AppMode::Comparison => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.mode = AppMode::Timeline;
            }
            _ => {}
        },
    }
    KeyOutcome::Continue
}

fn render(frame: &mut Frame<'_>, app: &AppState) {
    let palette = app.palette();
    if frame.area().width < MIN_TERMINAL_WIDTH || frame.area().height < MIN_TERMINAL_HEIGHT {
        render_too_small(frame, &palette);
        return;
    }

    match app.mode {
        AppMode::Timeline => render_timeline(frame, app, &palette),
        AppMode::Comparison => render_comparison(frame, app, &palette),
    }
}

fn render_comparison(frame: &mut Frame<'_>, app: &AppState, palette: &Palette) {
    let Some(state) = app.opened_diff() else {
        render_timeline(frame, app, palette);
        return;
    };
    let area = frame.area();
    let layout = resolved_layout(state);
    frame.render_widget(Clear, area);
    match layout {
        ResolvedLayout::Split => {
            let rows = render_split_rows(state, palette);
            let split = split_geometry(area, &rows);
            render_split_header(frame, state, area, palette, &split);
            render_split_body(frame, body_area(area), palette, &split, rows);
        }
        ResolvedLayout::Stacked => {
            let rows = render_unified_rows(state, palette);
            let stacked = stacked_geometry(area, state, &rows);
            render_stacked_header(frame, state, area, palette, &stacked);
            render_stacked_body(frame, body_area(area), palette, &stacked, rows);
        }
    }
}

fn render_too_small(frame: &mut Frame<'_>, palette: &Palette) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let message = vec![
        Line::from(Span::styled(
            "Mig needs more room",
            Style::default()
                .fg(palette.warning_fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "Resize to at least {}x{}.",
            MIN_TERMINAL_WIDTH, MIN_TERMINAL_HEIGHT
        )),
        Line::from(format!("Current size is {}x{}.", area.width, area.height)),
        Line::from(""),
        Line::from("Press q or Ctrl-C to exit."),
    ];

    let paragraph = Paragraph::new(message)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" mig ")
                .border_style(Style::default().fg(palette.separator)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_timeline(frame: &mut Frame<'_>, app: &AppState, palette: &Palette) {
    let area = frame.area();
    let rows = timeline_rows(app, palette);
    let timeline = timeline_geometry(area, &rows);
    frame.render_widget(Clear, area);

    let header = Line::from(vec![
        Span::styled(
            format!(
                "entry {} / {}",
                app.cursor.index + 1,
                app.timeline.entries.len()
            ),
            header_style(palette),
        ),
        Span::styled(
            format!("  marks {}", app.marks.len()),
            Style::default().fg(palette.gutter_fg),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(
            timeline
                .content
                .centered_line(header, separator_style(palette)),
        ),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );
    render_full_width_separator(frame, area, area.y + 1, palette);

    let body = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(3),
        ..area
    };
    render_timeline_rows(frame, body, &timeline, rows);

    let footer_y = area.y + area.height.saturating_sub(1);
    let footer = timeline_footer(app, palette);
    frame.render_widget(
        Paragraph::new(
            timeline
                .content
                .positioned_line(footer, Style::default().fg(palette.gutter_fg)),
        ),
        Rect {
            x: area.x,
            y: footer_y,
            width: area.width,
            height: 1,
        },
    );
}

fn timeline_rows(app: &AppState, palette: &Palette) -> Vec<TimelineRenderRow> {
    app.timeline
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let is_cursor = index == app.cursor.index;
            let is_marked = app.marks.contains(&entry.id);
            let line = timeline_entry_line(entry, index, is_cursor, is_marked, palette);
            let style = if is_cursor {
                Style::default()
                    .fg(palette.changed_new_fg)
                    .bg(palette.changed_bg)
            } else {
                Style::default().fg(palette.text)
            };
            TimelineRenderRow { line, style }
        })
        .collect()
}

fn render_timeline_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    timeline: &TimelineGeometry,
    rows: Vec<TimelineRenderRow>,
) {
    for (offset, row) in rows.into_iter().enumerate() {
        let y = area.y + offset as u16;
        if y >= area.y + area.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(timeline.content.positioned_line(row.line, row.style)),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
    }
}

#[derive(Clone, Debug)]
struct TimelineRenderRow {
    line: Line<'static>,
    style: Style,
}

fn timeline_entry_line(
    entry: &TimelineEntry,
    index: usize,
    is_cursor: bool,
    is_marked: bool,
    palette: &Palette,
) -> Line<'static> {
    let bg = if is_cursor {
        palette.changed_bg
    } else {
        Color::Reset
    };
    let text = if is_cursor {
        palette.changed_new_fg
    } else {
        palette.text
    };
    let muted = if is_cursor {
        palette.changed_old_fg
    } else {
        palette.gutter_fg
    };
    let identity = entry.identity();
    let identity_color = if is_cursor {
        text
    } else if entry.backing_rev.is_some() {
        palette.header
    } else {
        text
    };
    let cursor = if is_cursor { ">" } else { " " };
    let mark = if is_marked { "*" } else { " " };

    Line::from(vec![
        Span::styled(
            format!("{cursor}{mark} "),
            Style::default().fg(muted).bg(bg),
        ),
        Span::styled(
            format!("{:>2} ", index + 1),
            Style::default().fg(muted).bg(bg),
        ),
        Span::styled(
            format!("{identity:<18} "),
            Style::default()
                .fg(identity_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(entry.summary.clone(), Style::default().fg(text).bg(bg)),
    ])
}

fn timeline_footer(app: &AppState, palette: &Palette) -> Line<'static> {
    let opened = match app.opened.as_ref() {
        Some(OpenedComparison::Cursor(id)) => format!("opened entry {}", id.0 + 1),
        Some(OpenedComparison::Marked(ids)) => format!("opened {} marked entries", ids.len()),
        None => "opened none".to_owned(),
    };
    Line::from(Span::styled(opened, Style::default().fg(palette.gutter_fg)))
}

fn body_area(area: Rect) -> Rect {
    Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolvedLayout {
    Split,
    Stacked,
}

fn resolved_layout(state: &DiffState) -> ResolvedLayout {
    match state.view.viewport.layout {
        DiffLayout::Split => ResolvedLayout::Split,
        DiffLayout::Stacked => ResolvedLayout::Stacked,
        DiffLayout::Auto => auto_layout(state),
    }
}

fn auto_layout(state: &DiffState) -> ResolvedLayout {
    // Coarse v0 heuristic: adjacent old/new lines are treated as a possible
    // transformation, so split view wins. This needs revisiting before v1 with
    // AST-aware identity and better moved-block detection.
    if has_touching_old_new_lines(state) {
        ResolvedLayout::Split
    } else {
        ResolvedLayout::Stacked
    }
}

fn has_touching_old_new_lines(state: &DiffState) -> bool {
    for block in &state.graph.blocks {
        let (left, right) = block_side_presence(block);
        if block.kind.is_changed() && left && right {
            return true;
        }
    }

    for blocks in state.graph.blocks.windows(2) {
        let (left_before, right_before) = block_side_presence(&blocks[0]);
        let (left_after, right_after) = block_side_presence(&blocks[1]);
        let old_then_new = left_before && !right_before && !left_after && right_after;
        let new_then_old = !left_before && right_before && left_after && !right_after;
        if old_then_new || new_then_old {
            return true;
        }
    }

    false
}

fn block_side_presence(block: &AlignedBlock) -> (bool, bool) {
    (
        block_has_lines(block, SideId(0)),
        block_has_lines(block, SideId(1)),
    )
}

fn block_has_lines(block: &AlignedBlock, side: SideId) -> bool {
    block
        .sides
        .get(side.0)
        .and_then(Option::as_ref)
        .is_some_and(|span| span.range.start < span.range.end)
}

fn render_split_header(
    frame: &mut Frame<'_>,
    state: &DiffState,
    area: Rect,
    palette: &Palette,
    split: &SplitGeometry,
) {
    let left = center_line_in_region(
        Line::from(vec![Span::styled(
            origin_text(state, SideId(0)),
            header_style(palette),
        )]),
        split.left_width,
        split.left_content.content_x,
        split.left_content.content_width,
        separator_style(palette),
    );
    frame.render_widget(
        Paragraph::new(left),
        Rect {
            x: area.x,
            y: area.y,
            width: split.left_width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(separator_span(palette))),
        Rect {
            x: split.separator_x,
            y: area.y,
            width: 1,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(center_line_in_region(
            Line::from(vec![Span::styled(
                origin_text(state, SideId(1)),
                header_style(palette),
            )]),
            split.right_width,
            split.right_content.content_x,
            split.right_content.content_width,
            separator_style(palette),
        )),
        Rect {
            x: split.right_x,
            y: area.y,
            width: split.right_width,
            height: 1,
        },
    );
    render_horizontal_separator(frame, area, area.y + 1, &split, palette);
}

fn render_stacked_header(
    frame: &mut Frame<'_>,
    state: &DiffState,
    area: Rect,
    palette: &Palette,
    stacked: &StackedGeometry,
) {
    frame.render_widget(
        Paragraph::new(stacked.content.centered_line(
            stacked_header_line(state, palette),
            separator_style(palette),
        )),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );
    render_full_width_separator(frame, area, area.y + 1, palette);
}

fn render_split_body(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    split: &SplitGeometry,
    rows: Vec<SplitRow>,
) {
    let mut offset = 0;
    for row in rows {
        let style = row_fill_style(row.kind, palette);
        let left = soft_wrap_line(row.left, split.left_content.content_width, style);
        let right = soft_wrap_line(row.right, split.right_content.content_width, style);
        let height = left.len().max(right.len());

        for index in 0..height {
            if offset >= area.height {
                return;
            }
            let y = area.y + offset;
            let left = split
                .left_content
                .positioned_line(left.get(index).cloned().unwrap_or_else(empty_line), style);
            let right = split
                .right_content
                .positioned_line(right.get(index).cloned().unwrap_or_else(empty_line), style);

            frame.render_widget(
                Paragraph::new(left),
                Rect {
                    x: area.x,
                    y,
                    width: split.left_width,
                    height: 1,
                },
            );
            frame.render_widget(
                Paragraph::new(Line::from(separator_span_for(row.kind, palette))),
                Rect {
                    x: split.separator_x,
                    y,
                    width: 1,
                    height: 1,
                },
            );
            frame.render_widget(
                Paragraph::new(right),
                Rect {
                    x: split.right_x,
                    y,
                    width: split.right_width,
                    height: 1,
                },
            );
            offset += 1;
        }
    }
}

fn render_stacked_body(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    stacked: &StackedGeometry,
    rows: Vec<UnifiedRow>,
) {
    let mut offset = 0;
    for row in rows {
        let style = row_fill_style(row.kind, palette);
        for line in soft_wrap_line(row.line, stacked.content.content_width, style) {
            if offset >= area.height {
                return;
            }
            let y = area.y + offset;
            let line = stacked.content.positioned_line(line, style);
            frame.render_widget(
                Paragraph::new(line),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            offset += 1;
        }
    }
}

fn render_horizontal_separator(
    frame: &mut Frame<'_>,
    area: Rect,
    y: u16,
    split: &SplitGeometry,
    palette: &Palette,
) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(split.left_width as usize),
            separator_style(palette),
        ))),
        Rect {
            x: area.x,
            y,
            width: split.left_width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(separator_span(palette))),
        Rect {
            x: split.separator_x,
            y,
            width: 1,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(split.right_width as usize),
            separator_style(palette),
        ))),
        Rect {
            x: split.right_x,
            y,
            width: split.right_width,
            height: 1,
        },
    );
}

fn render_full_width_separator(frame: &mut Frame<'_>, area: Rect, y: u16, palette: &Palette) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            separator_style(palette),
        ))),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
}

#[derive(Clone, Debug)]
struct SplitRow {
    left: Line<'static>,
    right: Line<'static>,
    kind: BlockKind,
}

fn render_split_rows(state: &DiffState, palette: &Palette) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    for block in &state.graph.blocks {
        let left = block_lines(state, block, SideId(0), palette);
        let right = block_lines(state, block, SideId(1), palette);
        let height = left.len().max(right.len());

        for index in 0..height {
            rows.push(SplitRow {
                left: left
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| empty_block_line(state, block, SideId(0), palette)),
                right: right
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| empty_block_line(state, block, SideId(1), palette)),
                kind: block.kind,
            });
        }
    }
    rows
}

#[derive(Clone, Debug)]
struct UnifiedRow {
    line: Line<'static>,
    kind: BlockKind,
}

fn render_unified_rows(state: &DiffState, palette: &Palette) -> Vec<UnifiedRow> {
    let mut rows = Vec::new();
    for block in &state.graph.blocks {
        let (left, right) = block_side_presence(block);
        if !block.kind.is_changed() && right {
            rows.extend(stacked_block_lines(state, block, SideId(1), ' ', palette));
        } else if !block.kind.is_changed() && left {
            rows.extend(stacked_block_lines(state, block, SideId(0), ' ', palette));
        } else {
            rows.extend(stacked_block_lines(state, block, SideId(0), '-', palette));
            rows.extend(stacked_block_lines(state, block, SideId(1), '+', palette));
        }
    }
    rows
}

fn stacked_block_lines(
    state: &DiffState,
    block: &AlignedBlock,
    side: SideId,
    marker: char,
    palette: &Palette,
) -> Vec<UnifiedRow> {
    let Some(span) = block.sides.get(side.0).and_then(Option::as_ref) else {
        return Vec::new();
    };
    let Some(doc) = state.snapshot(span.snapshot) else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    for line in span.range.clone() {
        let text = doc
            .line_text(line)
            .unwrap_or_default()
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        lines.push(UnifiedRow {
            line: render_source_line(state, block, side, line, Some(marker), &text, palette),
            kind: block.kind,
        });
    }
    lines
}

fn block_lines(
    state: &DiffState,
    block: &AlignedBlock,
    side: SideId,
    palette: &Palette,
) -> Vec<Line<'static>> {
    let Some(span) = block.sides.get(side.0).and_then(Option::as_ref) else {
        return Vec::new();
    };
    let Some(doc) = state.snapshot(span.snapshot) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for line in span.range.clone() {
        let text = doc
            .line_text(line)
            .unwrap_or_default()
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        lines.push(render_source_line(
            state, block, side, line, None, &text, palette,
        ));
    }
    lines
}

fn empty_block_line(
    state: &DiffState,
    block: &AlignedBlock,
    side: SideId,
    palette: &Palette,
) -> Line<'static> {
    let style = source_line_style(state, block.kind, side, palette);
    Line::from(vec![
        Span::styled("     ", Style::default().fg(palette.gutter_fg).bg(style.bg)),
        Span::styled(" ", Style::default().bg(style.bg)),
    ])
}

fn render_source_line(
    state: &DiffState,
    block: &AlignedBlock,
    side: SideId,
    line: u32,
    marker: Option<char>,
    text: &str,
    palette: &Palette,
) -> Line<'static> {
    let style = source_line_style(state, block.kind, side, palette);
    let gutter = marker.map_or_else(
        || format!("{:>4} ", line + 1),
        |marker| format!("{marker}{:>4} ", line + 1),
    );
    let mut spans = vec![Span::styled(
        gutter,
        Style::default().fg(palette.gutter_fg).bg(style.bg),
    )];

    let inline_segments = inline_segments_for_line(state, block, side, line);
    if inline_segments.is_empty() {
        spans.push(Span::styled(
            text.to_owned(),
            Style::default().fg(style.fg).bg(style.bg),
        ));
        return Line::from(spans);
    }

    let mut cursor = 0;
    for segment in inline_segments {
        let range = segment.range;
        if cursor < range.start {
            spans.push(Span::styled(
                text[cursor..range.start].to_owned(),
                Style::default().fg(style.fg).bg(style.bg),
            ));
        }
        spans.push(Span::styled(
            text[range.clone()].to_owned(),
            inline_style(state, segment.alignment, side, palette),
        ));
        cursor = range.end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(
            text[cursor..].to_owned(),
            Style::default().fg(style.fg).bg(style.bg),
        ));
    }

    Line::from(spans)
}

#[derive(Clone, Debug)]
struct InlineSegment {
    alignment: InlineAlignmentId,
    range: Range<usize>,
}

fn inline_segments_for_line(
    state: &DiffState,
    block: &AlignedBlock,
    side: SideId,
    line: u32,
) -> Vec<InlineSegment> {
    let mut segments = Vec::new();
    let Some(span) = block.sides.get(side.0).and_then(Option::as_ref) else {
        return segments;
    };

    for alignment_id in &block.inline_alignments {
        let Some(alignment) = state.graph.inline.get(*alignment_id) else {
            continue;
        };
        let Some(Some(line_ref)) = alignment.sides.get(side.0) else {
            continue;
        };
        if line_ref.snapshot != span.snapshot || line_ref.line != line {
            continue;
        }

        for edit in &alignment.edits {
            if edit.kind == TokenEditKind::Equal {
                continue;
            }
            let Some(Some(range)) = edit.line_ranges.get(side.0) else {
                continue;
            };
            segments.push(InlineSegment {
                alignment: *alignment_id,
                range: range.clone(),
            });
        }
    }
    segments.sort_by_key(|segment| segment.range.start);
    segments
}

struct SourceLineStyle {
    fg: Color,
    bg: Color,
}

fn source_line_style(
    state: &DiffState,
    kind: BlockKind,
    side: SideId,
    palette: &Palette,
) -> SourceLineStyle {
    if kind.is_changed() {
        let tone = side_change_tone(state, side);
        return SourceLineStyle {
            fg: match tone {
                ChangeTone::Old => palette.changed_old_fg,
                ChangeTone::New => palette.changed_new_fg,
            },
            bg: palette.changed_bg,
        };
    }

    SourceLineStyle {
        fg: palette.text,
        bg: Color::Reset,
    }
}

fn inline_style(
    state: &DiffState,
    alignment: InlineAlignmentId,
    side: SideId,
    palette: &Palette,
) -> Style {
    let hue = alignment_hue(alignment, palette);
    let bg = match side_change_tone(state, side) {
        ChangeTone::Old => hue.old_bg,
        ChangeTone::New => hue.new_bg,
    };

    Style::default()
        .fg(palette.inline_fg)
        .bg(bg)
        .add_modifier(Modifier::BOLD)
}

#[derive(Clone, Copy, Debug)]
enum ChangeTone {
    Old,
    New,
}

fn side_change_tone(state: &DiffState, side: SideId) -> ChangeTone {
    match state.side(side).map(|side| side.role) {
        Some(SideRole::Base | SideRole::Before) => ChangeTone::Old,
        Some(SideRole::After | SideRole::Ours | SideRole::Theirs) => ChangeTone::New,
        None if side.0 == 0 => ChangeTone::Old,
        None => ChangeTone::New,
    }
}

#[derive(Clone, Copy, Debug)]
struct AlignmentHue {
    old_bg: Color,
    new_bg: Color,
}

#[derive(Clone, Copy, Debug)]
struct Palette {
    text: Color,
    header: Color,
    separator: Color,
    gutter_fg: Color,
    warning_fg: Color,
    changed_bg: Color,
    changed_old_fg: Color,
    changed_new_fg: Color,
    inline_fg: Color,
    alignment_hues: [AlignmentHue; 4],
}

impl Palette {
    fn dark_terminal() -> Self {
        Self {
            text: Color::Rgb(174, 181, 190),
            header: Color::Rgb(213, 218, 226),
            separator: Color::Rgb(70, 78, 90),
            gutter_fg: Color::DarkGray,
            warning_fg: Color::Yellow,
            changed_bg: Color::Rgb(222, 229, 238),
            changed_old_fg: Color::Rgb(34, 42, 52),
            changed_new_fg: Color::Rgb(20, 28, 38),
            inline_fg: Color::Rgb(16, 20, 26),
            alignment_hues: [
                AlignmentHue {
                    old_bg: Color::Rgb(214, 172, 92),
                    new_bg: Color::Rgb(245, 191, 70),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(104, 185, 203),
                    new_bg: Color::Rgb(69, 210, 235),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(167, 139, 222),
                    new_bg: Color::Rgb(197, 168, 255),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(194, 174, 99),
                    new_bg: Color::Rgb(230, 205, 85),
                },
            ],
        }
    }

    fn light_terminal() -> Self {
        Self {
            text: Color::Rgb(67, 74, 84),
            header: Color::Rgb(32, 38, 46),
            separator: Color::Rgb(136, 146, 160),
            gutter_fg: Color::Rgb(116, 126, 140),
            warning_fg: Color::Rgb(161, 98, 7),
            changed_bg: Color::Rgb(31, 38, 48),
            changed_old_fg: Color::Rgb(218, 224, 233),
            changed_new_fg: Color::Rgb(240, 244, 248),
            inline_fg: Color::Rgb(249, 250, 251),
            alignment_hues: [
                AlignmentHue {
                    old_bg: Color::Rgb(103, 74, 31),
                    new_bg: Color::Rgb(160, 108, 24),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(32, 83, 96),
                    new_bg: Color::Rgb(21, 132, 155),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(70, 55, 118),
                    new_bg: Color::Rgb(111, 83, 184),
                },
                AlignmentHue {
                    old_bg: Color::Rgb(82, 70, 37),
                    new_bg: Color::Rgb(134, 112, 42),
                },
            ],
        }
    }
}

fn alignment_hue(alignment: InlineAlignmentId, palette: &Palette) -> AlignmentHue {
    // Inline replacements use one hue per alignment. Old/new role is encoded
    // by intensity, not by red-vs-green meaning.
    palette.alignment_hues[alignment.0 % palette.alignment_hues.len()]
}

#[derive(Clone, Debug)]
struct SplitGeometry {
    left_width: u16,
    left_content: ContentColumn,
    separator_x: u16,
    right_x: u16,
    right_width: u16,
    right_content: ContentColumn,
}

fn split_geometry(area: Rect, rows: &[SplitRow]) -> SplitGeometry {
    let left_width = area.width.saturating_sub(1) / 2;
    let separator_x = area.x + left_width;
    let right_x = separator_x + 1;
    let right_width = area.width.saturating_sub(left_width + 1);
    let left_content = ContentColumn::right_aligned(
        left_width,
        content_width(left_width, rows.iter().map(|row| row.left.width()), 0),
    );
    let right_content = ContentColumn::left_aligned(
        right_width,
        content_width(right_width, rows.iter().map(|row| row.right.width()), 0),
    );

    SplitGeometry {
        left_width,
        left_content,
        separator_x,
        right_x,
        right_width,
        right_content,
    }
}

#[derive(Clone, Debug)]
struct StackedGeometry {
    content: ContentColumn,
}

fn stacked_geometry(area: Rect, state: &DiffState, rows: &[UnifiedRow]) -> StackedGeometry {
    let header_width = stacked_header_width(state);
    let min_width = usize::from(MIN_STACKED_CONTENT_WIDTH).max(header_width);
    StackedGeometry {
        content: ContentColumn::centered(
            area.width,
            content_width(
                area.width,
                rows.iter().map(|row| row.line.width()),
                min_width,
            ),
        ),
    }
}

#[derive(Clone, Debug)]
struct TimelineGeometry {
    content: ContentColumn,
}

fn timeline_geometry(area: Rect, rows: &[TimelineRenderRow]) -> TimelineGeometry {
    TimelineGeometry {
        content: ContentColumn::centered(
            area.width,
            content_width(
                area.width,
                rows.iter().map(|row| row.line.width()),
                usize::from(MIN_TIMELINE_CONTENT_WIDTH),
            ),
        ),
    }
}

#[derive(Clone, Debug)]
struct ContentColumn {
    pane_width: u16,
    content_x: u16,
    content_width: u16,
}

impl ContentColumn {
    fn left_aligned(pane_width: u16, content_width: u16) -> Self {
        Self {
            pane_width,
            content_x: 0,
            content_width: content_width.min(pane_width),
        }
    }

    fn right_aligned(pane_width: u16, content_width: u16) -> Self {
        let content_width = content_width.min(pane_width);
        Self {
            pane_width,
            content_x: pane_width.saturating_sub(content_width),
            content_width,
        }
    }

    fn centered(pane_width: u16, content_width: u16) -> Self {
        let content_width = content_width.min(pane_width);
        Self {
            pane_width,
            content_x: pane_width.saturating_sub(content_width) / 2,
            content_width,
        }
    }

    fn centered_line(&self, line: Line<'static>, style: Style) -> Line<'static> {
        center_line_in_region(
            line,
            self.pane_width,
            self.content_x,
            self.content_width,
            style,
        )
    }

    fn positioned_line(&self, mut line: Line<'static>, style: Style) -> Line<'static> {
        if self.content_x > 0 {
            line.spans
                .insert(0, Span::styled(" ".repeat(self.content_x as usize), style));
        }
        pad_line(line, self.pane_width, style)
    }
}

fn content_width(pane_width: u16, widths: impl Iterator<Item = usize>, min_width: usize) -> u16 {
    let mut widths = widths.collect::<Vec<_>>();
    if widths.is_empty() {
        return pane_width;
    }

    widths.sort_unstable();
    let p90_index = (widths.len() * 9).div_ceil(10).saturating_sub(1);
    let width = widths[p90_index].saturating_add(2).max(min_width);
    let width = width.min(usize::from(pane_width));
    width as u16
}

fn stacked_header_width(state: &DiffState) -> usize {
    text_width(&stacked_header_text(state))
}

fn stacked_header_text(state: &DiffState) -> String {
    format!(
        "{} | {}",
        signed_origin_label(state, SideId(0), '-'),
        signed_origin_label(state, SideId(1), '+')
    )
}

fn stacked_header_line(state: &DiffState, palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            signed_origin_label(state, SideId(0), '-'),
            header_style(palette),
        ),
        Span::styled(" | ", separator_style(palette)),
        Span::styled(
            signed_origin_label(state, SideId(1), '+'),
            header_style(palette),
        ),
    ])
}

fn signed_origin_label(state: &DiffState, side: SideId, sign: char) -> String {
    format!("{sign}{}", origin_text(state, side))
}

fn origin_text(state: &DiffState, side: SideId) -> String {
    let side = state.side(side);
    let doc = side.and_then(|side| state.snapshot(side.snapshot));
    let path = side
        .and_then(|side| side.label.as_deref())
        .or_else(|| doc.and_then(|doc| doc.uri.as_deref()))
        .map(|label| label.rsplit_once('/').map_or(label, |(_, path)| path))
        .unwrap_or("<memory>");
    let origin = side
        .and_then(|side| side.origin.as_deref())
        .unwrap_or("git:unknown");
    format!("{path} @ {origin}")
}

fn header_style(palette: &Palette) -> Style {
    Style::default()
        .fg(palette.header)
        .add_modifier(Modifier::BOLD)
}

fn separator_style(palette: &Palette) -> Style {
    Style::default().fg(palette.separator)
}

fn separator_span(palette: &Palette) -> Span<'static> {
    Span::styled("│", separator_style(palette))
}

fn separator_span_for(kind: BlockKind, palette: &Palette) -> Span<'static> {
    Span::styled("│", separator_style(palette).bg(row_fill_bg(kind, palette)))
}

fn center_line_in_region(
    mut line: Line<'static>,
    pane_width: u16,
    region_x: u16,
    region_width: u16,
    style: Style,
) -> Line<'static> {
    let pane_width = usize::from(pane_width);
    let region_x = usize::from(region_x).min(pane_width);
    let region_width = usize::from(region_width).min(pane_width.saturating_sub(region_x));
    let line_width = line.width();
    let region_mid = region_x + region_width / 2;
    let mut left = region_mid.saturating_sub(line_width / 2);

    if line_width < pane_width {
        left = left.min(pane_width - line_width);
    } else {
        left = 0;
    }

    let right = pane_width.saturating_sub(left + line_width);
    if left > 0 {
        line.spans.insert(0, Span::styled(" ".repeat(left), style));
    }
    if right > 0 {
        line.spans.push(Span::styled(" ".repeat(right), style));
    }

    line
}

fn soft_wrap_line(line: Line<'static>, width: u16, fill_style: Style) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if width == 0 || line.width() <= width {
        return vec![line];
    }

    let prefix_width = continuation_prefix_width(&line, width);
    let chars = styled_chars(line);
    let mut rows = Vec::new();
    let mut remaining = chars.as_slice();
    let mut is_first = true;

    while !remaining.is_empty() {
        let capacity = if is_first {
            width
        } else {
            width.saturating_sub(prefix_width).max(1)
        };
        let count = take_display_width(remaining, capacity);
        let (current, rest) = remaining.split_at(count);
        let mut line = Line::from(styled_spans(current));
        if !is_first && prefix_width > 0 {
            line.spans
                .insert(0, Span::styled(" ".repeat(prefix_width), fill_style));
        }
        rows.push(line);
        remaining = rest;
        is_first = false;
    }

    rows
}

fn continuation_prefix_width(line: &Line<'_>, row_width: usize) -> usize {
    let gutter_width = line
        .spans
        .first()
        .map(|span| text_width(span.content.as_ref()))
        .unwrap_or_default();
    let source_indent = source_indent_width(line);
    let desired = gutter_width + source_indent + 2;
    let max_prefix = row_width.saturating_sub(8).max(row_width / 3);
    desired.min(max_prefix)
}

fn source_indent_width(line: &Line<'_>) -> usize {
    let mut width = 0;
    for span in line.spans.iter().skip(1) {
        for ch in span.content.chars() {
            match ch {
                ' ' => width += 1,
                '\t' => width += 4,
                _ => return width,
            }
        }
    }
    width
}

#[derive(Clone, Copy, Debug)]
struct StyledChar {
    ch: char,
    style: Style,
    width: usize,
}

fn styled_chars(line: Line<'static>) -> Vec<StyledChar> {
    let mut chars = Vec::new();
    for span in line.spans {
        let style = span.style;
        for ch in span.content.chars() {
            chars.push(StyledChar {
                ch,
                style,
                width: char_width(ch),
            });
        }
    }
    chars
}

fn take_display_width(chars: &[StyledChar], capacity: usize) -> usize {
    let mut width = 0;
    for (index, ch) in chars.iter().enumerate() {
        if index > 0 && width + ch.width > capacity {
            return index;
        }
        width += ch.width;
        if width >= capacity {
            return index + 1;
        }
    }
    chars.len()
}

fn styled_spans(chars: &[StyledChar]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let Some(first) = chars.first() else {
        return spans;
    };

    let mut text = String::new();
    let mut style = first.style;
    for ch in chars {
        if ch.style != style {
            spans.push(Span::styled(text, style));
            text = String::new();
            style = ch.style;
        }
        text.push(ch.ch);
    }
    spans.push(Span::styled(text, style));
    spans
}

fn char_width(ch: char) -> usize {
    text_width(&ch.to_string())
}

fn text_width(text: &str) -> usize {
    Line::from(text.to_owned()).width()
}

fn empty_line() -> Line<'static> {
    Line::from(Vec::<Span<'static>>::new())
}

fn pad_line(mut line: Line<'static>, width: u16, style: Style) -> Line<'static> {
    let padding = usize::from(width).saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(Span::styled(" ".repeat(padding), style));
    }
    line
}

fn row_fill_style(kind: BlockKind, palette: &Palette) -> Style {
    Style::default().bg(row_fill_bg(kind, palette))
}

fn row_fill_bg(kind: BlockKind, palette: &Palette) -> Color {
    if kind.is_changed() {
        palette.changed_bg
    } else {
        Color::Reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn inline_change_scene_uses_real_diff_state() {
        let state = inline_change_state();
        mig::diff_model::assert_diff_state_invariants(&state);
        assert_eq!(state.graph.snapshots.len(), 2);
        assert_eq!(state.graph.sides.len(), 2);
        assert_eq!(state.graph.blocks.len(), 3);
        assert_eq!(state.graph.inline.alignments.len(), 1);
        assert_eq!(state.graph.anchors.anchors.len(), 1);
    }

    #[test]
    fn whole_function_scene_highlights_function_in_split_layout() {
        let state = whole_function_state();
        mig::diff_model::assert_diff_state_invariants(&state);
        assert_eq!(state.graph.blocks[0].kind, BlockKind::Replace);
        assert_eq!(state.graph.blocks[0].sides[0].as_ref().unwrap().range, 0..4);
        assert_eq!(state.graph.blocks[0].sides[1].as_ref().unwrap().range, 0..5);
        assert_eq!(resolved_layout(&state), ResolvedLayout::Split);
    }

    #[test]
    fn move_without_identity_scene_uses_stack_layout_for_separated_insert_delete() {
        let state = move_without_identity_state();
        mig::diff_model::assert_diff_state_invariants(&state);
        assert_eq!(state.graph.blocks[1].kind, BlockKind::Delete);
        assert_eq!(state.graph.blocks[3].kind, BlockKind::Insert);
        assert_eq!(resolved_layout(&state), ResolvedLayout::Stacked);
    }

    #[test]
    fn auto_layout_splits_touching_old_new_lines() {
        let state = inline_change_state();
        assert_eq!(resolved_layout(&state), ResolvedLayout::Split);
    }

    #[test]
    fn auto_layout_stacks_one_sided_changes() {
        let mut state = inline_change_state();
        state.graph.blocks[1].kind = BlockKind::Insert;
        state.graph.blocks[1].sides[0] = None;
        assert_eq!(resolved_layout(&state), ResolvedLayout::Stacked);
    }

    #[test]
    fn split_geometry_pulls_left_content_toward_separator() {
        let state = inline_change_state();
        let rows = render_split_rows(&state, &Palette::dark_terminal());
        let geometry = split_geometry(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 24,
            },
            &rows,
        );
        assert_eq!(geometry.separator_x, 59);
        assert!(geometry.left_content.content_width < geometry.left_width);
        assert!(geometry.left_content.content_x > 0);
        assert!(geometry.right_content.content_width < geometry.right_width);
        assert_eq!(geometry.right_content.content_x, 0);
    }

    #[test]
    fn stacked_geometry_centers_content_column() {
        let state = move_without_identity_state();
        let rows = render_unified_rows(&state, &Palette::dark_terminal());
        let geometry = stacked_geometry(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 24,
            },
            &state,
            &rows,
        );
        assert!(geometry.content.content_width < geometry.content.pane_width);
        assert!(geometry.content.content_width >= MIN_STACKED_CONTENT_WIDTH);
        assert!(geometry.content.content_x > 0);
        assert_eq!(
            soft_wrap_line(
                rows[0].line.clone(),
                geometry.content.content_width,
                Style::default()
            )
            .len(),
            1
        );
    }

    #[test]
    fn timeline_geometry_centers_content_column() {
        let app = AppState::timeline_fixture(TuiTheme::Dark);
        let rows = timeline_rows(&app, &Palette::dark_terminal());
        let geometry = timeline_geometry(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 24,
            },
            &rows,
        );
        assert!(geometry.content.content_width < geometry.content.pane_width);
        assert!(geometry.content.content_width >= MIN_TIMELINE_CONTENT_WIDTH);
        assert!(geometry.content.content_x > 0);
    }

    #[test]
    fn soft_wrap_line_preserves_basic_continuation_indent() {
        let style = Style::default().bg(Color::Blue);
        let line = Line::from(vec![
            Span::styled("  42 ", style),
            Span::styled("    let value = alpha_beta_gamma_delta;", style),
        ]);

        let rows = soft_wrap_line(line, 24, style);

        assert!(rows.len() > 1);
        assert!(rows.iter().all(|row| row.width() <= 24));
        assert!(line_text(&rows[1]).starts_with("           "));
    }

    #[test]
    fn center_line_in_region_uses_content_column() {
        let line = center_line_in_region(Line::from("title"), 12, 8, 4, Style::default());
        assert_eq!(line.width(), 12);
        assert_eq!(line_text(&line), "       title");
    }

    #[test]
    fn timeline_fixture_starts_in_timeline_place() {
        let app = AppState::timeline_fixture(TuiTheme::Dark);
        assert_eq!(app.mode, AppMode::Timeline);
        assert_eq!(app.timeline.entries.len(), 5);
        assert_eq!(app.cursor.index, 1);
        assert!(app.opened.is_none());
    }

    #[test]
    fn selected_backed_timeline_identity_uses_selected_foreground() {
        let app = AppState::timeline_fixture(TuiTheme::Dark);
        let palette = Palette::dark_terminal();
        let entry = &app.timeline.entries[0];

        let line = timeline_entry_line(entry, 0, true, false, &palette);

        assert_eq!(line.spans[2].style.fg, Some(palette.changed_new_fg));
    }

    #[test]
    fn timeline_enter_opens_cursor_and_escape_returns() {
        let mut app = AppState::timeline_fixture(TuiTheme::Dark);
        handle_key(&mut app, KeyCode::Down);
        let id = app.cursor_id().unwrap();

        handle_key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::Comparison);
        assert_eq!(app.opened, Some(OpenedComparison::Cursor(id)));

        handle_key(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Timeline);
        assert_eq!(app.cursor_id(), Some(id));
    }

    #[test]
    fn q_and_escape_leave_comparison_before_exiting() {
        let mut app = AppState::timeline_fixture(TuiTheme::Dark);
        handle_key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::Comparison);

        assert_eq!(
            handle_key(&mut app, KeyCode::Char('q')),
            KeyOutcome::Continue
        );
        assert_eq!(app.mode, AppMode::Timeline);
        assert_eq!(handle_key(&mut app, KeyCode::Char('q')), KeyOutcome::Exit);

        let mut app = AppState::timeline_fixture(TuiTheme::Dark);
        handle_key(&mut app, KeyCode::Enter);
        assert_eq!(handle_key(&mut app, KeyCode::Esc), KeyOutcome::Continue);
        assert_eq!(app.mode, AppMode::Timeline);
        assert_eq!(handle_key(&mut app, KeyCode::Esc), KeyOutcome::Exit);
    }

    #[test]
    fn timeline_marks_open_marked_comparison() {
        let mut app = AppState::timeline_fixture(TuiTheme::Dark);
        let first = app.cursor_id().unwrap();
        handle_key(&mut app, KeyCode::Char(' '));
        assert_ne!(app.cursor_id(), Some(first));

        let second = app.cursor_id().unwrap();
        handle_key(&mut app, KeyCode::Char(' '));

        handle_key(&mut app, KeyCode::Enter);

        assert_eq!(app.mode, AppMode::Comparison);
        assert_eq!(
            app.opened,
            Some(OpenedComparison::Marked(vec![first, second]))
        );
        handle_key(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Timeline);
        assert_eq!(app.marks.len(), 2);
    }

    #[test]
    fn space_marking_stops_at_last_timeline_entry() {
        let mut app = AppState::timeline_fixture(TuiTheme::Dark);
        app.cursor.index = app.timeline.entries.len() - 1;
        let last = app.cursor_id().unwrap();

        handle_key(&mut app, KeyCode::Char(' '));

        assert_eq!(app.cursor_id(), Some(last));
        assert!(app.marks.contains(&last));
    }

    #[test]
    fn timeline_scene_renders_to_ratatui() -> Result<()> {
        let app = AppState::timeline_fixture(TuiTheme::Dark);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("entry 2 / 5"));
        assert!(buffer.contains("HEAD~2"));
        assert!(buffer.contains("src/tui.rs+1-1"));
        assert!(buffer.contains("rename inline token wording"));
        assert!(!buffer.contains("Mig Timeline"));
        assert!(!buffer.contains("L1 git"));
        assert!(!buffer.contains("L2 mig"));
        Ok(())
    }

    #[test]
    fn inline_change_scene_renders_to_ratatui() -> Result<()> {
        let state = inline_change_state();
        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("left.rs @ git:HEAD~1"));
        assert!(buffer.contains("right.rs @ git:worktree"));
        Ok(())
    }

    #[test]
    fn inline_change_scene_can_render_stacked_projection() -> Result<()> {
        let mut state = inline_change_state();
        state.view.viewport.layout = DiffLayout::Stacked;
        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("-left.rs @ git:HEAD~1 | +right.rs @ git:worktree"));
        assert!(buffer.contains("-   2"));
        assert!(buffer.contains("+   2"));
        Ok(())
    }

    #[test]
    fn whole_function_scene_renders_to_ratatui() -> Result<()> {
        let state = whole_function_state();
        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("summarize_turn"));
        assert!(buffer.contains("event_count"));
        Ok(())
    }

    #[test]
    fn unicode_inline_ranges_render_without_panic() -> Result<()> {
        let state = mig::diff_engine::text_diff_state(mig::diff_engine::TextDiffInput {
            label: "unicode.rs",
            before_origin: "before".to_owned(),
            after_origin: "after".to_owned(),
            before: "let marker = \"🔥\";\n",
            after: "let marker = \"✨\";\n",
        });
        mig::diff_model::assert_diff_state_invariants(&state);

        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        Ok(())
    }

    #[test]
    fn move_without_identity_scene_renders_stacked_markers() -> Result<()> {
        let state = move_without_identity_state();
        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("-   2"));
        assert!(buffer.contains("+   8"));
        Ok(())
    }

    #[test]
    fn inline_change_scene_renders_resize_hint_when_too_small() -> Result<()> {
        let state = inline_change_state();
        let app = AppState::comparison(TuiTheme::Dark, state);
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, &app))?;
        let buffer = terminal.backend().to_string();
        assert!(buffer.contains("Mig needs more room"));
        assert!(buffer.contains("60x20"));
        Ok(())
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}

use super::*;
use crate::{
    diff::{DisplaySpan, Hunk, LineCoverage, diff_file},
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
    assert!(screen.contains(" 24 │"));
    assert!(screen.contains("fn should_refresh"));
    assert!(screen.contains("Stale and legacy profiles need refreshing"));
    assert!(screen.contains("profile.schema < 4 || age > Duration::from_secs(300)"));
    assert!(screen.contains("fn display_label"));
    assert!(screen.contains(".to_owned().replace('\\n', \" \")"));
    assert!(screen.contains("16 → 38 │ fn cache_key"));
    assert!(screen.contains("⋮ │ ⋮"));
    assert!(!screen.contains("session.tenant()"));
    assert!(!screen.contains("key.push"), "{screen}");
    assert!(screen.contains("43 │ }"));
    assert_eq!(screen.matches("fn cache_key").count(), 1);
    assert!(screen.contains("legacy_counter → {Metric, ReviewMeter}"));
    assert!(screen.contains("25 │ fn render_response(profile: &Profile) -> Response {"));
    assert!(screen.contains("~ 26 │     Response::new(StatusCode::OK, profile.display_name())"));
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
    let definition = lines
        .iter()
        .position(|line| line.contains("fn load_profile"))
        .expect("definition must be rendered");
    assert!(definition < move_end);
    assert!(move_end < import);

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
            cell.fg == Palette::GHOST
                && !cell.modifier.contains(Modifier::DIM)
                && !cell.modifier.contains(Modifier::ITALIC)
        }
    ));
    assert!(run_on_line_matches(
        buffer,
        "already trusted",
        "26 │",
        |cell| cell.fg == Palette::GHOST && !cell.modifier.contains(Modifier::DIM)
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
            cell.fg == softened_syntax_foreground(SyntaxClass::Type)
                && !cell.modifier.contains(Modifier::DIM)
        }
    ));
    assert!(run_on_line_matches(
        buffer,
        "cached.and_then",
        ".and_then(validate_profile)",
        |cell| { cell.fg == Palette::CURRENT && cell.modifier.contains(Modifier::BOLD) }
    ));
    for (line, removed) in [
        ("already trusted", "already"),
        ("legacy_counter → {Metric, ReviewMeter}", "legacy_counter"),
    ] {
        assert!(run_on_line_matches(buffer, line, removed, |cell| {
            cell.fg == Palette::GHOST_EMPHASIS && cell.modifier.contains(Modifier::BOLD)
        }));
    }
    assert!(run_on_line_matches(
        buffer,
        "legacy_counter → {Metric, ReviewMeter}",
        "{Metric, ReviewMeter}",
        |cell| { cell.fg == Palette::CURRENT && cell.modifier.contains(Modifier::BOLD) }
    ));
    for (line, marker, foreground) in [
        ("already trusted", "- ", Palette::GHOST),
        ("must be revalidated", "+ ", Palette::CURRENT),
        ("Response::new", "~ ", Palette::FAINT),
    ] {
        assert!(run_on_line_matches(buffer, line, marker, |cell| {
            cell.fg == foreground && !cell.modifier.contains(Modifier::BOLD)
        }));
    }
    assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
}

#[test]
fn ghost_lines_never_use_syntax_foregrounds_or_backgrounds() {
    let classes = [
        SyntaxClass::Plain,
        SyntaxClass::Keyword,
        SyntaxClass::Identifier,
        SyntaxClass::Type,
        SyntaxClass::Literal,
        SyntaxClass::String,
        SyntaxClass::Comment,
        SyntaxClass::Punctuation,
    ];
    let mut spans = Vec::new();
    for (index, syntax) in classes.into_iter().enumerate() {
        spans.push(DisplaySpan {
            text: format!("context{index} "),
            syntax,
            mark: DiffMark::Context,
        });
        spans.push(DisplaySpan {
            text: format!("removed{index} "),
            syntax,
            mark: DiffMark::Removed,
        });
    }
    let line = DisplayLine { number: 7, spans };

    let row = source_line(
        &line,
        Some(SourceMarker::Removed),
        GutterLayout { label_columns: 3 },
        400,
        false,
    );

    let syntax_foregrounds = [
        Palette::TEXT,
        Palette::KEYWORD,
        Palette::TYPE,
        Palette::LITERAL,
        Palette::STRING,
        Palette::COMMENT,
    ];
    let body = &row.spans[3..];
    assert_eq!(body.len(), classes.len() * 2);
    assert_eq!(row.spans[1].style.fg, Some(Palette::GHOST));
    assert_eq!(row.spans[2].style.fg, Some(Palette::GHOST));
    assert!(row.spans.iter().all(|span| span.style.bg.is_none()));

    for (index, span) in body.iter().enumerate() {
        let changed = index % 2 == 1;
        let expected = if changed {
            Palette::GHOST_EMPHASIS
        } else {
            Palette::GHOST
        };
        assert_eq!(span.style.fg, Some(expected));
        assert!(!syntax_foregrounds.contains(&expected));
        assert_eq!(span.style.add_modifier.contains(Modifier::BOLD), changed);
        assert!(!span.style.add_modifier.contains(Modifier::ITALIC));
    }
}

#[test]
fn multiline_replacement_renders_old_block_before_current_block() {
    let diff = diff_file(
        "notes.txt",
        "header\nthis\nwent\naway\ntail\n",
        "header\nand then\nthis came in\nno meatgrinder\ntail\n",
    )
    .expect("plain replacement");
    let mut app = App::new(vec![diff]);
    let backend = TestBackend::new(100, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal");

    terminal
        .draw(|frame| render(frame, &mut app))
        .expect("render replacement");

    let screen = buffer_text(terminal.backend().buffer());
    let positions = [
        "- 2 │ this",
        "- 3 │ went",
        "- 4 │ away",
        "+ 2 │ and then",
        "+ 3 │ this came in",
        "+ 4 │ no meatgrinder",
    ]
    .map(|text| {
        screen
            .find(text)
            .unwrap_or_else(|| panic!("missing {text:?}"))
    });

    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{screen}"
    );
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
    let line = |number| DisplayLine {
        number,
        spans: vec![DisplaySpan {
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
            rows: vec![
                DiffRow::Line(line(2)),
                DiffRow::LineChange {
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
    let line = DisplayLine {
        number: 1,
        spans: vec![
            DisplaySpan {
                text: "ab".to_owned(),
                syntax: SyntaxClass::Plain,
                mark: DiffMark::Context,
            },
            DisplaySpan {
                text: "\tvalue".to_owned(),
                syntax: SyntaxClass::Keyword,
                mark: DiffMark::Added,
            },
        ],
    };

    let row = source_line(&line, None, GutterLayout { label_columns: 1 }, 80, false);

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
        header.contains("src/first.rs \u{2502} notes.txt \u{2502} build/generated.rs @generated"),
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
fn line_dense_review_explains_the_projection_limit() {
    let notice = FileNotice::too_many_lines("generated/dense.txt", Some(1), Some(100_001), 100_000);
    let mut app = App::new(vec![FileReview::Notice(notice)]);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");

    terminal
        .draw(|frame| render(frame, &mut app))
        .expect("render line-dense notice");

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("exceeds the 100000-line per-revision limit"));
    assert!(screen.contains("before 1 lines  ·  current 100001 lines"));
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

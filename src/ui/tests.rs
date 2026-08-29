use super::*;
use crate::diff::{
    DiffMark, LineCoverage, PresentedFile, ReviewHunk, ReviewRow, SourceRow, SourceSpan,
};

#[test]
fn gutter_width_follows_the_widest_presented_label() {
    let line = |number| SourceRow {
        number,
        spans: vec![SourceSpan {
            text: "alpha".to_owned(),
            syntax: SyntaxClass::Plain,
            mark: DiffMark::Context,
        }],
    };
    let diff = PresentedFile {
        path: "alpha.rs".to_owned(),
        generated: false,
        hunks: vec![ReviewHunk {
            coverage: LineCoverage {
                before: Some(1..2),
                after: Some(1..2),
            },
            rows: vec![
                ReviewRow::Current(line(2)),
                ReviewRow::Removed(line(123_456)),
            ],
        }],
    };
    let gutter = GutterLayout::new(&diff);

    assert_eq!(gutter.label_columns, UnicodeWidthStr::width("- 123456"));
    assert_eq!(gutter.padding("2"), " ".repeat(7));
    assert_eq!(gutter.width(), gutter.label_columns + 3);
}

#[test]
fn scrolling_clamps_to_the_current_viewport() {
    let mut app = App::new(vec![review("alpha.rs")]);
    app.total_rows = 23;
    app.viewport_rows = 8;

    handle_key(&mut app, key(KeyCode::End));
    assert_eq!(app.scroll, 15);

    handle_key(&mut app, key(KeyCode::Down));
    assert_eq!(app.scroll, 15);

    handle_key(&mut app, key(KeyCode::Home));
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(app.scroll, 0);

    handle_key(&mut app, key(KeyCode::PageDown));
    assert_eq!(app.scroll, 8);
    handle_key(&mut app, key(KeyCode::PageUp));
    assert_eq!(app.scroll, 0);

    app.viewport_rows = 20;
    app.scroll = 15;
    app.clamp_scroll();
    assert_eq!(app.scroll, 3);
}

#[test]
fn file_navigation_clamps_and_resets_scroll() {
    let mut app = App::new(vec![review("alpha.rs"), review("beta.rs")]);
    app.scroll = 4;

    handle_key(&mut app, key(KeyCode::Right));
    assert_eq!(app.current_diff().path, "beta.rs");
    assert_eq!(app.scroll, 0);

    app.scroll = 3;
    handle_key(&mut app, key(KeyCode::Right));
    assert_eq!(app.current_diff().path, "beta.rs");
    assert_eq!(app.scroll, 3);

    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.current_diff().path, "alpha.rs");
    assert_eq!(app.scroll, 0);

    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.current_diff().path, "alpha.rs");
}

fn review(path: &str) -> PresentedFile {
    crate::diff::diff_file(path, "fn alpha() {}\n", "fn beta() {}\n").expect("metasyntactic review")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

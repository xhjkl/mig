#![cfg(feature = "fixture-bins")]

use std::fs;
#[cfg(feature = "stress")]
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn dump(extension: &str, before: &str, after: &str) -> String {
    let directory = TempDir::new().expect("source pair");
    let before_path = directory.path().join(format!("before.{extension}"));
    let after_path = directory.path().join(format!("after.{extension}"));
    fs::write(&before_path, before).expect("write before source");
    fs::write(&after_path, after).expect("write after source");
    let output = Command::new(env!("CARGO_BIN_EXE_d"))
        .args([before_path, after_path])
        .output()
        .expect("run dump CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 dump")
}

#[test]
fn semantic_changes_are_visible_through_the_cli() {
    for (extension, before, after) in [
        (
            "c",
            "int f(int x, int y) {\n return x + ++y;\n}\n",
            "int f(int x, int y) {\n return x++ + y;\n}\n",
        ),
        (
            "rs",
            "fn f() {\n m!(foo bar);\n}\n",
            "fn f() {\n m!(foobar);\n}\n",
        ),
        (
            "rs",
            "fn f() {\n let s = \"a\nb\";\n}\n",
            "fn f() {\n let s = \"ab\";\n}\n",
        ),
    ] {
        let output = dump(extension, before, after);
        assert!(output.contains("[Edit"), "{output}");
        assert!(!output.contains("[Reflow"), "{output}");
        assert!(output.lines().any(|line| line.starts_with('-')), "{output}");
        assert!(output.lines().any(|line| line.starts_with('+')), "{output}");
    }
}

#[test]
fn reflow_keeps_a_final_newline_change_visible() {
    let output = dump("rs", "fn f() { call(); }", "fn f() {\n call();\n}\n");
    assert!(
        output.contains("line 1: no newline → line 3: LF"),
        "{output}"
    );
}

#[test]
fn deeply_nested_input_stays_reviewable_on_the_cli_stack() {
    let before = format!("{}0{}\n", "[".repeat(10_000), "]".repeat(10_000));
    let after = before.replacen('0', "1", 1);
    let output = dump("json", &before, &after);
    assert!(output.contains("plain lines"), "{output}");
    assert!(output.contains("[Edit"), "{output}");
}

#[cfg(feature = "stress")]
fn write_case(directory: &Path, extension: &str, before: &str, after: &str) {
    fs::create_dir_all(directory).expect("case directory");
    fs::write(
        directory.join("meta.txt"),
        format!("commit: contract\npath: case.{extension}\n"),
    )
    .expect("case metadata");
    fs::write(directory.join(format!("before.{extension}")), before).expect("before revision");
    fs::write(directory.join(format!("after.{extension}")), after).expect("after revision");
}

#[cfg(feature = "stress")]
#[test]
fn stress_replays_cases_in_child_processes() {
    let directory = TempDir::new().expect("replay fixture");
    let cases = directory.path().join("cases");
    write_case(
        &cases.join("macro"),
        "rs",
        "fn f() { m!(foo bar); }\n",
        "fn f() { m!(foobar); }\n",
    );
    write_case(
        &cases.join("reflow"),
        "rs",
        "fn f() { call(); }",
        "fn f() {\n call();\n}\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_stress"))
        .arg(&cases)
        .args(["--cases", "--threads", "2", "--out"])
        .arg(directory.path().join("out"))
        .output()
        .expect("replay stress cases");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("files with defects: 0 of 2"));
}

#[cfg(feature = "stress")]
#[test]
fn stress_audits_the_web_language_fixtures() {
    let directory = TempDir::new().expect("web replay fixture");
    let cases = directory.path().join("cases");
    for (index, fixture) in mig_core::fixture::web::ALL.iter().enumerate() {
        let extension = Path::new(fixture.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .expect("fixture language");
        write_case(
            &cases.join(index.to_string()),
            extension,
            fixture.before,
            fixture.after,
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_stress"))
        .arg(&cases)
        .args(["--cases", "--threads", "2", "--out"])
        .arg(directory.path().join("out"))
        .output()
        .expect("audit web fixtures");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "stress")]
#[test]
fn stress_defects_fail_the_command() {
    let directory = TempDir::new().expect("replay fixture");
    let cases = directory.path().join("case");
    write_case(&cases, "txt", "before\n", &"\n".repeat(100_001));
    let output = Command::new(env!("CARGO_BIN_EXE_stress"))
        .arg(&cases)
        .args(["--cases", "--out"])
        .arg(directory.path().join("out"))
        .output()
        .expect("replay invalid source");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("files with defects: 1 of 1"));
}

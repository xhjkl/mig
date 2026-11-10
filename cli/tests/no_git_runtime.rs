use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn clean_scan_does_not_require_a_git_executable() {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["config", "user.name", "Alpha"]);
    git(
        repository.path(),
        &["config", "user.email", "alpha@example.invalid"],
    );
    fs::write(repository.path().join("tracked.txt"), "stable\n").expect("write tracked file");
    git(repository.path(), &["add", "tracked.txt"]);
    git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

    let output = Command::new(env!("CARGO_BIN_EXE_m"))
        .current_dir(repository.path())
        .env("PATH", "")
        .output()
        .expect("run installed-style m without Git on PATH");

    assert!(
        output.status.success(),
        "m failed without Git on PATH: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn commitish_resolution_does_not_require_a_git_executable() {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["config", "user.name", "Alpha"]);
    git(
        repository.path(),
        &["config", "user.email", "alpha@example.invalid"],
    );
    git(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "empty"],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_m"))
        .arg("HEAD")
        .current_dir(repository.path())
        .env("PATH", "")
        .output()
        .expect("run commit review without Git on PATH");

    assert!(
        output.status.success(),
        "m failed without Git on PATH: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("execute Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

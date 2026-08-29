use super::*;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn commit_review_uses_pinned_trees_and_keeps_generated_files_last() {
    let repository = initialized_repository();
    write(
        repository.path(),
        "changed.rs",
        "fn answer() -> &'static str { \"baseline\" }\n",
    );
    write(repository.path(), "old-name.rs", "fn renamed() {}\n");
    write(repository.path(), "removed.txt", "removed\n");
    write(
        repository.path(),
        "a-generated.rs",
        "// @generated\nfn generated() -> u8 { 1 }\n",
    );
    write_bytes(repository.path(), "binary.dat", b"old\0binary\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

    write(repository.path(), "added.txt", "added\n");
    write(
        repository.path(),
        "changed.rs",
        "fn answer() -> &'static str { \"selected\" }\n",
    );
    git(repository.path(), &["mv", "old-name.rs", "new-name.rs"]);
    fs::remove_file(repository.path().join("removed.txt")).expect("remove tracked input");
    write(
        repository.path(),
        "a-generated.rs",
        "// @generated\nfn generated() -> u8 { 2 }\n",
    );
    write_bytes(repository.path(), "binary.dat", b"new\0binary\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "change"]);

    write(
        repository.path(),
        "changed.rs",
        "fn answer() -> &'static str { \"later\" }\n",
    );
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "later"]);
    write(
        repository.path(),
        "changed.rs",
        "fn answer() -> &'static str { \"worktree\" }\n",
    );
    fs::create_dir(repository.path().join("nested")).expect("create nested invocation path");

    let reviews =
        diff_commit(&repository.path().join("nested"), Path::new("HEAD~1")).expect("review commit");
    let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

    assert_eq!(
        paths,
        vec![
            "added.txt",
            "changed.rs",
            "new-name.rs",
            "old-name.rs",
            "removed.txt",
            "a-generated.rs"
        ]
    );
    let changed = reviews
        .iter()
        .find(|review| review.path() == "changed.rs")
        .expect("changed file review");
    let rendered = format!("{changed:?}");
    assert!(rendered.contains("answer"));
    assert!(rendered.contains("selected"));
    assert!(!rendered.contains("later"));
    assert!(!rendered.contains("worktree"));

    let regex_reviews = diff_commit(&repository.path().join("nested"), Path::new(":/ch.nge"))
        .expect("review commit selected by message regex");
    let regex_paths = regex_reviews
        .iter()
        .map(FileReview::path)
        .collect::<Vec<_>>();
    assert_eq!(regex_paths, paths);
}

#[test]
fn root_commit_and_annotated_tag_are_reviewed_against_the_empty_tree() {
    let repository = initialized_repository();
    write(repository.path(), "root.rs", "fn root() {}\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "root"]);
    git(
        repository.path(),
        &["tag", "-a", "root-release", "-m", "root release"],
    );

    let reviews =
        diff_commit(repository.path(), Path::new("root-release")).expect("review root tag");

    assert!(matches!(
        reviews.as_slice(),
        [FileReview::Diff(diff)] if diff.path == "root.rs" && !diff.hunks.is_empty()
    ));
}

#[test]
fn merge_commit_is_compared_with_its_first_parent() {
    let repository = initialized_repository();
    write(repository.path(), "main.txt", "base\n");
    write(repository.path(), "side.txt", "base\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "base"]);
    git(repository.path(), &["branch", "side"]);

    write(repository.path(), "main.txt", "main\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "main"]);
    git(repository.path(), &["switch", "--quiet", "side"]);
    write(repository.path(), "side.txt", "side\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "side"]);
    git(repository.path(), &["switch", "--quiet", "main"]);
    git(
        repository.path(),
        &["merge", "--quiet", "--no-ff", "side", "-m", "merge"],
    );

    let reviews = diff_commit(repository.path(), Path::new("HEAD")).expect("review merge");
    let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

    assert_eq!(paths, vec!["side.txt"]);
}

#[test]
fn oversized_commit_blob_stays_visible_without_loading_its_pair() {
    let repository = initialized_repository();
    write_bytes(repository.path(), "large.txt", &[0xff]);
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
    write(repository.path(), "large.txt", "12345");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "large"]);

    let reviews = diff_commit_with_limit(repository.path(), Path::new("HEAD"), 4)
        .expect("review oversized commit");

    assert!(matches!(
        reviews.as_slice(),
        [FileReview::Notice(FileNotice::TooLarge {
            path,
            before_bytes: Some(1),
            after_bytes: Some(5),
            limit_bytes: 4,
        })] if path == "large.txt"
    ));
}

#[test]
fn non_commit_revision_is_rejected_as_a_commitish() {
    let repository = initialized_repository();
    git(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "empty"],
    );

    let error = diff_commit(repository.path(), Path::new("HEAD^{tree}"))
        .expect_err("tree must not be accepted as a commit");

    assert!(error.to_string().contains("does not resolve to a commit"));
}

#[test]
fn sha256_root_commit_uses_the_repository_empty_tree() {
    let repository = TempDir::new().expect("temporary SHA-256 repository");
    git(
        repository.path(),
        &[
            "init",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha256",
        ],
    );
    git(repository.path(), &["config", "user.name", "Alpha"]);
    git(
        repository.path(),
        &["config", "user.email", "alpha@example.invalid"],
    );
    write(repository.path(), "root.txt", "root\n");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "root"]);

    let reviews =
        diff_commit(repository.path(), Path::new("HEAD")).expect("review SHA-256 root commit");

    assert!(matches!(
        reviews.as_slice(),
        [FileReview::Diff(diff)] if diff.path == "root.txt" && !diff.hunks.is_empty()
    ));
}

fn initialized_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary repository");
    git(
        repository.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(repository.path(), &["config", "user.name", "Alpha"]);
    git(
        repository.path(),
        &["config", "user.email", "alpha@example.invalid"],
    );
    repository
}

/// Real filesystem input committed through Git's external contract.
fn write(root: &Path, path: &str, source: &str) {
    write_bytes(root, path, source.as_bytes());
}

/// Real binary filesystem input committed through Git's external contract.
fn write_bytes(root: &Path, path: &str, source: &[u8]) {
    let path = root.join(path);
    let Some(parent) = path.parent() else {
        panic!("test path needs a parent");
    };
    fs::create_dir_all(parent).expect("create test parent");
    fs::write(path, source).expect("write test source");
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(root).args(args).output();
    let output = output.expect("execute Git");
    assert!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

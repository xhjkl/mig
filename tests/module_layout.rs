use std::fs;
use std::path::Path;

/// Repository-wide guard for the `X.rs` plus `X/**` module layout.
#[test]
fn rust_modules_never_use_mod_rs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![root.to_owned()];
    let mut offenders = Vec::new();

    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!(
                    "failed to inspect an entry in {}: {error}",
                    directory.display()
                )
            });
            let name = entry.file_name();
            let file_type = entry.file_type().unwrap_or_else(|error| {
                panic!("failed to inspect {}: {error}", entry.path().display())
            });

            if file_type.is_dir() {
                if name == ".git" || name == "target" {
                    continue;
                }
                pending.push(entry.path());
                continue;
            }
            if name == "mod.rs" {
                offenders.push(entry.path());
            }
        }
    }

    offenders.sort();
    let offenders = offenders
        .iter()
        .map(|path| relative_display(root, path))
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "`mod.rs` is forbidden; use `X.rs` with children under `X/**`: {}",
        offenders.join(", ")
    );
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

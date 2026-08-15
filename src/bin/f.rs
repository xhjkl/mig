use mig::{diff::diff_file, fixture, ui};

fn main() -> anyhow::Result<()> {
    let diff = diff_file(fixture::LABEL, fixture::BEFORE, fixture::AFTER)?;
    ui::run(vec![diff.into()])
}

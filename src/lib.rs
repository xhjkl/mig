mod app;
mod commit;
mod diff;
#[cfg(any(test, feature = "fixture-bins"))]
mod fixture;
mod input;
mod review;
mod ui;
mod worktree;

pub use app::run;

/// Run the single-file visual fixture.
#[cfg(feature = "fixture-bins")]
pub fn run_fixture() -> anyhow::Result<()> {
    let diff = diff::diff_file(fixture::LABEL, fixture::BEFORE, fixture::AFTER)?;
    ui::run(vec![review::ReviewEntry::Diff(diff)])
}

/// Run the web-language visual fixtures.
#[cfg(feature = "fixture-bins")]
pub fn run_web_fixture() -> anyhow::Result<()> {
    let mut reviews = Vec::with_capacity(fixture::web::ALL.len());
    for fixture in fixture::web::ALL {
        let diff = diff::diff_file(fixture.path, fixture.before, fixture.after)?;
        reviews.push(review::ReviewEntry::Diff(diff));
    }

    ui::run(reviews)
}

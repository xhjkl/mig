mod app;
mod commit;
#[cfg(feature = "fixture-bins")]
mod dump;
mod input;
mod review;
#[cfg(feature = "stress")]
mod stress;
mod ui;
mod worktree;

#[cfg(feature = "fixture-bins")]
use mig_core::fixture;

pub use app::run;
#[cfg(feature = "stress")]
pub use stress::run_stress;

/// Run the single-file visual fixture.
#[cfg(feature = "fixture-bins")]
pub fn run_fixture() -> anyhow::Result<()> {
    let diff = mig_core::diff_file(fixture::LABEL, fixture::BEFORE, fixture::AFTER)?;
    ui::run(vec![review::ReviewEntry::Diff(diff.into())])
}

/// Run the web-language visual fixtures.
#[cfg(feature = "fixture-bins")]
pub fn run_web_fixture() -> anyhow::Result<()> {
    let mut reviews = Vec::with_capacity(fixture::web::ALL.len());
    for fixture in fixture::web::ALL {
        let diff = mig_core::diff_file(fixture.path, fixture.before, fixture.after)?;
        reviews.push(review::ReviewEntry::Diff(diff.into()));
    }

    ui::run(reviews)
}

/// Print the review as plain text instead of opening the terminal interface.
#[cfg(feature = "fixture-bins")]
pub fn run_dump() -> anyhow::Result<()> {
    let reviews = app::collect_reviews()?;
    dump::print(&reviews)
}

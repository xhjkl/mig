use mig::{diff::diff_file, fixture::web, review::FileReview, ui};

fn main() -> anyhow::Result<()> {
    let mut reviews = Vec::with_capacity(web::ALL.len());
    for fixture in web::ALL {
        let diff = diff_file(fixture.path, fixture.before, fixture.after)?;
        reviews.push(FileReview::from(diff));
    }

    ui::run(reviews)
}

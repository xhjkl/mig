use mig::{diff::diff_file, fixture::web, review::ReviewItem, ui};

fn main() -> anyhow::Result<()> {
    let mut reviews = Vec::with_capacity(web::ALL.len());
    for fixture in web::ALL {
        let diff = diff_file(fixture.path, fixture.before, fixture.after)?;
        reviews.push(ReviewItem::from(diff));
    }

    ui::run(reviews)
}

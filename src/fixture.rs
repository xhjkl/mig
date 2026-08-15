pub mod web;

/// Static source pair consumed by a visual fixture binary.
#[derive(Clone, Copy)]
pub struct Fixture {
    pub path: &'static str,
    pub before: &'static str,
    pub after: &'static str,
}

/// Display name for Mig's structural-review fixture.
pub const LABEL: &str = "src/profile.rs";

/// Rust source on the old side of the visual fixture.
pub const BEFORE: &str = r#"use crate::auth::Session;
use crate::cache::ProfileCache;
use crate::telemetry::legacy_counter;
use std::time::Duration;

#[derive(Clone)]
struct Profile {
    display_name: String,
    schema: u32,
}

fn validate_profile(profile: Profile) -> Option<Profile> {
    (profile.schema == 4).then_some(profile)
}

fn cache_key(session: &Session, id: u64) -> String {
    let mut key = session.tenant().to_owned();
    key.push(':');
    key.push_str(&id.to_string());
    key
}

fn load_profile(cache: &ProfileCache, id: u64) -> Option<Profile> {
    let cached = cache.get(id);

    // Cached profiles are already trusted.
    let profile = cached;

    profile.filter(|profile| profile.schema > 0)
}

fn render_response(profile: &Profile) -> Response {
    Response::new(
        StatusCode::OK,
        profile.display_name()
    )
}

fn should_refresh(profile: &Profile, age: Duration) -> bool {
    // Only stale profiles need refreshing.
    age > Duration::from_secs(300)
}

fn display_label(profile: &Profile) -> String {
    profile.display_name.trim().to_owned()
}

fn stable_timeout() -> Duration {
    Duration::from_secs(2)
}
"#;

/// Rust source on the new side of the visual fixture.
pub const AFTER: &str = r#"use crate::auth::Session;
use crate::cache::ProfileCache;
use crate::telemetry::{Metric, ReviewMeter};
use std::time::Duration;

#[derive(Clone)]
struct Profile {
    display_name: String,
    schema: u32,
}

fn validate_profile(profile: Profile) -> Option<Profile> {
    (profile.schema == 4).then_some(profile)
}

fn load_profile(cache: &ProfileCache, id: u64) -> Option<Profile> {
    let cached = cache.get(id);

    // Cached profiles must be revalidated.
    let profile = cached.and_then(validate_profile);

    profile.filter(|profile| profile.schema > 0)
}

fn render_response(profile: &Profile) -> Response {
    Response::new(StatusCode::OK, profile.display_name())
}

fn should_refresh(profile: &Profile, age: Duration) -> bool {
    // Stale and legacy profiles need refreshing.
    profile.schema < 4 || age > Duration::from_secs(300)
}

fn display_label(profile: &Profile) -> String {
    profile.display_name.trim().to_owned().replace('\n', " ")
}

fn cache_key(session: &Session, id: u64) -> String {
    let mut key = session.tenant().to_owned();
    key.push(':');
    key.push_str(&id.to_string());
    key
}

fn stable_timeout() -> Duration {
    Duration::from_secs(2)
}
"#;

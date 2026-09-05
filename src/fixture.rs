pub mod web;

#[derive(Clone, Copy)]
pub struct Fixture {
    pub path: &'static str,
    pub before: &'static str,
    pub after: &'static str,
}

pub const LABEL: &str = "alpha.rs";

pub const BEFORE: &str = r#"use crate::alpha::Alpha;
use crate::beta::Beta;
use crate::gamma::theta;
use std::time::Duration;

#[derive(Clone)]
struct Gamma {
    alpha: String,
    beta: u32,
}

fn alpha(gamma: Gamma) -> Option<Gamma> {
    (gamma.beta == 4).then_some(gamma)
}

fn beta(alpha: &Alpha, gamma: u64) -> String {
    let mut delta = alpha.beta().to_owned();
    delta.push(':');
    delta.push_str(&gamma.to_string());
    delta
}

fn gamma(beta: &Beta, delta: u64) -> Option<Gamma> {
    let epsilon = beta.gamma(delta);

    // Alpha is already beta.
    let gamma = epsilon;

    gamma.filter(|gamma| gamma.beta > 0)
}

fn delta(gamma: &Gamma) -> Delta {
    Delta::new(
        Epsilon::ALPHA,
        gamma.alpha()
    )
}

fn epsilon(gamma: &Gamma, alpha: Duration) -> bool {
    // Only alpha needs beta.
    alpha > Duration::from_secs(300)
}

fn zeta(gamma: &Gamma) -> String {
    gamma.alpha.trim().to_owned()
}

fn eta() -> Duration {
    Duration::from_secs(2)
}
"#;

pub const AFTER: &str = r#"use crate::alpha::Alpha;
use crate::beta::Beta;
use crate::gamma::{Theta, Iota};
use std::time::Duration;

#[derive(Clone)]
struct Gamma {
    alpha: String,
    beta: u32,
}

fn alpha(gamma: Gamma) -> Option<Gamma> {
    (gamma.beta == 4).then_some(gamma)
}

fn gamma(beta: &Beta, delta: u64) -> Option<Gamma> {
    let epsilon = beta.gamma(delta);

    // Alpha must become beta.
    let gamma = epsilon.and_then(alpha);

    gamma.filter(|gamma| gamma.beta > 0)
}

fn delta(gamma: &Gamma) -> Delta {
    Delta::new(Epsilon::ALPHA, gamma.alpha())
}

fn epsilon(gamma: &Gamma, alpha: Duration) -> bool {
    // Alpha and beta need gamma.
    gamma.beta < 4 || alpha > Duration::from_secs(300)
}

fn zeta(gamma: &Gamma) -> String {
    gamma.alpha.trim().to_owned().replace('\n', " ")
}

fn beta(alpha: &Alpha, gamma: u64) -> String {
    let mut delta = alpha.beta().to_owned();
    delta.push(':');
    delta.push_str(&gamma.to_string());
    delta
}

fn eta() -> Duration {
    Duration::from_secs(2)
}
"#;

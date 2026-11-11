//! hushrun's types, configuration parsing and validation, and the planner.
//!
//! This crate is pure: no IO, no clock and no environment access. Everything the planner
//! needs is passed in, so any decision can be replayed from its recorded input.
#![forbid(unsafe_code)]

pub mod agent;
pub mod counts_from;
pub mod digest;
pub mod event;
pub mod glob;
pub mod jcs;
pub mod ledger_url;
pub mod names;
pub mod plan;
pub mod policy;
pub mod run;
pub mod scope;
pub mod signers;
pub mod time;
pub mod transparency;
pub mod units;
pub mod yaml;

pub use digest::Digest;
pub use names::{ArtifactName, CheckName, ScopeName, StageName};
pub use run::{Custody, Provider, RunRef, Trigger};
pub use time::{Dur, Time};

/// Product and binary name. A placeholder until 1.0: change it here and in the Cargo names.
pub const PRODUCT: &str = "hushrun";

static RELEASE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Names the release that is running, for [`user_agent`]. The binary calls this first: this
/// crate's own version stays put, since the hosted service builds with it and a CLI release
/// changes nothing for it.
pub fn set_release(version: &'static str) {
    let _ = RELEASE.set(version);
}

/// The `User-Agent` of hushrun's HTTP clients: the product and the release that is running.
pub fn user_agent() -> String {
    let version = RELEASE.get().copied().unwrap_or(env!("CARGO_PKG_VERSION"));
    format!("{PRODUCT}/{version}")
}

/// Base of every predicate type and schema URI. Changing it later is a breaking change.
pub const SPEC_URI_BASE: &str = "https://github.com/hushrun/hushrun/spec";

/// The scope of a repository whose policy declares none.
pub const DEFAULT_SCOPE: &str = "default";

/// The source digest canonicalization this version implements. Frozen once M1 ships: a
/// change is a new canonicalization, and digests never match across them (D-8).
pub const CANONICALIZATION: &str = "hushrun-source-v1";

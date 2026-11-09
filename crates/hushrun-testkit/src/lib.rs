//! Test support: golden vectors, fixtures and fakes (dev only).
#![forbid(unsafe_code)]

#[cfg(unix)]
pub mod container;
pub mod fake_forge;
pub mod fake_github;
pub mod fake_gitlab;
pub mod fake_ledger;
pub mod fake_results;
pub mod git;
pub mod http;
pub mod scenario;
pub mod schema;
pub mod sigstore;
pub mod vectors;
pub mod world;

use std::path::PathBuf;

/// The workspace root, for reading `testdata/`.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/hushrun-testkit is two levels below the root")
        .to_path_buf()
}

/// A path under `testdata/`.
pub fn testdata(relative: &str) -> PathBuf {
    workspace_root().join("testdata").join(relative)
}

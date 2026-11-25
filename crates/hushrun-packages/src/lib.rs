//! Baked packages (`docs/spec/11-baked-packages.md`, K-1 to K-12): a **package store**, a folder
//! of a repository's downloaded packages, one **part** per package manager in that tool's own
//! cache layout, every file listed with its SHA-256, optionally signed. `hushrun packages bake`
//! writes one; a run, or `hushrun packages exec`, points each tool at its part.
//!
//! It is a sibling of the baked store of compiled code (`hushrun_build::baked`), never inside
//! it: a run, and every released hushrun, refuses a baked store that holds anything else.
//!
//! | Path | What |
//! |---|---|
//! | `packages.json` | [`store::Packages`], schema `hushrun.baked-packages/v1` |
//! | `packages.sigstore.json` | optional: its signature, a Sigstore bundle |
//! | `rows/<part>.jsonl` | one [`store::Row`] a line per file of the part, sorted by path as bytes |
//! | `parts/<part>/…` | the part's files, in the package manager's own cache layout |
//!
//! Nothing here opens a network connection or decides by a clock.

pub mod store;
pub mod write;

/// The variable that names a package store a run reads (K-11). Set and empty: none.
pub const LOWER: &str = "HUSHRUN_PACKAGES_LOWER";
/// The variable that lets a run that writes nothing others trust read a store nothing verifies
/// (K-7): `1`.
pub const LOWER_UNTRUSTED: &str = "HUSHRUN_PACKAGES_LOWER_UNTRUSTED";
/// K-11: where a run finds a package store when [`LOWER`] is unset: the folder an image's
/// `/hushrun-packages` is copied to.
pub const DEFAULT_LOWER: &str = "/hushrun-packages";
/// K-11: `HUSHRUN_PACKAGES=off` turns every reader off.
pub const OFF: &str = "HUSHRUN_PACKAGES";

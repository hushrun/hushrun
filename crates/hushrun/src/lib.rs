//! hushrun's code, as a library: the binary in `main.rs` is a thin `main` over it.
//!
//! The library exists so that hushrun's own integration tests can call its code in-process, an
//! adapter's steps and the unit store, rather than start the binary, which reads every source
//! file of the crate (CONTRIBUTING.md, "Which tests CI runs again"). It is not a stable API: the
//! crate is never published, and anything here may change with any commit.
#![forbid(unsafe_code)]
// CI documents the library with its private items, as cargo documented the binary before it, so
// a public item's link to a private one resolves (`rustdoc` in .github/workflows/ci.yml).
#![allow(rustdoc::private_intra_doc_links)]

pub mod app;
pub mod ci;
pub mod cli;
pub mod cmd;
pub mod coding_agent;
pub mod context;
pub mod credentials;
pub mod environment;
pub mod events;
pub mod exit;
pub mod gitlab_job;
pub mod hosted;
pub mod isolated;
pub mod lifecycle;
pub mod machine;
pub mod measure;
pub mod notice;
pub mod outbox;
pub mod output;
pub mod planfile;
pub mod reach;
pub mod releases;
pub mod results;
pub mod session;
pub mod signing;
pub mod skill;
pub mod units;

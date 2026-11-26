//! What every build tool's cache that hushrun serves shares, whatever the tool: the HTTP/1.1 it
//! is spoken to in ([`http`]) and where it listens ([`endpoint`]). Gradle's (`crate::gradle`)
//! and Maven's (`crate::maven`) caches are built on them.

pub mod endpoint;
pub mod http;

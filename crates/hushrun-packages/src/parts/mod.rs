//! One module per part (K-10). [`crate::part::ALL`] lists them.

pub mod bundler;
pub mod cargo;
pub mod composer;
pub mod go;
pub mod gradle;
pub mod hex;
pub mod maven;
pub mod npm;
pub mod nuget;
pub mod pip;
pub mod pnpm;
pub mod poetry;
pub mod swiftpm;

#[cfg(test)]
mod tests;

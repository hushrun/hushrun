//! One module per part (K-10). [`crate::part::ALL`] lists them.

pub mod bun;
pub mod bundler;
pub mod composer;
pub mod gradle;
pub mod maven;
pub mod npm;
pub mod nuget;
pub mod pip;
pub mod pnpm;
pub mod poetry;
pub mod swiftpm;
pub mod yarn;

#[cfg(test)]
mod tests;

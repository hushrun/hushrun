//! One module per part (K-10). [`crate::part::ALL`] lists them.

pub mod composer;
pub mod gradle;
pub mod maven;
pub mod npm;
pub mod nuget;
pub mod pip;
pub mod pnpm;
pub mod poetry;
pub mod vcpkg;

#[cfg(test)]
mod tests;

//! One module per part (K-10). [`crate::part::ALL`] lists them.

pub mod composer;
pub mod gradle;
pub mod hex;
pub mod maven;
pub mod npm;
pub mod pip;
pub mod pnpm;
pub mod poetry;

#[cfg(test)]
mod tests;

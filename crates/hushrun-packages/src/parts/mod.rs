//! One module per part (K-10). [`crate::part::ALL`] lists them.

pub mod gradle;
pub mod maven;
pub mod npm;
pub mod nuget;
pub mod pnpm;

#[cfg(test)]
mod tests;

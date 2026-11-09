# F39: a crate of another workspace, by path

**Pattern.** `back/` builds with a crate of `cli/`'s Cargo workspace by path, and that crate
inherits from its workspace: its edition, its lints and its dependencies (`x.workspace = true`).
CI tests each in its own job.

**Mirrors.** hushrun's own repository, where a server depends on four of the CLI's crates.

**Expected after ADR 0055.** Two scopes. back's holds back, the crate it builds with, and
`cli/Cargo.toml`, the workspace's manifest it inherits from; not the crate's dev-dependency
`kit`, which back never builds. cli's holds `cli/**`. Before, init kept one scope: `cli/crates/core/Cargo.toml
inherits from a workspace`.

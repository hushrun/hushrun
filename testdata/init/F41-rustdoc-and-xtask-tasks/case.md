# F41: rustdoc under its job's flags, and the repository's own tasks

**Pattern.** `cargo doc` in a job whose `env` sets `RUSTDOCFLAGS: -D warnings`, which makes a
broken link an error; and `cargo xtask` tasks: `lint`, `codegen --check` and `trace`, and
`dist` in a job of its own.

**Mirrors.** hushrun's own repository, whose agents missed rustdoc on 2026-11-05 because init
didn't see it, and rust-analyzer's, Bevy's and wasmtime's `cargo xtask` checks.

**Expected after ADR 0055.** init wraps `cargo doc` as `rustdoc`, a linter's check, with
`RUSTDOCFLAGS` carried into the line through `env`: a run elsewhere without the flag would pass on
a broken link, and its pass must never skip CI's. It wraps `cargo xtask lint` and `cargo xtask
codegen --check`, each named after its task, and lists `cargo xtask trace` and `cargo xtask dist`
under "Could also wrap": a task of the repository's own can do anything.

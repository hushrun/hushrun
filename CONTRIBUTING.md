# Contributing to hushrun

Thanks for helping. Issues and pull requests are welcome.

## Before you start

For anything larger than a small fix, open an issue first so we can agree on the approach. hushrun
records its decisions as ADRs in [docs/adr](docs/adr), and its normative rules live in
[docs/spec](docs/spec): a change to behavior usually changes one of them too.

## Building and testing

You need the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it on first use) and
git.

```bash
cargo build
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Tests run against fakes and need no network. Integration tests against real registries, Sigstore
and Docker sit behind the `it` feature (`cargo test --features it`). A test that covers a spec rule
names it after a `covers:` marker, and `cargo xtask trace` checks every rule is covered.

## Pull requests

- Keep each pull request to one change, with tests.
- Use [Conventional Commits](https://www.conventionalcommits.org) for commit messages, such as
  `fix(init): read matrix values from an expression`.
- Sign off every commit (`git commit -s`), which certifies the
  [Developer Certificate of Origin](https://developercertificate.org): that you wrote the change,
  or have the right to submit it under the project's license.

### Which tests CI runs again

CI's `test` (Linux) and `test-macos` (macOS) steps run `cargo nextest` through `hushrun run` with
`tests: mode: reuse` ([hushrun.yaml](hushrun.yaml)): a test binary that already passed on the files
it read doesn't run again on that OS. For Rust, the unit is a whole test binary, never one test.
A test binary reads its own sources, every workspace library it may link, and, for an integration
test, every other target of its package
([ADR 0068](docs/adr/0068-test-files-observed.md), C-31). Each file in `crates/hushrun/tests/` is an
integration test of the `hushrun` package, which starts the `hushrun` binary, so each reads all of
`crates/hushrun/src/`. Those 80 binaries and the binary's own unit tests are 81 of CI's 108 test
binaries, and nearly all of its test time.

| A pull request that changes | runs, in `test` and `test-macos` |
| --- | --- |
| only files `tests.ignore` lists, such as `docs/adr/`, `docs/spec/`, the root's `*.md`, `ci.yml` | an audit's 2%: about 3 binaries, a few minutes |
| one file in `crates/hushrun/tests/` | that binary, and the audit |
| a library crate, such as `crates/hushrun-build/src/` | its own tests and those of every crate that links it, so `hushrun`'s 81 too |
| anything in `crates/hushrun/src/` | all 108: 25 to 40 minutes on Linux, about 45 on macOS |
| `Cargo.lock`, `rust-toolchain.toml` or a build script | all 108 |

So a pull request that changes `crates/hushrun/src/` runs every test binary on every push, on both
systems. That is reuse working: each of those tests runs the binary that changed. The macOS job is
the longest, so that is where it shows. To keep a pull request from running more than it must:

- **Push once the branch is ready**, after `hushrun ready` here. A push cancels the pull request's
  running jobs (`cancel-in-progress` in `ci.yml`), and a cancelled `hushrun run` records nothing:
  a CHANGELOG fix pushed ten minutes after the code throws away the macOS job's run, and the next
  one runs every binary the code changed again.
- **Merge `main` in only when GitHub says it conflicts**, or just before merging. CI already tests
  the pull request merged with `main`, and each push of a merge in which `main` changed
  `crates/hushrun/src/` runs every test binary again.
- **Give docs, ADR and spec changes that stand alone a pull request of their own**: alone, they
  reuse all but the audit's.
- **Put a test that needs only a library in that library's crate**, not in `crates/hushrun/tests/`:
  there it reads that crate and what it links, never the whole binary.
- **Read the line hushrun prints before the tests start**, such as `hushrun: test-macos: 3 of 107
  test binaries run; 104 already passed on the files they read` or `all 108 test binaries run: …`:
  it says what CI decided, and why.

The Maven and Gradle adapters' tests need a filled Maven repository and Gradle user home. CI keeps
both in Actions' cache, keyed by the files that decide what they download (`ci.yml` lists them). A
pull request that changes one of those files fills them online once, which runs those two test
binaries outside hushrun first.

Pull requests are reviewed and merged here, with your commits and their sign-off as they are.
This repository is public, so CI scans every push and pull request for anything that looks like
a secret (`scripts/check-tree.py`, `scripts/verify.sh`).

## License

By contributing, you agree that your contributions are licensed under the [MIT license](LICENSE.md).

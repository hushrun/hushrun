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

Pull requests are reviewed and merged here, with your commits and their sign-off as they are.
This repository is public, so CI scans every push and pull request for anything that looks like
a secret (`scripts/check-tree.py`, `scripts/verify.sh`).

## License

By contributing, you agree that your contributions are licensed under the [MIT license](LICENSE.md).

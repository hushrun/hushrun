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

## Which tests CI runs again

CI runs `cargo nextest` through `hushrun run test` with `tests: mode: reuse` (`hushrun.yaml`):
a test that already passed on the files it read isn't run again. What a Rust test read is what
it can reach from its own code, by a static map of each test's calls that needs no coverage run:

- the functions a test calls, and the ones those call, in every crate of the workspace;
- for a call through a trait object (`dyn Adapter`) or a generic bound (`A: Adapter`), and in a
  trait's default method, which calls through `Self`: every implementation of the trait;
- for a test that starts the `hushrun` binary (`env!("CARGO_BIN_EXE_hushrun")`, directly or
  through hushrun-testkit or `tests/common/cli.rs`): the whole binary, every file of
  `crates/hushrun/src`.

So a test that starts the binary runs again on any change to hushrun, and so does a test that
goes through `units::adapter::choose`, `choose_kept` or `RECOGNIZED`, through `units::Session`
or `units::why`, which take a `Box<dyn Adapter>`, or through anything in `cmd::`: each reaches
every adapter. Until the static map lands, a test binary runs again when anything it was built
from changed, which for `crates/hushrun/tests/` is all of `crates/hushrun/src`.

### An adapter test the map can narrow

`crates/hushrun` is a library its binary is a thin `main` over, so a test can call its code
in-process. An adapter's tests live in `tests/units_<adapter>.rs` and run a check as `hushrun run
CHECK -- COMMAND` would, through that one adapter. `units_gradle.rs` is the pattern:

```rust
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, gradle};

/// What Gradle's adapter answers that never changes in a run.
fn profile(adapter: &gradle::Recognized) -> Profile {
    Profile { word: adapter.word(), runner: adapter.runner(), /* … each field */ }
}

/// `hushrun run unit -- ARGS`, in-process, through Gradle's adapter alone.
fn run(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // What `gradle::recognized` does, without the trait object.
    let adapter = gradle::recognize(&command).unwrap().unwrap();
    if let Some(why) = gradle::unloadable(&adapter, &r.repo) {
        return in_process::Run::new(r, "unit", args, env).unobserved(&why);
    }
    hushrun_run!(r, "unit", args, env, &adapter, plan_from_files, profile(&adapter))
}
```

- Name the adapter by its own type (`gradle::Recognized`, `maven::Recognized`, `pytest::Pytest`)
  and find it as its `recognized` function does, without the box: its own `recognize` and
  checks. Never call `recognized`, `choose`, `choose_kept` or `RECOGNIZED`.
- `hushrun_run!` (`tests/common/in_process.rs`) makes each call `hushrun run` makes of the
  adapter, on the value you pass, around the steps of `units::Observer`: the code every adapter
  shares, which takes no adapter. Pass the method that makes the plan, the one the adapter's
  `impl Adapter` defines: `plan`, or `plan_from_files` where it keeps the trait's default `plan`.
  The same goes for anything else you call on it: call what its `impl` defines, never a default
  that calls another method.
- Write no helper generic over `Adapter`, and take no `&dyn Adapter`: each reaches every adapter.
  A helper of your own takes the adapter's own type.
- A `Ran` holds COMMAND's `code`, `stdout` and `stderr`, the `lines` hushrun's steps said
  (`hushrun run` writes each after `hushrun: `), and `units`, the counts the check's result
  keeps. `Repo::map`, `unit_results` and `disagreements` read this machine's store.
- COMMAND runs for real: the runner and its tools, as before. Fixtures and tool lookups that an
  adapter's end-to-end tests share go in `tests/common/<adapter>.rs`.
- Nothing in the harness changes what the test process shares: it sets no environment
  variable, keeps the current directory, and installs no signal handler. COMMAND gets its
  environment on its own `Command`, and the observer gets the cache directory as an argument. So
  the tests are safe as threads under `cargo test`, not only one per process under nextest.
  Keep it that way: a test that needs a variable gives it to COMMAND (`Repo::give`, or the
  `env` of a run).

### When a test is end to end

A test starts the binary only for what the CLI alone does, which no in-process test sees: its
lines as it writes them (`hushrun: `, and their order with COMMAND's output), a run's last line
and exit code, the check's own result and skip, the audit's second start of COMMAND, `hushrun
why` and `hushrun ready`, the target runner and the programs an adapter puts on `PATH`, the hook,
and CI's detection. Those tests are the end-to-end set:

- `tests/e2e_<adapter>.rs`: each adapter's small smoke set, one run through the CLI that reuses
  and says so, and whatever of the list above the adapter has (`e2e_gradle.rs`). They include
  `tests/common/cli.rs`, the one file of `tests/common/` that names the binary; an in-process
  test file never includes it.
- the CLI's own flows, whose files keep their names for now: `hushrun init`, `ready`, `hook`,
  `login`, `run` and its CI lines, the build blocks, and the rest of `tests/` that isn't an
  adapter's.

Everything else an adapter test checks moves in-process, with the same assertions: the plan's
line, what was left out, what each unit read, what the store kept, and how each ended.

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

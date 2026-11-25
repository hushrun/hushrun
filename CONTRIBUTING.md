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
integration test of the `hushrun` package, so each reads all of `crates/hushrun/src/`: nearly all
start the `hushrun` binary, and the in-process ones link its library, which is the same files. Those 80 binaries and the binary's own unit tests are 81 of CI's 108 test
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

#### What the static map will count

A static map of each Rust test, which needs no coverage run, will narrow the unit from a test
binary to one test: a test that already passed is reused when no file it can reach changed. It
follows each call rust-analyzer resolves, item by item, and keys a test by the files those
items are in. So:

- a call on a concrete type (`adapter.read(…)` where `adapter` is a `gradle::Recognized`)
  reaches that type's own method, and nothing else;
- a call through a trait object or a generic bound (`Box<dyn Adapter>`, `&dyn Adapter`,
  `T: Adapter`) reaches the trait's method, so every `impl` of it: every adapter. So does going
  through `units::adapter::choose`, `choose_kept` or `RECOGNIZED`, `units::Session` or
  `units::why`, or anything in `cmd::`;
- a name rust-analyzer can't resolve, in a `macro_rules!` body or code a `cfg` leaves out,
  reaches every item of that name: a macro that calls `adapter.read(…)` reaches every adapter's
  `read`;
- a test that starts a program it can't name, such as the `hushrun` binary
  (`env!("CARGO_BIN_EXE_hushrun")`, `current_exe()`, hushrun-testkit's `World`,
  `tests/common/cli.rs`) or a `Command::new` of a variable outside a function the map trusts,
  reads the whole binary, every file of `crates/hushrun/src/`.

#### An adapter test the map can narrow

`crates/hushrun` is a library its binary is a thin `main` over, so a test can call its code
in-process instead of starting the binary. An adapter's tests live in `tests/units_<adapter>.rs`
and run a check as `hushrun run CHECK -- COMMAND` would, through that one adapter.
`units_gradle.rs` is the pattern:

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
    observe(r, args, env, &adapter)
}

/// One run, with each call `hushrun run` makes of the adapter, on `gradle::Recognized`.
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &gradle::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else { return run.plain() };
    let variables = adapter.variables(&observer.start_at(&run.env()));
    run.started(&mut observer, variables);
    // … the plan, COMMAND, the facts read, the audit's second run: see units_gradle.rs
    run.record(code, observed)
}
```

- Name the adapter by its own type (`gradle::Recognized`, `maven::Recognized`, `pytest::Pytest`)
  and find it as its `recognized` function does, without the box: its own `recognize` and
  checks. Never call `recognized`, `choose`, `choose_kept` or `RECOGNIZED`.
- Copy `observe` from `units_gradle.rs` and change the type: it makes each call `hushrun run`
  makes of the adapter, in order, around the steps of `units::Observer` and
  `tests/common/in_process.rs`'s `Run`, the code every adapter shares, which takes no adapter.
  Make the plan with the method the adapter's `impl Adapter` defines: `plan`, or
  `plan_from_files` where it keeps the trait's default `plan`.
- Keep it a function. No macro, no helper generic over `Adapter`, no `&dyn Adapter`: a helper of
  your own takes the adapter's own type.
- A `Ran` holds COMMAND's `code`, `stdout` and `stderr`, the `lines` hushrun's steps said
  (`hushrun run` writes each after `hushrun: `), and `units`, the counts the check's result
  keeps. `Repo::map`, `unit_results` and `disagreements` read this machine's store.
- COMMAND runs for real: the runner and its tools, as before. Fixtures and tool lookups that an
  adapter's in-process and end-to-end tests share go in `tests/common/<adapter>.rs`.
- Start every program through `Repo::command`, or `hushrun_testkit::tool::command` outside a
  `Repo`, never `Command::new` of a variable: the map counts that as perhaps the hushrun binary,
  and the test as reading all of it. `tool::command` carries the map's marker
  (`#[doc(alias = "hushrun::starts_no_workspace_binary")]`) and keeps its promise at run time:
  it panics when its program is one of the workspace's binaries. A literal name
  (`Command::new("git")`) is fine as it is.
- Nothing in the harness changes what the test process shares: it sets no environment
  variable, keeps the current directory, and installs no signal handler. COMMAND gets its
  environment on its own `Command`, and the observer gets the cache directory as an argument. So
  the tests are safe as threads under `cargo test`, not only one per process under nextest.
  Keep it that way: a test that needs a variable gives it to COMMAND (`Repo::give`, or the
  `env` of a run).

#### When a test is end to end

A test starts the binary only for what the CLI alone does, which no in-process test sees: its
lines as it writes them (`hushrun: `, and their order with COMMAND's output), a run's last line
and exit code, the check's own result and skip, the audit's second start of COMMAND, `hushrun
why` and `hushrun ready`, the hook, and CI's detection. Those tests are the end-to-end set:

- `tests/e2e_<adapter>.rs`: each adapter's small smoke set, one run through the CLI that reuses
  and says so, and whatever of the list above the adapter has (`e2e_gradle.rs`). They include
  `tests/common/cli.rs`, the one file of `tests/common/` that names the binary; an in-process
  test file never includes it.
- an adapter whose COMMAND runs the hushrun binary itself as a helper: Cargo's target runner, the
  programs `units::program` puts on `PATH` (CTest, Swift, Meson, xcodebuild), Bats' hook, the
  VSTest step, and the build blocks' wrapper, launcher and helpers. A flow that runs the helper
  reaches the whole binary, as it should, and stays end to end: never fake the helper. What of
  that adapter needs no helper, such as reading a runner's output or facts, or making its plan
  and keys, is tested in-process all the same, in `units_<adapter>.rs`.
- the CLI's own flows, whose files keep their names for now: `hushrun init`, `ready`, `hook`,
  `login`, `run` and its CI lines, the build blocks, and the rest of `tests/` that isn't an
  adapter's.

Everything else an adapter test checks moves in-process, with the same assertions: the plan's
line, what was left out, what each unit read, what the store kept, and how each ended.

Pull requests are reviewed and merged here, with your commits and their sign-off as they are.
This repository is public, so CI scans every push and pull request for anything that looks like
a secret (`scripts/check-tree.py`, `scripts/verify.sh`).

## License

By contributing, you agree that your contributions are licensed under the [MIT license](LICENSE.md).

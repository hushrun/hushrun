# 0055. init reads CI as hushrun's own repository writes it

- **Status:** accepted
- **Date:** 2026-11-06
- **Rules:** C-15, C-22, C-23
- **Amends:** [0037](0037-any-language-from-cis-own-files.md): item 4, under which a
  `workspace = true` dependency kept one scope, and item 6, under which any condition kept a job's
  checks with CI; [0050](0050-init-reads-what-ci-runs.md): item 2, the variables that go through
  `env`, and item 9, what init suggests

## Context

Setting hushrun up on its own repository on 2026-11-05 took scopes and several wraps written by hand, because of gaps in
init and `ready`:

- **One scope for the repository.** A server builds with four crates of the CLI's Cargo workspace
  by path, and those inherit from it (`edition.workspace = true`, `serde.workspace = true`). ADR
  0037 read any `workspace = true` as a dependency it couldn't place (`crates/hushrun-tlog/Cargo.toml
  inherits from a workspace`), so every change ran every check.
- **A folded block, read a line at a time.** YAML gives the shell a `run: >-` block as one line,
  but init and `ready` read its lines one by one: a wrap folded over three lines got a second
  `hushrun run` proposed inside it.
- **No way to leave a line.** init applies its whole plan or nothing, so every init wraps the
  public-good Sigstore test (`cargo test … --features it --test sigstore`), which should run
  as it is: its answer changes with Sigstore's services and trust root, not only with the code.
  The line had to be put back by hand after each init.
- **`ready` silent behind a changes filter.** Every job of the repository's `ci.yml` runs behind
  `if: needs.changes.outputs.<area> == 'true'`, from a changes script, and `ready` left
  every check of a job with an `if:` with CI, `its job has a condition`: it listed none of them.
  A workflow such a job called, `cli-ci.yml`, was read without its caller's condition at all.
- **rustdoc, unseen.** init didn't know `cargo doc`, a check agents missed on
  2026-11-05, nor the repository's `cargo xtask trace` and `cargo xtask notices --check`. And
  the job's `RUSTDOCFLAGS: -D warnings`, which makes a broken link an error, isn't part of a key:
  a laptop's pass without it would skip CI's run with it.

## Decision

1. **A crate that inherits from its workspace is followed there** (amends 0037 item 4). For
   `workspace = true` in a crate's `Cargo.toml`, its workspace is the directory
   `package.workspace` names, else the nearest one above with a `[workspace]` table that doesn't
   `exclude` the crate, as Cargo finds it, inside the repository. The crate reaches that
   workspace's `Cargo.toml`, and the `path` of each dependency it inherits from
   `[workspace.dependencies]`: those of `[dependencies]` and `[build-dependencies]`, with their
   `target.<cfg>` tables, and those of `[dev-dependencies]` only for the project itself, since Cargo
   never builds a dependency's dev-dependencies for a project that depends on it. That was checked:
   with a broken manifest for such a dev-dependency, `cargo build`, `test`, `clippy` and `doc` in
   the project pass as before. A dependency the workspace doesn't give, a workspace that isn't in
   the repository, and a manifest the reader can't read keep one scope, with why. A
   `pyproject.toml`'s `workspace = true` still does.
2. **A project inside a Cargo workspace reaches all of it.** A check whose project is a member of a
   workspace above it builds with that workspace's lockfile, the other members' manifests and its
   `[patch]`, so its scope holds the workspace's directory, whatever the member inherits. Before, it
   held only the member's.
3. **Manifests are read as TOML.** A small reader of hushrun's own takes tables, dotted and quoted
   keys, inline tables, arrays over several lines, the four kinds of strings, and comments, and
   gives up on anything else, which keeps one scope. It is read only where a manifest says
   `workspace`, so a manifest that read before reads the same.
4. **A folded block is read as YAML gives it to the shell.** In a `run: >` block, `>-` and `>+`
   too, the line break between two lines at the block's indentation is a space, and one next to a
   blank line or to a line indented further stays a break. init and `ready` read its commands so,
   each still placed on the line it starts on: a wrap goes in front of a folded command's first
   line, and a folded `hushrun run` is one through hushrun already. A comment after a block,
   indented no further than its key, isn't the block's: before, init read it as one more line of a
   `|` block.
5. **rustdoc is a check.** `cargo doc` and `cargo rustdoc`, but with `--open`, which starts a
   browser, are the check `rustdoc`, a linter's (C-26).
6. **What Cargo reads to decide what it builds goes into the line** (amends 0050 item 2). When CI
   sets `RUSTFLAGS` or `RUSTDOCFLAGS` for a cargo line, any but `cargo fmt`, the wrap carries the
   value through `env`: `hushrun run rustdoc -- env 'RUSTDOCFLAGS=-D warnings' cargo doc …`. It is
   found as 0050 item 1 finds a variable, nearest first: the step's `env`, a literal an earlier
   step writes to `$GITHUB_ENV`, the job's `env`, the workflow's. The key then holds it, as it holds
   the `A=1` of 0050 item 2's `env A=1 cmd`, so a run elsewhere runs with it too, and a pass without
   it never skips CI's. A value with `${{ … }}` is carried as written, its expressions judged as
   0050 item 1 judges a line's words, so a secret leaves the line. What the line sets itself is its
   own. The line stays as it is, with why, when an earlier step may write the variable to
   `$GITHUB_ENV` in a way only run time knows; when the step may run on Windows, which has no
   `env`; when the value can't be written into the line's YAML, such as a plain line's `: ` or ` #`,
   or a line break; and in a GitLab job whose `variables` set one, as 0050 leaves a GitLab line that
   sets variables first. Other variables aren't carried: one a repository's own tests read, such as
   the CLI's `HUSHRUN_IT_REGISTRIES`, is its people's to put in the line.
7. **The repository's own tasks** (amends 0050 item 9). `cargo xtask <task>` is a check named
   after its task, made a name, when the task is one of a task runner's names (`test`, `tests`,
   `check`, `checks`, `lint`, `vet`, `typecheck`, `fmt-check`, `format-check`, `unit`) or runs
   with `--check`, which asks it to look and change nothing; its kind is the target's (C-26). Any
   other task can release or publish, so init lists it under "Could also wrap", in any job, with
   the line to use and why: `it runs a task of the repository's own (cargo xtask), which can deploy
   or write reports: wrap it if it only runs checks`.
8. **A comment leaves a line.** `# hushrun: leave`, with the reason after it if there is one,
   `# hushrun: leave, <why>`, leaves the commands it marks as they are. init wraps none of them,
   suggests none, and lists each the table knows under what it leaves, with `a comment says to
   leave it: <why>`; their job gets nothing for them, no setup and no ID token, unless init wraps
   another line there. It marks a step's commands from the comment lines right above the step, or
   from the end of its `run:` line; a GitLab script item's, from right above the item or the end of
   its line; and one command of a `run: |` block, from a shell comment line right above it. A blank
   line between the comment and the line marks nothing. `hushrun:` starts the comment, and `leave`
   is a word of its own. A line through hushrun already stays as it is, comment or not: taking
   `hushrun run … --` off it leaves it out. The comment sits in CI's file, beside the line it is
   about, and a hushrun from before reads it as any comment.
9. **A condition on other jobs lets a job run** (C-22, amends 0037 item 6). A job's `if:` that
   reads only other jobs' outputs and results, `needs.<job>.outputs.<name>`, `needs.<job>.result`
   and `needs.*.result`, with literals, comparisons, `&&`, `||`, `!`, `success()`, `always()`,
   `!cancelled()` and the functions of values (`contains`, `startsWith`, `endsWith`, `format`,
   `join`, `toJSON`, `fromJSON`), runs the job when those jobs say so. A changes filter's says so when its area
   changed, which is when its scope's code is new too, so `ready` and the hook judge such a job's
   checks as any job's: `missing` until a pass for this exact code, then `passed`. Any other
   condition, one that reads another context (`github`, `env`, `vars`, `secrets`, `inputs`,
   `matrix`, `steps`, `runner`, `strategy`, `job`), `failure()`, a `cancelled()` not negated, a `${{
   … }}` inside it, or a word this doesn't know, leaves its checks with CI as before, `its job has a
   condition`. A workflow of this repository a job calls takes that job's condition, `job <id> of
   <file>, which calls its workflow, has a condition`, unless another job calls it without one;
   before, it took none. `ready` decides nothing CI does: `hushrun run` in CI still decides what
   skips.

## Consequences

Measured on 2026-11-06, on a scratch clone of hushrun's own repository, with init's dry run.

- **Six scopes, where init kept one.** The server's scope holds the server, the four crates and the
  CLI's `Cargo.toml`, and more: a test's `"../cli"`, and strings such as `file:../ui` in the CLI's
  own tests, bring in other projects whole. That is the safe direction ADR 0037 takes for a path it
  can't tell from a real one; declared scopes are kept, as they always are.
- **rustdoc is wrapped with its flags**, `cargo xtask notices --check` is wrapped, and `cargo xtask
  trace` is suggested. A wrap folded over lines is read as through hushrun already.
- **`ready` judges `ci.yml`'s jobs behind their changes filter as any job's.** The web apps' and
  Terraform's checks are listed wherever a run of them counts, on Linux on amd64 by `counts_from`
  rules; on a Mac each still says it needs Linux. The server's stay with CI for the job's Postgres
  service. The CLI's, through
  `cli-ci.yml`, take the `cli` job's condition, which reads only `needs`, so they are listed as
  before.
- **The hook can refuse an agent's push for a check CI won't run on that push.** A job behind a
  changes filter runs only when its area changed, and the hook can't tell from here whether it
  did. When the check has no pass yet for this exact code, as right after hushrun is set up, the
  hook refuses an agent's push until it passes here, though CI would skip the job on this push.
  That costs a run here, once, never a skip, and a person's push still goes on. It widens C-23's
  rule that only a known gap stops a push: such a check is a gap only if the job runs.
- **The public-good Sigstore line stays as it is** with `# hushrun: leave, public-good Sigstore
  changes while the code doesn't` above its step, and its job gets nothing from init.
- **The 40 earlier init fixtures' plans don't change.** F39 to F42 pin the new ones, and A9 runs
  their wrapped lines twice, as CI would.
- **Limits.**
  - GitLab's `rules:` with `changes:`, its own changes filter, still leaves a job's checks with CI,
    as do a step's `if:` and GitLab's `only`, `except` and `when: manual`.
  - A condition that reads another job's output still runs only when that job says so: one that
    isn't a changes filter, such as a release's `should_release`, now lists its checks too.
  - A member's scope is its whole workspace, wider than what Cargo reads of it.
  - `[workspace.package]`'s paths, such as `readme` and `license-file`, aren't followed: Cargo
    reads them to package a crate, not to build it.
  - `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS` and the `CARGO_TARGET_<triple>_RUSTFLAGS`
    forms aren't carried, nor are the flags `.cargo/config.toml` sets, which a scope holds anyway.
  - A cargo line through hushrun already, from before, isn't changed, flags or not: init leaves
    such a line as it is.

## Alternatives considered

- **The `toml` crate.** A full parser, and half a dozen crates more in every release binary and its
  notices, for a reader that needs a few keys.
- **Every path of `[workspace.dependencies]`.** Simpler, and wider than what a crate builds with:
  for hushrun's own server, all of the CLI.
- **The flags in the key.** The key is the command's words (C-9), and a contract that only grows
  can't add a part every key would change for.
- **Leaving a cargo line whose job sets the flags.** Safe, and it would leave rustdoc unwrapped,
  the check this is for.
- **Wrapping every `cargo xtask` task.** `cargo xtask dist` or `release` would skip a release.
- **A key in `hushrun.yaml` to leave a line**, such as a list of commands. The file is read
  strictly, so a key a hushrun doesn't know makes it refuse the file: every older one, CI's pinned
  0.7.0 included, until the pin moves. And a key would name a line by its file, job or words,
  which drift as CI's files change.
- **A flag of init's**, such as `--leave <check>`. It holds for one run, and the next init wraps the
  line again.
- **Working out the changes filter.** `ready` could run the repository's changes script or read a
  `paths-filter`'s patterns to know whether an area changed against the push's base. It starts no
  repository code (C-22, ADR 0045), and the filters are as many as repositories: a scope's digest
  already says whether its code is new.
- **Every condition stays with CI, as before.** Safe, and it left the repository's whole `ci.yml`
  unlisted, so an agent was never told of a check it missed.

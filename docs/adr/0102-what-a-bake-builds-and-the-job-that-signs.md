# 0102. What a bake builds, and the job that signs it

- **Status:** proposed; built
- **Date:** 2026-11-22
- **Rules:** B-75 rewritten, B-76 new; B-63, B-65, B-67, B-69, B-72, B-73 and B-74 changed; B-24,
  B-29, B-31 and B-66 read

## Context

B-75 (ADR 0101) lets a bake in CI on the default branch build what a merge whose checks CI skipped
left unbuilt, so `-latest` isn't the commit before. A review of that code with proof tests, and a
backend's end-to-end run against it, found that it rarely did what it was for:

1. **An init-made repository had nothing to build.** B-75 built each check hushrun.yaml gives a
   `run`, and `init` writes none: it writes `hushrun run NAME -- COMMAND` into CI's files. Adding a
   `run` would make the check required (C-10) and change how CI's `hushrun run NAME` behaves.
2. **Its keys weren't CI's.** It forced `CARGO_INCREMENTAL=0`, didn't have CI's variables
   (`RUSTFLAGS` and the like at the workflow's or the job's level), filled no matrix value, and
   built each check in a target folder of its own where CI's steps share one. Any of these gives
   every compile another key: nothing of CI's is reused, and what it uploads CI never asks for.
3. **What it closed could stick.** A build that failed or was stopped still closed a manifest naming
   the commit, so every later bake saw the commit as built and baked the part; so did a build whose
   blocks CI's short end-of-command wait left unsent. A re-run of an old bake built an older commit
   and set the backend's newest build, and a plain registry's `-latest`, back.
4. **On GitLab it removed its own store.** A bake's store there is the command's own; the first
   check's session ended by removing it.
5. **It ran the repository's code in the job that signs.** Build scripts, proc macros, tests and
   copied third-party actions ran with the job's environment: GitHub's ID token request token
   (which mints Sigstore tokens as the bake workflow), `GITHUB_TOKEN` with `packages: write`, GitLab's
   `SIGSTORE_ID_TOKEN` and `HUSHRUN_ID_TOKEN`. The reviewer proposed building in a separate job
   before the bake.

## Decision

1. **CI's file is the one definition of what the bake builds.** For each check it bakes, the bake
   reads CI's own files at the commit checked out, as `hushrun ready` reads them, and takes the
   command of `hushrun run NAME -- COMMAND` as the line writes it, its step's folder, and the
   variables CI's file gives it (workflow, job and step `env` on GitHub; the file's and the job's
   `variables`, and the first `parallel: matrix:` entry, on GitLab), each time it bakes. A check
   hushrun.yaml gives a `run` that CI doesn't run is built from it.
   - *Alternatives.* A field in hushrun.yaml that init fills with the wrapped command (outside the
     check's definition digest, so neither required-ness nor keys change), or the commands written
     into the bake workflow: either is a second copy, made once, that an edit of CI's file leaves
     behind, after which the bake builds something CI doesn't and nothing it uploads is reused.
     Copying CI's variables into the bake job has the same drift, and would put CI's secrets in the
     signing job's environment besides.
   - **A matrix stands for its primary entry**: the first combination GitHub makes whose `runs-on`
     is a Linux x64 runner (the bake job's platform), else the first. Baking each entry was
     considered: the tags name one platform's `-latest`, and two entries on the same platform would
     race for it. A matrix only CI can expand (an expression) builds nothing, said.
   - The job's **setup steps** can't be read at bake time (only GitHub runs actions), so `init`
     still copies them, filled in from the same matrix entry and variables with the same function,
     each value keeping its YAML type (a `1.80` filled in plain would read as the number 1.8). A
     step's `if:` has its matrix keys filled in with or without `${{ }}`, and each copied step gets
     the build job's workflow-level and job-level `env:` where it doesn't set them.
   - **CI's own variables take the bake job's values.** The bake runs in a job of the same
     repository on the same kind of runner, so `${{ github.workspace }}`, `${{ runner.temp }}` and
     the like, `${{ env.X }}` no level of the file sets, and GitLab's `$CI_PROJECT_DIR` and other
     variables expanded as GitLab's runner expands them (`expand: false` and `$$` included) are
     filled in from its environment. A variable with any other expression, or one naming a
     credential the build never gets, is left out with a line: passed as written, cargo would make
     a folder named `$CI_PROJECT_DIR` and no key would be CI's.
   - **COMMAND runs under CI's shell**: on GitHub the step's, job's or workflow's `shell:`, else
     GitHub's own `bash -e {0}`; on GitLab bash where the image has it, else sh. A job whose
     `runs-on` expression names no Linux x64 runner for its primary entry isn't replayed.
2. **Nothing CI doesn't set is forced**, `CARGO_INCREMENTAL` included: a crate CI compiles
   incrementally is passed through in the bake too. Each check builds into **the target folder
   cargo gives it in CI**, as `cargo metadata` says in its folder with its variables
   (`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `.cargo/config.toml`'s `build.target-dir`, else
   the workspace's own), in the job's fresh checkout, so a block keyed by the target folder's path
   is keyed as CI keys it; checks CI builds into one folder share one, and one there already gets a
   fresh one in the bake's store, said.
3. **Only a whole build is closed.** A check's build closes a manifest only when its command
   succeeded and every block it built went up: it waits for its uploads (up to 10 minutes, with no
   share-of-cost cut, since its store goes with its job) and otherwise closes nothing, said, so the
   next bake builds again. A block `begin` refused for good (`too_large`, `unsupported`, `no_scope`,
   `suspended`) never reaches the backend, so a build that made one isn't whole either: it closes
   nothing and names each refused block and why. **Only a later commit is built, and only the
   checks that are behind**: a check is behind when the backend holds no build of it, or its newest
   is of a commit HEAD descends from (`git merge-base --is-ancestor`); one HEAD doesn't descend from,
   or one a shallow checkout can't place, builds nothing, said. A check that compiles nothing (a
   linter, a JS test) closes an empty manifest, which counts as built at its commit: before, it had
   no manifest, so every bake rebuilt every check, tests included, in the job that signs. This
   asks of a backend that it keep such a manifest, with the commit its job's token vouched for,
   where it keeps none of that check with entries (never over one with entries); one that doesn't
   leaves that check behind at every bake, and only it is built. The job
   `init` writes checks out the whole history.
4. **A CI command's own store belongs to the command.** The guard of a store made for one command
   takes its lock from the process's run entry, so sessions the bake begins there end without
   removing it, and the store goes once, with the guard (removed while its lock is held, then the
   lock released). `--from store` in such a job reads nothing (no run's blocks are in it) and is
   refused, naming `--from backend`; `--deps-only`, which bakes its own build's store whatever
   `--from` says, is not (one decision, `BakeSource`, which the refusal and the bake both read).
5. **The build stays in the bake job, hardened** (this decision; the reviewer's separate job is the
   alternative below).
   - **Copied actions must be pinned.** `init` copies a `uses:` step into the bake job only when it
     is one of GitHub's own (`actions/…`, `github/…`, which run its runners anyway), pinned to a
     full commit, a container pinned by its digest, or an action of this repository whose own
     `action.yml` uses only actions this rule copies, followed into this repository's actions up to
     4 deep (one init can't read is refused as one not pinned). Otherwise it copies no step, writes
     the job with `--no-build`, and says how to pin each (`git ls-remote` prints the commit; init
     reads nothing over the network, so it can't write the pinned form itself). A tag the action's
     owner can move would otherwise run new code in the signing job. An action of another owner
     pinned to a commit can still use actions of its own by tag, which init can't read offline: the
     plan says so.
   - **The build gets no credential**, on every path that runs one: the build of what's missing,
     the dependencies-only build (B-72) and `--report`'s two builds (B-74) all run through one
     function (`bake_build::in_bake` and `run_in_bake`), which applies what follows and the
     subreaper. Its commands get the bake's environment less every credential the bake holds: GitHub's `ACTIONS_ID_TOKEN_REQUEST_TOKEN` and `_URL`,
     `ACTIONS_RUNTIME_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`; GitLab's `SIGSTORE_ID_TOKEN`,
     `HUSHRUN_ID_TOKEN`, `CI_JOB_TOKEN`, `CI_JOB_JWT*`, `CI_REGISTRY_PASSWORD`,
     `CI_DEPLOY_PASSWORD`; hushrun's own token, agent, signing and machine keys. They also get
     `HUSHRUN_BACKEND` empty and `HUSHRUN_INSIDE_RUN=1`, so a `hushrun run` inside a Makefile or a
     script looks up and records nothing. The bake gets the backend's token itself before the build
     starts (its relay mints any later one in its own process), and settles the registry's
     credential then too: in CI the job's own token first, else the Docker configuration's, read
     and its helper run before the build; after the build it consults neither a Docker
     configuration nor a credential helper, either of which the build could have written (a
     helper planted on `PATH` would otherwise run with the bake's whole environment). A signing key
     (`--signing-key FILE`, `HUSHRUN_SIGNING_KEY_FILE`) is read into memory before the build too,
     and its variable is withheld; its file is not (below).
   - **The signing token comes after the build is gone.** Keyless signing asks for its ID token
     lazily, when the folder is first signed, which is after the build of what's missing returns;
     and that returns only once every check's command and every process it started have ended: on
     Linux the bake is the build's subreaper while it runs, and when a check's command ends,
     whatever it left (a daemon, a test runner's child in a group of its own) is stopped, killed,
     tree and all, and reaped. Past the 30-minute bound the same happens to a running check, its
     process group first. On macOS (no CI bake runs there) an orphan escapes to the system.

## The threat this leaves, and why it is narrow

Code that runs in the default branch's CI can already write the repository's trusted blocks: the
build job runs the same build scripts, proc macros and tests with an ID token that writes the
`protected/` scope (B-29, B-31), and a bake signs what that scope holds. So whoever can make that
code do something can already put a block of their choosing in every image the bake signs. Running
the build in the signing job adds only what the signing identity can do beyond that: sign as
`hushrun-bake.yml@refs/heads/<default>` (or the GitLab job) something the bake didn't bake, such as a
store naming blocks outside the trusted scope, which a run with no backend to ask (B-67) would then
take on the signature alone; and, on GitHub, push to the repository's packages with `GITHUB_TOKEN`.

What remains after the hardening, on Linux runners where every step of a job is the same user:

- **`/proc/<pid>/environ`.** A process can read the initial environment of every process of the same
  user. GitHub puts the ID token request token in **every** step's environment, so the bake's own
  process and the step's shell that started it hold it; GitLab puts `SIGSTORE_ID_TOKEN` and
  `HUSHRUN_ID_TOKEN` in the job's. A build process can read them there while it runs: withholding
  them from its own environment stops the accident and the casual reader, not a determined one.
  Overwriting the bake's environment in place would leave the shell's, so it isn't done. Yama's
  `ptrace_scope=1` (Ubuntu's default) keeps a child from reading its parent's memory, so a token
  the bake holds only in memory (the Sigstore ID token it asks for after the build) isn't exposed
  that way; hence the order above.
- **`GITHUB_TOKEN`** is in the bake step's environment (`env:` on the step, for the push), readable
  the same way.
- **GitLab's copied `before_script:`** runs in the job's shell, with the job's tokens, before the
  bake; it is the repository's own text, but its `image:` is copied as written (pinning it is the
  repository's choice).
- **A signing key's file stays readable to the build.** The build runs as the same user, so it can
  read the PEM file a bake signs with: a GitLab file variable sits at a predictable path, and
  `--signing-key FILE` is in the bake's command line. hushrun reads the key before the build and
  withholds its variable, and leaves the file as it is (it is the person's). Unlike a keyless
  certificate, which lasts minutes, a key lasts until it is taken out of `build: baked: signers`,
  and `not_before` is no defence against it (decision 7). **Keyless signing is the setup to use**
  in a job that builds; a key belongs in a bake with `--no-build`, or in a job that builds nothing.
- **Sudo and Docker escape the subreaper.** On GitHub-hosted runners the build's user has
  passwordless sudo and is in the `docker` group. A build script can start a process outside the
  bake's tree (`sudo systemd-run`, `docker run -d --pid=host --privileged`), which the subreaper
  never sees and which can still run when the bake asks for its signing token and signs; with sudo
  it can also read the bake's memory, which makes Yama's `ptrace_scope` moot. Only a build in a
  separate job, on another machine, removes this; whether to move it there is the owner's
  decision.

These are the reviewer's case for a separate build job: a different job runs on a different
machine on hosted runners, with its own tokens, so nothing of the build can read the signing job's.
It costs a second checkout and toolchain setup and hands the build's blocks to the bake through the
backend, which is what the bake already reads. It stays open: the narrow surface above is what
choosing the bake job accepts. The server's word comes first wherever a backend answers (B-67): a
run that can ask it never trusts a block on the signature alone.

## Also decided

6. **A held bake has a way out** (B-73). A previous image dated past the bake's clock is never
   "recent" (it was already so; now tested). A held bake followed by no merge left `-latest` behind
   until the next merge, which the spec understated; the job `init` writes now also runs daily on a
   schedule (GitHub's `schedule`; on GitLab the plan says to add a pipeline schedule), and a bake a
   schedule starts publishes whatever the limit, as `--force` does. A GitHub scheduled run's event
   names no repository (no webhook is behind it), so its job's `if:` takes `schedule` without
   comparing the branch, and hushrun takes the ref's branch as the default branch for that event
   alone (GitHub runs a schedule only on the default branch's latest commit); before, the job never
   ran and, run, wouldn't have moved `-latest`. A full bake is never held by a
   dependencies-only image, and a dependencies-only bake asks again just before its moving tags move
   (a full image may have landed while it built). The workspace layer's recorded time counts as
   old more than ten minutes past the bake's clock (it was a day).
7. **`not_before` reads a time the signer can't choose, where there is one** (B-67). A keyless
   signature's timestamp, which the timestamp authority made and the certificate's validity holds,
   is what `not_before` compares, never the `baked_ms` the statement claims (a compromised workflow
   could sign a far-future one). A key's signature has no such time: its signed `baked_ms` is the
   key holder's claim, and `not_before` is no defence against a leaked key; taking the key out of
   `build: baked: signers` is.
8. **A dependencies-only `cargo test` compiles its tests and runs none** (`--no-run`, B-72): rustdoc
   compiles doctests without the wrapper, against the stubs, and failed on any doctest. The doc
   tests are left out: `cargo test --doc` compiles the library's tests instead (`--lib`), since
   cargo takes no `--no-run` with `--doc`. A Go
   module's COMMAND gives its build flags (`-race`, `-tags`, `-cover`, …) to the third-party build
   instead of running.
9. **Programs go in the recent layer** (B-65). A repository's programs (binaries, test executables)
   link everything below them, so most merges change them; counted against the recent layer's bound,
   they compacted the workspace layer at almost every merge (the backend's run: a 31.7 MB recent
   layer against a 120 MB workspace one), and every pull took the workspace layer again. They are
   always in the recent layer now and don't count against its bound, but one a kept workspace layer
   of an older image holds with the same bytes, which stays there: no path is in two layers. A
   fourth layer was considered; it would change the label and the backend's publish request for
   the same effect on pulls.
10. **Blocks bound to CI's folders are baked from CI's trusted scope** (B-76, the owner's decision).
    CI uploads blocks bound to its folders (B-24, B-29), and a bake from the backend's trusted scope
    by CI carries them; `baked.json` records the folders (`paths`: `HOME`, `CARGO_HOME`,
    `RUSTUP_HOME` and the checkout the build ran with, CI's file's variables for its first check
    over the bake job's own), which are CI's runner's, not a person's. A run at
    other folders says which folders those blocks need, and `init` prints them. A person's bake
    records none, and a block bound to a person's folders never leaves their machine.

11. **A check's close names no crate that stays on the machine** (B-30). A compiled crate neither
    portable nor (from a CI job's own ID token) bound to folders alone never goes up, yet the close
    named it, so every bake of the check was partial. In nushell that is nu-command, whose
    `extern` keeps it here (its externs titlecase and fff-search are built as `cdylib` too, which
    the wrapper passes through): every image of nushell was partial, took no fixed tag, and a
    backend's registry refused it. The close leaves it out, as it does a Go or C object that stays.

## Measurement

**nushell, the plan's "done when"**, as ADR 0101 measured it, now with the blocks bound to CI's
folders: nushell 0.113.0 (686 crates) in Linux containers (Docker Desktop on an Apple M-series
Mac, 10 CPUs and 8 GB, `linux-arm64`, Rust 1.94.0, `cargo build -j6`, dev profile), a release build
of this branch, every container at GitHub's runner's folders (`HOME=/home/runner`,
`CARGO_HOME=/home/runner/.cargo`, the checkout at `/home/runner/work/nushell/nushell`). The CI job:
a build with `CARGO_INCREMENTAL=0` sharing through a fake backend (the testkit's `fake_build`
example) with a GitHub job's ID token, then, at the same folders and with a fresh target, `build
bake --from backend --trusted`: 683 blocks, 765 MB, none missing, `paths` recorded as above (the
first run, before decision 11, baked 683 with one missing: nu-command). The warm laptop is a
container that built the tree (incremental, its own store); the sandbox is the base image plus the
baked store at `/hushrun`, read on the say-so (`HUSHRUN_BUILD_LOWER_UNTRUSTED=1`). One line atop
`crates/nu-cli/src/lib.rs`, four rounds, interleaved and rotated, each case in a fresh container,
host load 6.3 to 49 (other work), medians:

| Case | wall (min–max) | CPU | reused / built |
|---|---:|---:|---|
| warm laptop | 15.1 s (13.3–22.9) | 18.0 s | 0 / 3 |
| sandbox, `CARGO_INCREMENTAL=0`, audit off | **37.2 s** (32.6–45.7) | 73.3 s | 679 / 7 |
| sandbox, `CARGO_INCREMENTAL=0`, audit one in ten | 43.7 s (37.8–56.2) | 105.4 s | 607–615 / 71–79 |
| sandbox, cargo's defaults (incremental) | 45.7 s (42.3–84.8) | 136.5 s | 572–585 / 101–114 |

**2.5 times the warm laptop: the 1.5x still does not hold for nushell** (ADR 0101: 2.5x, 671
reused). The CI job uploaded 10 blocks bound to its folders (the C `-sys` crates, uucore twice, a build
script, nu-cmd-lang, the `nu` program), and the sandbox reuses 8 more crates than before; 7 compile: the change (nu-cli, nu-lsp), the `nu` program, nu-command and the two
`cdylib` crates it depends on. One sandbox build with cargo's `--timings` (32.4 s) shows where the
time goes, on the critical path: build scripts **run** again in a fresh target and compile their C
sources (libgit2-sys 7.3 s, libsqlite3-sys 3.0 s, ring 2.8 s; hushrun's C launcher serves CMake
builds, B-50, not the `cc` crate), nu-command compiles (7.2 s), and `nu` links (10.3 s, in the warm
laptop's 15 s too). What would bring it near: the build-script runs' outputs (or the `cc` crate's
compiles) reused, and `cdylib` crates stored so nu-command travels; together about 18 s of the 37
are those. Neither is in this change.

**cosign 3.0.2, offline, on timestamp-less key bundles** (a key-signed bake asks no authority unless
`HUSHRUN_TSA` names one, B-67): run in a container with no network, the trust root vendored in
`hushrun-attest` passed with `--trusted-root` (cosign's TUF fetch fails offline, as intended).

| Check | Result |
|---|---|
| `verify-blob-attestation --key` the store's bundle, `baked.json` | exit 1: `failed to verify log inclusion: not enough verified log entries from transparency log: 0 < 1` |
| the same with `--insecure-ignore-tlog=true` | `Verified OK` |
| the same, another key | exit 1: `accepted signatures do not match threshold, Found: 0, Expected 1` |
| the same, a `baked.json` one field changed | exit 1: `provided artifact digest does not match any digest in statement` |
| the image's signature (its referrer's bundle) over the image manifest's bytes, `--insecure-ignore-tlog=true` | `Verified OK` |
| the same without the flag; another key | exit 1, as for the store |
| `verify-attestation --local-image` on the OCI layout | exit 1: `must verify either an image index or image` (cosign reads its own layout, not one with a tag-schema referrer index beside the image) |

So cosign verifies a key-signed bake only with `--insecure-ignore-tlog=true`: hushrun writes nothing
to a transparency log, and without `HUSHRUN_TSA` there is no timestamp either. A run verifies it
with the key alone, as B-67 says.

## Amendment: the job that signs is CI's (ADR 0103)

What signs a bake is decided once, with the same "in CI" the bake reads for its tags and lines
(`App::bake_signing`): outside CI an ID token at hand signs only where `HUSHRUN_SIGNING=sigstore`
asks. The cosign table above is a key's, whose bundle holds no timestamp; a keyless bundle is
verified with `--use-signed-timestamps` (B-67).

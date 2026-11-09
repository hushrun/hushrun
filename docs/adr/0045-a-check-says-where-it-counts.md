# 0045. A check says where it counts

- **Status:** accepted
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-11-02
- **Rules:** C-25 (new); C-5, C-8, C-9, C-10, C-14, C-15, C-22, C-23, E-6
- **Amends:** [0044](0044-one-key-for-runs-that-differ.md), which named `counts_from` as a later
  setting. **Departs from** [0031](0031-membership-is-trust.md) amendment 1 in one respect: see
  item 9.

## Context

A key holds no environment (C-9), so a pass on any machine stands in for a run on any other. That
is the product, and it is what lets a laptop's pass skip CI. It is also wrong for a check whose answer
depends on where it runs. An agent in a Linux sandbox can pass `swiftlint`, which says nothing of
`xcodebuild test`. A laptop on Go 1.26 can pass the tests of a backend that ships on 1.25. ADR 0044
kept the key as it is and narrowed what a CI pass stands in for, and it left a gap: two workflows
with one job id on one system, on different tool versions, share a pass.

A repository knows what its checks need. It can say so in a line, and hushrun can measure that
wherever the check runs. What it can't do is prove it. Everything a machine says about itself is
that machine's word.

## Decision

1. **`counts_from` says what a check needs,** in `hushrun.yaml`: `os`, `arch`, `go`, `rust`, `node`,
   `xcode` and `env`, a closed list. Each key takes one value or a list, and a list means any of
   them. A version is a quoted prefix compared by dotted segment, so `"1.25"` matches 1.25 and
   1.25.14, not 1.250 or 1.2. An unquoted number is an error, since YAML makes `1.20` into `1.2`.
2. **The nearest level wins whole:** `checks.<name>`, else `scopes.<name>`, else the top level, with
   no merging. `counts_from: anywhere` is no rule at that level, so a check can leave its scope's
   rule. A check's rule applies by name in every scope.
3. **The rule joins the run key, and what was measured doesn't.** A rule goes into the key
   normalized, exactly as `external` does, only when there is one, so every other key stays as it
   was, bit for bit. Every machine that meets a rule computes one key, `ready` needs no server to
   find it, and a run that read another rule can't reach it. `hushrun run` and `hushrun ready` get
   key and rule from one function. `--config`, `--config-ref`, `HUSHRUN_CONFIG` and `--rev` choose
   which `hushrun.yaml` a run reads, and it takes its key and what it has to meet from that one read.
4. **A pass under the key skips on any machine,** and skipping measures nothing. The rule says who
   may leave a pass, not who may use one. Otherwise an agent on Linux couldn't skip a macOS check
   that passed on a Mac, which is the point. The guards of ADR 0044 stand: a rule never makes a run
   skip where they refuse, and they never make a run count where the rule refuses.
5. **A run measures after the command, and that decides.** A toolchain may arrive during the run. A
   run that doesn't meet the rule keeps its exit code, records nothing and costs no quota. It says
   so before a long run, for information, and its last line is the verdict. Without a rule, a run
   measures nothing and prints what it always printed.
6. **Not counting is the safe answer to everything uncertain.** A tool that isn't there, one that
   can't be measured, a variable that differs, or a command that picks its own toolchain while the
   rule names that tool doesn't count. The measuring asks what PATH finds, so a command that starts
   something else has to be read from its words: `cargo +nightly`, `rustup run`, a tool given by its
   path, `PATH` or a version manager's variable set in the command, a version manager's exec form,
   or a launcher that starts or pins the tool itself, through `env`, `nice`, `time`, `timeout`,
   `sh -c` and `cmd /c`. A launcher that starts something else counts: `npx vitest` runs on the
   machine's node, and refusing it would end skipping for most JavaScript repositories. `node` is
   asked twice when a `node_modules/.bin` holds one, as PATH finds it and with that first, because
   `npm test` runs on the second and `node t.js` on the first, and nothing says which COMMAND does:
   the rule has to hold for both. A pre-release isn't the release its number names, so `"1.94"`
   doesn't match `1.94.0-nightly`, and `"1.25"` doesn't match `1.25rc2`, unless the rule writes it
   out.
7. **`environment` is on every result:** the platform, and the tools a rule made hushrun measure. It
   informs, and only the key decides. No server change is needed: a server stores the body as sent.
8. **`ready` and the pre-push hook start no tool, for any repository,** and nothing a repository can
   name: of programs they start git, and two only the environment brings in,
   `HUSHRUN_REPOSITORY_TOKEN_COMMAND` when it is set, and `hostname` when a sign-in's machine
   registers. They look for a container runtime on `PATH` and don't start it, and the hook's lines
   don't start a `node_modules/.bin/hushrun` that git tracks. Of a rule they read what needs nothing
   started, `os`, `arch` and `env`, and a machine that doesn't meet those gets `ci`, with why. A rule
   on a tool stays `missing`, with the usual command: the run decides. A run under a rule that
   doesn't count leaves a mark, pass or fail, whose file holds its reason, such as `needs go 1.25,
   this is go1.24.3`. `ready` then says `ci` with that reason. The three states stay, and `ci` now
   means nothing here changes it, and CI or another machine has to run it. No server request is
   added.

   **A recorded failure outranks all of that.** The gate exists to stop an agent pushing a failing
   check, and a failure under a rule's key can only come from a machine that met the rule. So a mark
   counts only while its key has no result, and a failed check stays `missing` whatever this machine
   is. When a run here couldn't clear it, `ready` prints no command, and says what would: a change to
   the code, or a pass on a machine that meets the rule. A result this machine records removes its
   mark. One recorded elsewhere leaves the file, unread while the key has a result.
9. **The rule is about where a run happened, never whose evidence counts.** [ADR
   0031](0031-membership-is-trust.md) amendment 1 moved `accept: ci` to the server, because a rule
   in `hushrun.yaml` sits in the change it governs, and a pull request could drop it. `counts_from`
   sits in the change it governs too, on purpose, and we accept that, because it stops mistakes, not
   people. It is kept with the code because it is about the code: a pull request that moves to Go
   1.26 changes the rule in the same commit. A change that weakens the rule is reviewed like any
   other line. Anything that needs more than that has what it needs already, on the server, out of
   a pull request's reach: **"CI reuses only CI's results"** (C-12), and a **CI-only stage** (T-16).
10. **Values are the machine's own word.** `go env GOVERSION`, `rustc --version`, `node --version`,
    `xcodebuild -version`, the platform and every variable are what this machine says. A wrapper, a
    shim or a person can say anything, and nothing here signs or verifies it. A rule is a statement
    of intent that honest machines follow, not a control. Its job is that a person or an agent on the
    wrong machine doesn't leave a pass by accident.
11. **`ready --strict` exits 0 only when a check is known and every check passed.** Plain `ready` is
    true when nothing is `missing`, so with more `ci` states, unrun work reads as ready. A script
    that needs "this passed" can now ask for that. Plain `ready` doesn't change.
12. **`passed` says who recorded it,** with `recordedBy`, which the server sets and the CLI used to
    drop. Nothing from a result's own body is shown: another machine wrote that.
13. **`checks.<name>.kind` becomes optional,** `source` by default, with the definition digest
    (E-6) it has when written out, so that a line can hold only a rule.
14. **`hushrun init` writes one rule, `{os: macos}`, for Xcode's and Swift's checks.** A rule is only
    worth having if people have it, and an iOS app's people know where its checks count: a workflow
    that runs `xcodebuild test` on `macos-15` already says so. But the two ways of being wrong cost
    differently. A missing rule leaves a repository as it is today. A wrong one narrows where a pass
    counts, so runs stop recording, with a line each that nobody asked to read, and skips are lost
    for ever. So init writes it only when a check at a level earns it, `xcodebuild` or `swift` in a
    job whose `runs-on` is one `macos-*` runner, and every other check there fits it, the top for a
    repository of one scope and a scope otherwise. `swiftlint` fits, and earns
    nothing: it reads the same on any machine that has it. Anything unsure says no, and C-15 lists it
    all. Commands left as they are count too, because a command left today can go through hushrun
    tomorrow: that is how "a matrix says no" can mean anything, since a matrix's commands are always
    left. So do JavaScript's checks, which a package.json at the root keeps out of what init wraps,
    not out of the repository. A job with `uses:`, and a job off a Mac that uses a local action, hand
    off what they run, so they say no: what that runs can't be read, or isn't a Mac's. A file that
    writes `counts_from` anywhere, `anywhere` included, has said what it wants, and a repository
    already set up keeps what it has, so running init twice changes nothing, and a rule someone
    removed doesn't come back. GitLab is left out: a job's `tags` are whatever its runners' owner
    chose, and nothing in `.gitlab-ci.yml` names a system.

    **It may be why `hushrun.yaml` exists.** The rule goes with the scopes and the backend, which
    don't wait for a ledger, so with nothing else to write the file, init writes `version`,
    `repository` and the rule, the two keys the file can't go without. A rule is about the code, so
    it needs nothing of the org's, and a repository whose org refuses it a ledger still has the file
    its runs read. With no `origin` remote there is no honest `repository` to write, so there is no
    file. With a package.json, init still wraps CI's commands, and the plan says what to add by hand.
    Without one it wraps nothing, so there is nothing to say of a rule.

    **For that to reach an iOS project, init has to read one.** Most write `xcodebuild` with its
    action last, or over several lines, or piped into a formatter, and that was a command it left.
    So the table takes the action wherever it stands, a `|` into `xcpretty` or `xcbeautify` with no
    report stops nothing, and lines that end in `\` are read as one. A report stays what it was:
    `-resultBundlePath` and `-enableCodeCoverage` write a file a later step may read, which a skip
    wouldn't, and so does `fastlane scan`, to `fastlane/test_output` by default, so it stays as it
    is and says why.

## Consequences

- **`hushrun.yaml` with `counts_from` fails to parse before this release,** as other new keys did,
  and so does a check with no `kind`. An older hushrun runs the command without skipping and records
  nothing, and its `ready` exits 2. A repository on an older pinned release moves together.
- **Adding, removing or changing a rule gives its checks new keys,** so they run once. That is what
  a rule change means. Removing one returns to the keys from before, whose passes count again.
- **A machine that doesn't meet a rule still runs what it is told to,** at no gain, and gets the
  command's own exit code and a line.
- **A rule that names a tool needs a process per tool, twice per run,** within 10 s each. `ready`,
  the hook and a check with no rule pay nothing.
- **Learning a tool's version costs one run, per key, per machine.** `ready` can't know that this
  machine has the wrong Go until a run says so. That run is wasted, and the early warning has already
  told the person or agent, before a long command.
- **A mark stands for that exact code on that machine, after the machine is fixed too.** Someone who
  installs the right Go sees `ci` for that code, which costs one CI run, or one run by hand, which
  counts and clears the mark. The next change of code starts clean.
- **An agent on the wrong machine stays refused while a failure stands,** until the code changes or
  a machine that meets the rule passes it. That is what the hook is for.
- **A cache that keeps nothing,** read-only, a file, or `HUSHRUN_CACHE_DIR` empty, would have `ready`
  ask for ever, since it could never learn that a run didn't count. So a run says the note couldn't
  be kept, and `ready`, which tries the cache, says `ci` for a check with a tool in its rule. That
  loses a skip on a machine that would have counted, until someone runs it by hand.
- **init writes no rule for most repositories that could use one.** A Go backend on Linux, a Mac
  app on a self-hosted runner, or a repository set up before this release has to say so by hand.
  That is the price of a rule never being wrong by init's hand.
- **Some of what iOS projects write stays out of reach.** `xcodebuild test 2>&1 | xcpretty` and
  `… | xcpretty && exit ${PIPESTATUS[0]}` stay, since the pipe isn't all that is on the line, and
  `env VAR=value xcodebuild test` stays and says nothing, since `env` is no launcher. Each costs a
  skip, and a rule if it is all a level has. A folded `run: >` block is the one that goes the other
  way: it is read line by line, though YAML makes it one command, so `swift test` then
  `--enable-code-coverage` over two lines wraps, and its report doesn't keep it as it is. Reading
  one takes joining all of a folded block's lines, as lines that end in `\` are.
- **A monorepo needs its scopes declared to get a rule.** init doesn't split a project whose
  manifest is a `Package.swift`, or that has only an `.xcodeproj` (C-15): it can't say what those
  read. So an iOS project beside a Go backend stays in one scope, where Go's check keeps the rule
  out. With `scopes:` in `hushrun.yaml` before init runs, the rule goes in the iOS scope.
- **hushrun.yaml can't do this for stages.** `counts_from` in a file with stages is an error, as
  `external` is. Evidence has its own rules, and `image` and `platform` stay stage-only.
- **Limits: this stops honest mistakes, not determined ones.** The command is read as words, and
  only as far as this says. A wrapper it doesn't name, such as `docker run golang:1.24 go test` or
  `make test`, is the command it starts with. A script's own `export PATH=…`, or a `.npmrc` with
  `use-node-version`, isn't seen, unless it is in COMMAND's words or the PATH hushrun itself got. A
  name built at run time, such as `$(which go)`, reads as the word it is. `arch` is the running
  binary's, so an x86-64 hushrun under Rosetta says `amd64`. A rule's `env` values sit in
  `hushrun.yaml` in plain text, so they name a sandbox, and don't hold a secret.

## Alternatives considered

- **Measured values in the key.** Two machines on 1.25.3 and 1.25.14 would stop sharing, a lockstep
  nobody asked for, and `ready` would have to measure before it could look anything up.
- **The rule on the server.** A rule that changes with the code would have to change at the moment
  a pull request merges, and `ready` would need a request it doesn't make today. ADR 0031 amendment
  1's reason doesn't apply: this doesn't decide who is trusted.
- **Refusing on the server, by `environment`.** A server would be checking a machine's word against
  a rule in a file it doesn't read, and a refused record costs more than one not sent.
- **Merging levels.** A scope that says `os` and a check that adds `go` read well, until a check
  needs less than its scope. The nearest level is readable in one place.
- **A fourth `ready` state.** `ci` already means that nothing here helps. A fourth state would break
  every reader of `hushrun.ready/v1`, for the same instruction.
- **Measuring in `ready` and the hook, with guards.** An earlier version of this ran the tools there,
  with `GOPROXY=off`, `RUSTUP_AUTO_INSTALL=0`, no `rustc` under a `rust-toolchain` file that could
  name a directory, and no `node_modules/.bin` that git tracks. On git push, that runs on whatever
  code is being pushed. A committed `Node_Modules/.bin/node` got past the check on macOS's
  case-insensitive disk, and `git push` executed it. Volta's `.volta/hooks.json` and asdf's `path:`
  are the same class, and guarding each tool never ends. A wasted run is a far smaller price than a
  hook that starts a repository's program.
- **Reading a Swift project as a manifest, so that init splits a monorepo.** It would give that
  monorepo its rule with no `scopes:` to write, and it is a change to what a scope holds for every
  Swift project, not to `counts_from`. A split is where a wrong skip comes from, so it is a decision
  of its own.
- **A rule per check, for a repository of one scope.** `checks.<name>.counts_from` would reach a
  monorepo with no split, and init would be naming checks in a file a person has to read. A check
  name isn't a project: two jobs share one, and a rename leaves a rule behind.
- **Rules for repositories already set up.** A rule changes its checks' keys, so re-running init for
  anything else would run them all again, and it would put back a rule someone took out.
- **GitLab's runner tags.** `saas-macos-medium-m1` reads like a Mac, and a tag is free text.
- **A mark only for what `ready` can't see.** It keeps `ready` right when a machine is fixed, and it
  needs `ready` to measure.

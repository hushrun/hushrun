# hushrun guide

What a coding agent needs to know about this hushrun, beyond its skill. `hushrun guide` prints it
from the hushrun installed here, so it always says what that hushrun does. Its last section says
what is true on this machine.

## Each check is one of three

- `passed`: it passed on this code, and CI skips it. `recordedBy` says who recorded it.
- `missing`: it hasn't, and the command it prints makes it pass here. With no command, it failed on
  this code and a run here wouldn't count: `why` says what clears it, a change to the code or a pass
  on a machine that meets the rule.
- `ci`: nothing here changes it, and CI or another machine has to run it. `why` says what it needs,
  such as `needs linux, this is macos`. `a run here didn't count: needs go 1.25, this is go1.24.3`
  means a run on this machine told it that, for this code, and that no result is recorded for it:
  running it again won't change anything. If the person expects it to pass here, tell them what it
  says. Once this machine is fixed, one run counts, and clears it.

`ready` says the code is ready when nothing is `missing`, so a `ci` check hasn't run. To know that
every check passed, use `hushrun ready --strict`, which exits 0 only then.

Where results stay on this machine, `passed` means it passed here, and a run here skips it, but CI
sees none of it and runs every check. That is after `hushrun init --local`, or where nobody here
is signed in to the backend the repository names. `ready` then says `passed here`, `failed here` or
`no pass here yet`, `--format json` has `"results": "local"`, and a note says what shares the
results with CI. The hook still refuses a coding agent's push until the checks pass here.

## The pre-push hook

The hook never runs checks itself, so a long check can't time out your push. It refuses a coding
agent's push and warns a person. If a check can't run on this machine, for example because it
needs a service this machine doesn't have, the person can push it themselves, or install the hook
with `hushrun hook install --warn`, which only warns. Leave both to them.

git doesn't clone hooks. A package manager's install adds this one, and so does the first
`hushrun ready` or `hushrun run` in a clone, where hushrun is installed. After a person's
`hushrun hook uninstall`, neither does: only `hushrun hook install` brings it back, so leave that to
them.

## Reading hushrun's lines

- `hushrun: test skipped: this exact code passed …` is a pass, with exit code 0.
- `hushrun: test failed in 2s (exit 1)` is a real failure, with the check's own output above it.
- `… files changed while it ran, so it wasn't recorded …`: the check writes files that aren't
  ignored. Tell the person, and suggest adding them to `.gitignore`.
- `hushrun: test passed in 6s on linux, go1.25.14; it counts`: this machine meets what
  `counts_from` in `hushrun.yaml` asks of it, and the pass is recorded.
- `hushrun: test passed in 6s; it won't count: needs go 1.25, this is go1.24.3`: the command really
  did pass, and nothing was recorded, so CI runs it, and `hushrun ready` stops asking. The same line
  comes first, as `running: this run won't count: …`, before a long run: you can stop it there.
  Don't edit `counts_from`, and don't change the command, to get past it: it says where this
  repository's checks count. Tell the person what the line says.
- `hushrun: installed the pre-push hook in …`: this clone had none. There is nothing to do.
- `hushrun: sign in to share results with acme: npx hushrun login`: nobody has signed in here, so
  results stay on this machine: a check skips here, and CI runs it. Tell the person, who signs in
  once.
- `hushrun: results aren't shared with acme: nobody is signed in to it here, so they stay on this
  machine`: the same, without a terminal, as in a sandbox without `HUSHRUN_AGENT`. Tell the person,
  and don't try to sign in.
- `hushrun: results aren't shared with acme: …`, with another reason: no credential the backend
  takes reached it. Checks still run. Tell the person, and don't try to sign in.
- `hushrun: test running without skipping: results are off: in CI, nothing names a backend, …`:
  CI keeps no results on its runners, so this job reruns every check until the repository names a
  backend. Tell the person, who runs `hushrun init`.
- `… not recorded: …`: the check ran, and its result isn't shared. `hushrun ready` stops asking for
  it, and CI runs it. Tell the person what the line says.
- `hushrun: hushrun 0.12.0 is available, and this is 0.11.0: …`: a newer release is out. Once the
  task is done, tell the person, with the command the line gives.

## Partial runs

`vitest run src/cart.test.ts` is a different check from `vitest run`. Use partial runs while you
work, then run the full check, the way `hushrun ready` names it.

## Running a check again

To see a skipped check's output, run it again with `--force`, which runs it once and records the
result. Give the script's name and its exact command, so that the result counts for the same
check: `npx hushrun run --force test -- vitest run`.

## Bun

`bun test` is Bun's own runner. It never reads package.json, so it never goes through hushrun and
can't skip. Where the `test` script wraps it, run `bun run test`, the way `hushrun ready` names it.

## Compiled code: `hushrun build`

A check that runs can still skip compiling what this machine already compiled from the same
inputs, in any worktree or clone: Rust crates, through cargo's `RUSTC_WRAPPER`, and the C and C++
objects of a CMake build, through CMake's compiler launcher. A compile counts as the same when the
compiler, its flags, the variables it reads and every file it read are the same; the folder, the
branch and the commit don't count. Go needs none of this on one machine, since Go's own build cache already reuses its
packages: hushrun adds Go only where the org shares compiled code (below).

**It is the person's.** On a person's machine reuse is off until they turn it on, and the mode
lives in their own settings file, `build.json` beside hushrun's credentials, for every repository
on the machine. Don't run `hushrun build on`, `observe`, `off`, `clean` or `share`, or set
`HUSHRUN_BUILD`, unless the person asks. In CI and in an agent's sandbox nobody chooses: reuse is
on there when the org shares compiled code through its backend, and in a sandbox `hushrun build
off` or `HUSHRUN_BUILD=off` wins. The last section of this guide says the mode here.

- `hushrun build status`, or `hushrun build` alone: the mode, the settings file, the store, how
  long blocks stay and how much the store holds. It changes nothing.
- `hushrun build on`: a compile whose inputs were compiled before is put in place from the store
  instead of compiled.
- `hushrun build observe`: everything compiles as usual, what compiles is kept, and each run says
  what would have been reused.
- `hushrun build off`: nothing is reused or kept. The store stays as it is.
- `hushrun build clean`: deletes everything the store holds.
- `HUSHRUN_BUILD=on`, `observe` or `off` sets the mode for one shell or command, and changes no
  file.

**Only commands run through `hushrun run`.** `hushrun run NAME -- COMMAND` and the checks of
`hushrun.yaml` that run here use the store. A plain `cargo build` or `cmake --build` outside
`hushrun run` neither reads nor fills it, so run builds the way the repository's scripts do. Turning
reuse on doesn't make cargo rebuild what it built already. Nothing is reused on Windows, or for a
check `hushrun run --isolated` runs in a container.

- **Rust.** A library crate is reused in any worktree. A program, a binary or a test binary, is
  reused only in the target folder that built it, unless the run shares. A `RUSTC_WRAPPER` that is
  set already, as sccache sets it, is left as it is, and then no Rust crate is reused.
- **C and C++.** CMake caches the launcher when it configures a build folder, so a folder
  configured outside `hushrun run` gets none, and the run says `build: C and C++ objects aren't
  reused in <folder>: it was configured before hushrun's launcher; configure it again inside
  hushrun run`. A launcher already in a folder's cache, such as ccache, is CMake's to use. Inside a
  run, compiles get `-ffile-prefix-map=<checkout>=.`, so `__FILE__` and debug info name files from
  the repository's root, and an object is the same in every worktree. `build: cc: maps: off` in
  `hushrun.yaml` keeps absolute paths; then an object that holds its path is reused only in its own
  checkout.

**Where it is kept, and for how long.** The store is `build` under hushrun's cache folder, one per
machine for every repository; `HUSHRUN_BUILD_DIR` moves it. A block, one compiled crate or object,
is deleted 15 days after its last use, by a sweep after a run, at most once a day
(`HUSHRUN_BUILD_KEEP_DAYS` changes the days). There is no size limit, but nothing is stored while
the store's disk has less than 10 GB free.

**Disk.** The store is a second place compiled code lives. Its files go in and out by cloning
where the file system can clone, as APFS on macOS does, and by copying where it can't. Where they
are cloned, the store and the target folders share their bytes, so a worktree that reuses its
crates takes little more disk than what it compiled itself. Where they are copied, the store is a
second copy: it saves disk only when target folders that would otherwise stay are deleted, since
they then rebuild quickly. `du`, and the size `hushrun build status` shows, count a cloned file in
full in each place, so they overstate what the disk holds.

**Reading its lines.** After COMMAND, the run says what its compiles came to. None of these lines
fails a check: report the check's own result.

- `hushrun: build: 32 of 35 crates reused, 3 built, saving about 17s`: reuse is on.
- `hushrun: build: 35 crates built, none reused`: nothing in the store matched, as on a first
  build.
- `hushrun: build: observing: 1 of 2 crates would have been reused`: reuse is in observe, and
  everything compiled.
- `hushrun: build: c/c++: 2 objects: 2 reused, 0 built`, and in observe `hushrun: build: c/c++:
  observing: 2 of 2 objects would have been reused, 2 compared with their blocks, 0 differed`.
- `hushrun: build: go: 29 packages: 27 reused (27 from the shared store), 2 built`: where the org
  shares, `(… from the shared store)` counts what other machines compiled.
- `hushrun: build: nothing was stored because the disk has less than 10 GB free`: tell the person.
- `build: C and C++ objects aren't reused in <folder>: …`: configure that folder again inside
  `hushrun run`, the way the repository's script does, or tell the person.
- `build reuse is off for this run: …`, `build: sharing is off for this run: …`: the build ran as
  usual. Tell the person what the line says.

**Across machines.** Where the org shares compiled code through its backend, `hushrun run NAME --
COMMAND` also reuses what the org's other machines compiled, Go packages among them, through Go's
`GOCACHEPROG`. `hushrun build share on|observe|off` sets how much this machine takes part, and is
the person's too. `hushrun build fetch` saves, in a sandbox's setup, what its later phases reuse
without the network.

## Test files one by one

With `tests: reuse` for a check in `hushrun.yaml`, a run of `hushrun run NAME -- COMMAND` that
can't skip the whole check leaves out the test files that already passed on the files they read.
`tests: observe` runs every test file and says what reuse would have done. `hushrun why CHECK
TEST_FILE` says whether a test file would run or be reused, and why, and changes nothing. When a
test file fails twice though it passed on the same files it read, reuse of that check's test files
is suspended, and only a person turns it on again, with `hushrun tests resume CHECK`, which refuses
a coding agent. Tell the person what the line said.

## Setting hushrun up

`npx hushrun init --yes` applies init's changes without asking. Without `--yes`, and without a
terminal, init prints its plan and exits with status 2. `npx hushrun init --dry-run` shows the plan
and changes nothing.

When the person wants no account, or only this machine, `npx hushrun init --local --yes` wraps the
checks and sets agents up with no sign-in and no backend: checks skip here, and CI runs every check
until someone runs `hushrun init` without `--local`, which keeps what is there and adds the rest.

If the person belongs to several organizations and none is named like the repository's owner,
init lists them, changes nothing and exits with status 2. Ask which one, then run
`npx hushrun init --yes --org <slug>`.

In a sandbox with no browser, a person gives you one variable, `HUSHRUN_AGENT`. Some sandboxes have
`HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY` instead. Without any, checks still run but share nothing:
say so instead of trying to sign in, which `HUSHRUN_AGENT` refuses anyway.

## Other commands

- `hushrun whoami` says where `hushrun run` records results, with which credential, and who the
  backend says you are. `hushrun doctor` checks the setup and says what to fix. Both change
  nothing, and are worth running when results aren't shared and nobody knows why.
- `hushrun login`, `hushrun logout` and `hushrun token` sign in, sign out and make agent tokens:
  they are the person's. Don't run them unless the person asks.
- The commands for deploys, such as `plan`, `attest` and `artifact`, are CI's: `hushrun --help`
  lists them.

## Keeping hushrun and its skill current

- **hushrun.** At most once a day, hushrun says when a newer release is out. Where a package
  manager installed it, the package is updated with it, such as `npm install hushrun@latest`;
  else `hushrun update` updates it. `HUSHRUN_NO_UPDATE_CHECK=1` turns the check off.
- **This repository's skill.** `hushrun init` writes it to `.claude/skills/hushrun/SKILL.md` and
  `.agents/skills/hushrun/SKILL.md`, and `hushrun skill update` updates both, then they are
  committed. Each copy says its revision, `hushrun-skill` in its frontmatter. A copy edited by
  hand, or one a newer hushrun wrote, is left as it is, unless the person asks for `--force`.
- **Your own copy.** `hushrun skill update --global` writes `~/.claude/skills/hushrun/SKILL.md`
  and `~/.agents/skills/hushrun/SKILL.md`, for repositories that have none. A copy from a skills
  marketplace is updated by that marketplace's own command.

`hushrun guide --format json` gives this as data: the guide, this hushrun's version and skill
revision, `update` when a newer release is known, and each copy of the skill with its state:
`current`, `older`, `newer`, `edited` or `absent`.

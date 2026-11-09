# The corpus: init on 100 real repositories

A survey of 100 open source repositories' CI (2026-11-03) measured how much of it `hushrun init`
lets skip: 96 of 823 test jobs. This harness measures it again, on the same repositories at the
same commits, every time it runs, so that each change to init shows what it changed, and the
assertions A1 to A10 hold on real CI files, not only on fixtures.

```sh
cargo xtask corpus                                 # build hushrun, fetch (cached), run, score
cargo xtask corpus --only n8n-io/n8n,pola-rs/polars
cargo xtask corpus --hushrun /path/to/hushrun      # measure another binary, such as a release
cargo xtask corpus --update-baseline               # accept this run as the new baseline.json
cargo xtask corpus --refresh                       # fetch the checkouts again
```

The first run needs the network: about 500 MB of partial checkouts, which take a few minutes and
stay in `$HUSHRUN_CORPUS_DIR`, else `cli/target/corpus/repos/`, out of git. Later runs need
nothing but the CPU: about three minutes on a laptop, a minute of it in the analyzer. The scoreboard is
`target/corpus/scoreboard.md`, with every number in `scoreboard.json`. The command exits 1 when an
assertion fails, or a job loses its skip against the baseline, unless `known.json` lists the case.
`cli-nightly.yml` runs it weekly and by hand, and uploads the scoreboard.

## What a run does

| Step | File | What it does |
|---|---|---|
| Build | `xtask/src/corpus.rs` | `cargo build --release -p hushrun`, unless `--hushrun` names a binary |
| Venv | | `target/corpus/venv`, with PyYAML pinned (6.0.3). Nothing else is installed |
| Fetch | `fetch.py`, `select_paths.py` | For each repository: `git init`, its origin, `git fetch --depth 1 --filter=blob:none origin <commit>`, sparse-checkout patterns from `select_paths.py` over `git ls-tree`, and the commit checked out on its default branch, as a clone leaves it. A finished checkout is kept |
| Init | `run_init.py` | `hushrun init --dry-run --no-browser --format json` in a copy of each checkout, with an empty `HOME` and XDG directories, no credentials, no CI variables and no update check; 180 s each |
| Again | `run_init.py --edits` | The same, on copies that hold the first run's edits (A6) |
| Read | `analyze.py`, `patterns.py` | Each job's facts, from the CI files, with the harness's own reader (PyYAML and the survey's regular expressions) |
| Score | `score.py` | One outcome per test job, the walls, the assertions, the delta, the scoreboard |

`corpus.json` pins each repository's commit and default branch. `python3 -I fetch.py repin
--corpus corpus.json` moves them to the default branches' heads on purpose, which invalidates the
baseline: the next run says so, and `--update-baseline` takes the new one.

Nothing in a checkout runs. git only reads objects, with hooks off and no configuration of the
machine's or the repository's; the scripts read checkouts by the paths they are given, under
`python3 -I`; init's dry run signs in to nothing, writes nothing, installs nothing and sends
nothing (C-15, C-16).

## The test jobs, and their outcomes

A **test job** is the survey's: a job with at least one test command (the analyzer's
`is_test_line`), whose workflow runs on `push`, `pull_request`, `merge_group` or
`pull_request_target`, or is a reusable workflow one of those calls. There are 823.

Each gets exactly one **outcome**, from init's plan (`hushrun.init-plan/v1`). A job counts as
wrapped when init wraps a test command in it, or a test script it runs: the check's name or its
command matches the survey's `TESTY` words (`test`, `spec`, `pytest`, `nextest`, `ctest`,
`check-`, `unittest`, `rspec`, `phpunit`, `e2e`).

| Outcome | Meaning |
|---|---|
| `skips` | init wraps its test command, and predicts CI can skip it on a pass of this job |
| `runs:reports` | wrapped, and CI runs it because a later step reads its reports (Phase 1) |
| `runs:variants` | wrapped, in a job with several variants and no identity |
| `runs:reusable` | wrapped, in a job CI names by its caller's workflow |
| `runs:cant-tell` | wrapped, and the run can't find or read its job |
| `stepped-aside` | a remote build cache already reuses its results (Phase 4): init leaves the job's command, or the package.json script it runs, to Turborepo, Nx, Gradle, Bazel or Pants, and the plan says `ciWhy: stepped-aside` for it. Such a command counts though its words say no test, as `./gradlew build` |
| `left:<reason>` | init leaves its test command: `variants`, `dollar-words`, `shell-operator`, `var-prefix`, `writes-report`, `advisory`, `cd`, `watches` or `other` |
| `js-not-wrapped` | a JavaScript script or tool init doesn't wrap there |
| `unseen` | init didn't see a test command in this job; from Phase 3 init reads a workflow another of this repository's running ones calls, so only a file other repositories call stays here |

The prediction is init's, made by the run's own rule (`variants::no_skip`), and A9 checks it
against real runs on every fixture.

The scoreboard also has the survey's **walls**, computed as its `export.py` did: matrix first,
then reusable workflow, then wrapped, left, JavaScript, and unrecognized. They are the view to
compare with the survey, and with a hushrun from before the JSON plan, whose text `score.py`
reads with the survey's own parser.

## The assertions

| | Holds when | In force |
|---|---|---|
| A1, reports | no wrapped test command in a job with a reader that isn't failure-only predicts `skips`, and the harness's readers and the CLI's agree, job by job, on whether a test job init's plan lists has one | from Phase 1: before it, init's plan names no readers, and A1 reads `not measured` |
| A2, variants | no wrapped command or script in a job with several (or unknown) variants predicts `skips` without identity | now |
| A3, reusable | no wrapped command or script in a job of a workflow that only `workflow_call` starts predicts `skips` without `HUSHRUN_WORKFLOW` | now |
| A4, secrets | no wrapped command holds `secrets.`, `github.token`, or a variable the job's `env` sets from them | now |
| A5, valid edits | every edit applies with `git apply`; every edited workflow still loads, keeps every job, step and key, and only gains setup steps, `id-token`/`contents` permissions, `hushrun run` in front of `run:` lines (and `env` before a line's own `NAME=value` words), and init's own env keys with the values it writes: `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}`, `HUSHRUN_INPUTS: ${{ toJSON(inputs) }}` and `HUSHRUN_WORKFLOW` naming its own file in a job's `env`, `HUSHRUN_REPORTS_READ` in GitLab's `variables`; package.json changes only `scripts`, `hushrun` and the hushrun dependency; a composite action of the repository's is held to a workflow's rules (Phase 4); turbo.json only gains hushrun's and CI's variables at the end of `globalPassThroughEnv`, and keeps every comment (Phase 5) | now |
| A6, idempotent | init run again on its own edits proposes none | now |
| A7, no crash | init exits 0 on every repository, within 60 s, with no `panicked` on stderr | now |
| A8, no regression | no test job that skipped in the baseline stops skipping | now; Phase 1 moves the survey's six report jobs to `runs:reports`, and Phase 4 the jobs a remote build cache keeps to `stepped-aside` (D7), and each list is in the scoreboard |
| A9, prediction equals decision | checked by `crates/hushrun/tests/init_fixtures.rs` on every fixture, not here | |
| A10, workspace graph | every package scope holds the workspace packages the harness finds it depends on | from Phase 5 |

`known.json` lists accepted exceptions, each with its reason; a case matches on its repository,
and its workflow and job when it names them. It leaves when what it excuses is fixed.

## The baseline, and how it compares with the survey

`baseline.json` is the scoreboard of the last accepted run, Phase 5's (below), taken on
2026-11-05: its totals, its walls and every test job's outcome, which A8 compares against. Before
it came Phase 4's and Phase 3's, the same day, and Phase 1's, taken on 2026-11-03; Phase 2 changed
no outcome.
Phase 0's baseline, hushrun 0.7.0 with the JSON plan, is where each phase's numbers start.

**The harness reproduces the survey.** Built from the tag `v0.6.0`, the version the survey ran
from npm, hushrun's text for all 100 repositories is the survey's `init_dry_run_outputs.txt`, but
for two lines of its environment: the npm package says `npx hushrun` in its advice, and the
survey's machine had `~/.cargo/bin` on `PATH`, so its plan offered to install hushrun there. The
harness then gives the survey's numbers exactly:

| Wall | Survey (0.6.0, npm) | Harness, 0.6.0 | Harness, 0.7.0 with the JSON plan |
|---|---:|---:|---:|
| Matrix job: no skip in CI | 263 | 263 | 263 |
| A command init doesn't recognize | 219 | 219 | 218 |
| Reusable workflow job: no skip in CI | 113 | 113 | 113 |
| A JavaScript script or tool init doesn't wrap | 107 | 107 | 107 |
| Wrapped by init: can skip in CI | 96 | 96 | 96 |
| init leaves its command | 25 | 25 | 26 |
| **Test jobs** | **823** | **823** | **823** |

The analyzer finds the survey's 2,480 workflow files, 6,175 jobs and 823 test jobs in 94
repositories. To measure a release again, build it from its tag beside this checkout, and give
the binary to the harness:

```sh
git worktree add --detach ../../hushrun-v0.6.0 v0.6.0
(cd ../../hushrun-v0.6.0/cli && cargo build --release --locked -p hushrun)
cargo xtask corpus --hushrun ../../hushrun-v0.6.0/cli/target/release/hushrun
```

**One job moves between walls, and five between outcomes, from 0.6.0 to 0.7.0, and none is a
change in what init does.** Built from the tag `v0.7.0`, hushrun's text is 0.6.0's on all 100
repositories, but for the version it pins and two lines about the org that 0.7.0 rewords. The
difference is the JSON's: init's text lists the CI commands it leaves only when it wraps one of
CI's commands, or no script; with a package.json whose scripts it wraps and no CI line, it says
nothing of them. The JSON plan lists them all. So TypeScript's `go-test` job, whose
`go test ./...` follows a `cd` init can't follow, is `left:cd` and on the "init leaves" wall,
where the text showed nothing; and three of Phoenix's matrix jobs, whose `mix test` lines init
leaves for their variants and their `$` words, are `left:*` instead of `unseen` (their wall stays
"matrix"). The fifth is n8n's `workflow-script-tests`, below.

| Outcome | 0.6.0 (text) | 0.7.0 (JSON), the baseline |
|---|---:|---:|
| `skips` | 96 | 96 |
| `runs:variants` | 6 | 6 |
| `runs:reusable` | 1 | 0 |
| `runs:cant-tell` | 0 | 1 |
| `left:*` | 94 | 98 |
| `js-not-wrapped` | 215 | 215 |
| `unseen` | 411 | 407 |

The six `runs:variants` are React's three, Svelte's, Prettier's and Tailwind's: a wrapped test
script in a matrix job. The one `runs:cant-tell` is n8n's `workflow-script-tests`, a reusable
workflow's job whose line init reads as the wrapped `test` script. Read from the text, by the
survey's walls, it is `runs:reusable`; the JSON says the run can't tell which code it is, since
the job checks the repository out with `useblacksmith/checkout`, which init doesn't count as a
checkout. CI runs it either way, and the line runs another package's script (below).

**The baseline's assertions.** A2, A3, A4, A5, A7 and A8 hold. A6 holds but for two known cases:
in PostHog and Codex, init wraps lines of CI's files and no script of package.json, so its first
run leaves package.json as it is, and a second run, on a repository now set up, adds the script
that installs the hook. C-15 (step 4) and C-24 list that script among what init writes unless it
is there, so the first run should have proposed it; `known.json` holds both until that is decided.

## Phase 1: a skip never breaks a report

Against Phase 0's baseline, ten test jobs change outcome:

| Outcome | Phase 0 | Phase 1, the baseline | Change |
|---|---:|---:|---:|
| `skips` | 96 | 91 | -5 |
| `runs:reports` | 0 | 8 | +8 |
| `runs:variants` | 6 | 6 | |
| `runs:cant-tell` | 1 | 1 | |
| `left:*` | 98 | 95 | -3 |
| `js-not-wrapped` | 215 | 215 | |
| `unseen` | 407 | 407 | |

- **Six stop skipping in CI, the survey's six:** a later step of their job reads what the check
  writes, so a skip would leave it nothing. n8n's `checks` (`ci-python.yml`) and
  `workflow-comparison` (`test-evals-python.yml`) upload coverage to Codecov; polars'
  `coverage-rust` and `coverage-python` report their coverage in a later step and upload it as an
  artifact; Transformers' `run_tests_templates_like` uploads its reports, and Mattermost's
  `test-mattermost-redux` its coverage, as artifacts. A8 lists them. In n8n's two, the format
  check, the type check and the linter still skip, since Codecov reads coverage only.
- **Two lines init left now run for their reports:** PostHog's `llm-gateway` uploads its test
  results, and Mattermost's `test-platform` its coverage, as artifacts. init wraps them, CI runs
  them every time, and laptops and agents can skip them.
- **numpy's `full` now skips:** init left its line, which writes HTML coverage, and no step of its
  job reads it.
- **Laravel's `windows_tests` is still left,** for its variants now: init read its
  `phpunit --no-coverage` as writing a report, which step 1.1 fixed.

On the survey's walls, "wrapped by init: can skip in CI" goes from 96 to 99, since it counts the
eight `runs:reports` jobs too, and "init leaves its command" from 26 to 23: the three lines init
wraps now. The plan's target, the baseline minus 6, counted the six alone; numpy's skip is one
more.

**Assertions.** A1 to A8 hold, A6 but for the same two known cases. A1 is measured from this
phase: no wrapped test command beside a reader that isn't failure-only predicts `skips`, and the
harness's readers and the CLI's agree on every test job init's plan lists.

## Phase 3: each variant is its own job

Against Phase 1's baseline, which Phase 2 left as it was, 53 test jobs change outcome:

| Outcome | Phase 1 | Phase 3, the baseline | Change |
|---|---:|---:|---:|
| `skips` | 91 | 133 | +42 |
| `runs:reports` | 8 | 11 | +3 |
| `runs:variants` | 6 | 0 | -6 |
| `runs:reusable` | 0 | 1 | +1 |
| `runs:cant-tell` | 1 | 1 | |
| `left:*` | 95 | 56 | -39 |
| `js-not-wrapped` | 215 | 215 | |
| `unseen` | 407 | 406 | -1 |

- **Matrix jobs skip per variant.** init writes `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}` and
  wraps their lines, so 31 jobs whose lines it left for their variants now skip, among them
  Tokio's, Laravel's, pydantic's core tests, Cargo's and Ollama's, and five of the six scripts that
  ran for theirs skip too: React's three, Svelte's and Tailwind's. Prettier's `test` and gin's
  `test` now run for their reports, which their variants hid, and so does pydantic's `test`, whose
  `uv run pytest` init now wraps beside the `make test` it leaves for its subshell: an artifact
  upload reads what it writes.
- **Called workflows skip per call.** Six jobs init didn't see, in files a running workflow of
  their repository calls with `uses: ./`, are wrapped and skip, with `HUSHRUN_WORKFLOW` (and
  `HUSHRUN_INPUTS` where the file takes inputs): next.js, Electron, Dify's two, Godot and duckdb's
  `xcode-release`. VS Code's `cli` is read now too, and left, since CI fills in its words.
- **polars' `test-python` runs:** `release-python.yml`, which runs only on releases, calls its file,
  and init predicts from the caller's view, where the run can't find the job; init writes
  `HUSHRUN_VARIANT` there, so its own pull requests' runs skip per variant.
- **Six jobs are `unseen` now,** pydantic's three third-party jobs, Mattermost's `tools-ci` `test`
  and duckdb's two `linux-release` jobs: their steps run outside this repository's checkout, as
  pydantic's checks BentoML out at the root. Their lines were `left:variants` only because that
  check came first; they were never this repository's checks.
- No job loses its skip (A8). The plan's target, 40 more `skips` than after Phase 1, is met: 42.

**Assertions.** A1 to A8 hold, A6 but for the same two known cases: a second init on its own edits
proposes none of these variables again. A2 and A3 are measured in earnest from this phase: every
job with several variants or a caller that predicts `skips` has `variantIdentity` `env`, or `named`
for GitLab's instances. A5 now takes init's own env keys, with the values it writes, and nothing
else in an `env`.

## Phase 4: init reads what CI runs

Against Phase 3's baseline, 105 test jobs change outcome:

| Outcome | Phase 3 | Phase 4, the baseline | Change |
|---|---:|---:|---:|
| `skips` | 133 | 201 | +68 |
| `runs:reports` | 11 | 25 | +14 |
| `runs:variants` | 0 | 0 | |
| `runs:reusable` | 1 | 2 | +1 |
| `runs:cant-tell` | 1 | 1 | |
| `stepped-aside` | 0 | 7 | +7 |
| `left:*` | 56 | 23 | -33 |
| `js-not-wrapped` | 215 | 213 | -2 |
| `unseen` | 406 | 351 | -55 |

- **Words CI fills in, judged from the file.** 28 jobs whose lines init left for their `$` words
  are wrapped. 14 skip, Tokio's 10 among them, whose `$TOKIO_STABLE_FEATURES` the workflow's
  `env` sets and whose `${{ matrix.* }}` the matrix lists. 14 run for their reports, Deno's 12
  `cargo test -p ${{ matrix.test_package }}` among them, since an artifact upload reads what they
  write. Four more skip whose `&&` and `||` sit inside `${{ }}`, which no shell sees: Zed's three
  `run_tests_*` and Laravel's `linux_tests`. Three stay left, with the variable named: an earlier
  step sets Prometheus's `$TestTargets`, Quarkus's `$CHANGED_PATHS_ARG` and Phoenix's `$FLAGS`,
  and init can't tell what they hold.
- **A variable set first.** Five jobs whose line starts with `NAME=value` skip: pandas'
  `Linux-LSan`, Terraform's `e2e-tests`, Prometheus's `test_go_386`, Grafana's `pgvector` and
  Rust's `calculate_matrix`. Their wrap is `hushrun run <name> -- env NAME=value <command>`.
- **More runners, and make targets by their recipes.** 44 jobs init didn't see now skip:
  - `ctest`, in 13 of llama.cpp's;
  - `python -m unittest`, in 13: PyTorch's three, Codex's four, Mattermost's two, and Django's,
    Gemini CLI's, Dify's and PostHog's (LangChain's `build`, in a called workflow, runs);
  - `uv run` with its own options before the command, such as `--locked`, `--no-sync` or
    `--group dev`: Airflow's four, pydantic's two `core-test-*` and Dify's `vector-backends`;
  - make targets read by their recipes: Gitea's seven (`make test-backend`,
    `make test-migration`), two of them through the composite action
    `./.github/actions/pgsql-shard`, and Prometheus's `test_ui`;
  - `bundle exec rake` in Rails' two, and `flutter test` in Immich's `check-mobile-patches`.

  Codex's `sdks` skips too, for its `uv run ... pytest`, beside the JavaScript test init still
  doesn't wrap.
- **A remote build cache steps aside (D7).** Seven jobs: Spring Boot's three and Spring
  Framework's two, whose `./gradlew` runs in `./.github/actions/build` with the Develocity plugin
  and Gradle's caching on; LLVM's `bazel-build`, whose `bazelisk test` passes `--remote_cache`;
  and Nx's `main-linux`, whose `nx affected` Nx Cloud serves. init leaves their commands to the
  tool, and the plan says why.
- **Eight jobs are seen now, and left with a reason:** Dify's four API test jobs, for
  `${PYTEST_TIMEOUT:-20}`, a default the shell fills in; Transformers' `run_doctests` and
  pydantic's `test-pydantic-xml`, for a `$(...)`; Airflow's `tests-shared-distributions`, for a
  subshell; and llama.cpp's `ubuntu-24-openvino`, for a `cd`.
- **Four are `unseen` now:** Transformers' three vLLM jobs in `vllm-ci-caller.yml`, and pydantic's
  `test-langchain`. Their lines run in another repository's checkout, as Phase 3 found for
  pydantic's other third-party jobs.
- **Composite actions.** The survey estimated 62 test jobs. The harness finds 21 whose test line
  is inside a local action: Gitea's two shards skip, Spring's five step aside, seven run a
  JavaScript script or tool init doesn't wrap (Phase 5's), and seven a command init doesn't
  recognize, numpy's five `spin test` among them.
- **Suggestions.** init suggests 101 test scripts in 34 repositories, such as Redis's
  `./runtest`, for a person to wrap. It applies none, and the harness counts none.
- No job loses its skip (A8).

**The targets.** 68 more `skips` than after Phase 3 meets the plan's 60. The survey's "command init
does not recognize" wall, 219 at the start and the plan's `unseen` target, is 182, above the
plan's 150:

| Repository | Jobs | What init doesn't recognize |
|---|---:|---|
| Redis | 34 | `./runtest`, which init suggests and doesn't wrap. Without them, the wall is 148 |
| llama.cpp | 27 | `time cmake --build build --target test-fusion -j$(nproc)` and the like, `./ci/run.sh` and `./tests.sh` |
| numpy | 23 | `spin test`, numpy's own runner |
| Django | 8 | `python -Wall tests/runtests.py` |
| Node.js | 7 | `make test-ci` and its kin, whose recipes run `tools/test.py`, directly or through other targets |
| 36 others | 83 | five or fewer each |

**Assertions.** A1 to A8 hold, A1 but for three known cases and A6 for three. A4 holds on every
wrapped command: none passes a secret on its command line.

- **A1:** Spring Framework's and Spring Boot's `build-and-deploy-snapshot`, and Spring Boot's
  `ci`. The harness counts a Develocity build scan as a reader, and the CLI's readers are steps
  that read report files. Their checks step aside, so no skip depends on it.
- **A6:** PostHog's and Codex's, as before, and Immich's: init wraps CI's lines and no script of
  package.json, so a second run adds the hook's install script.

## Phase 5: a workspace package is a scope

Against Phase 4's baseline, 24 test jobs change outcome:

| Outcome | Phase 4 | Phase 5, the baseline | Change |
|---|---:|---:|---:|
| `skips` | 201 | 221 | +20 |
| `runs:reports` | 25 | 28 | +3 |
| `runs:variants` | 0 | 0 | |
| `runs:reusable` | 2 | 2 | |
| `runs:cant-tell` | 1 | 0 | -1 |
| `stepped-aside` | 7 | 7 | |
| `left:*` | 23 | 23 | |
| `js-not-wrapped` | 213 | 191 | -22 |
| `unseen` | 351 | 351 | |

- **A package's script runs through hushrun now.** 20 jobs that ran a script of a package init
  didn't read skip, and 3 run for their reports: Supabase's 8, PostHog's 5, Immich's 3, Dify's 2,
  and Astro's, cal.com's, Prisma's, Tauri's and Discourse's. CI starts them three ways:
  - a filter, such as `pnpm --filter @immich/scripts test`, in Immich, PostHog, Prisma, Dify and
    Supabase;
  - a root script that runs each package's, such as Astro's `test:language-tools:no-vs`, whose
    name isn't among init's, and Supabase's `test`;
  - a line in the package's directory, such as PostHog's `pnpm test:unit` in
    `services/credentials-service`.
- **Packages get scopes:** 59 in 12 repositories, Astro's 22, Supabase's 19 and VS Code's 5
  among them. PostHog's and Prisma's workspaces read as one scope here, since a package they depend on is past the
  checkout's 400 package.json files (below); Immich's and Dify's CI runs a check in a directory
  with no manifest init reads; one of cal.com's packages names the whole repository; Tauri's is a
  Cargo workspace too. Their packages' scripts still run through hushrun, in the one scope.
- **n8n's `workflow-script-tests`** runs `npm test --prefix=.github/scripts`, the script of a
  package outside the workspace, which init doesn't read. It read as the root's `test` before
  (`runs:cant-tell`), which the root's script, wrapped then, didn't run.
- No job loses its skip (A8).
- **turbo.json passes hushrun's variables on.** Five of these repositories run a package's script
  through `turbo` in CI: Astro, cal.com, Prisma, Tailwind CSS and Next.js. Turborepo 2's strict
  mode would keep CI's ID token and hushrun's variables from the script, which would then run
  every time, so init adds `ACTIONS_ID_TOKEN_REQUEST_*`, `GITHUB_*` and `HUSHRUN_*` to their
  turbo.json's `globalPassThroughEnv`: a new list in Astro's, cal.com's and Prisma's, Prisma's
  with comments; after the entries of Tailwind CSS's, on its line within its `printWidth` of 100,
  and of Next.js's, an item a line past 80 columns. The outcomes stay as they are: they counted
  these jobs' skips already, and with the edit the run gets what it needs to skip. A5 checks each
  edit: only the list grows, by these, and every comment stays.

**The target.** The plan asks for 40 of the 188 JavaScript test jobs: 28 skip, from 15. The rest:

| What stops them | Jobs | Most in |
|---|---:|---|
| A root script outside init's ten names, such as `test-unit`, `test:ci` or `test:types` | 62 | Supabase 10, Prisma 8, ESLint 5, Prettier 5, axios 5, cal.com 5, Gemini CLI 5, Angular 4 |
| A line no package manager starts, such as VS Code's `./scripts/test.sh`, which init suggests | 38 | VS Code 31 |
| A package's script outside the ten names, such as `pnpm -C packages/zone.js jest:test` | 15 | n8n 3, Gemini CLI 3, Angular 2, ESLint 2 |
| A package.json outside the workspace's patterns, or a check no test word names, such as Vue's `check` | 14 | VS Code 4, React 3, Prettier 2, cal.com 2 |
| A script init leaves, such as Svelte's `check`, which uses `cd` | 3 | Svelte, n8n |

The first and third rows are the plan's second blocker, "CI calls scripts outside init's list",
which no step of Phase 5 changes: the list of ten names is C-15's.

**Assertions.** A1 to A10 hold, A1 but for the same three known cases. A6's three known cases,
PostHog's, Codex's and Immich's, pass now: each first run edits package.json for its packages'
scripts, so the hook's script comes with it, and `known.json` drops them. A10 is measured from this
phase: the 59 package scopes hold the workspace packages the harness's own reading finds they
depend on, by name in their four dependency fields.

## What init can't see yet

The harness measures init as it is. These limits of init's reading show in the corpus, and no
job of the baseline's 221 `skips` depends on them:

- **Only `actions/checkout` checks the repository out.** A job that checks it out with another
  action, such as n8n's 32 steps of `useblacksmith/checkout`, is to init a job that checks
  nothing out: init wraps none of its lines, and a wrapped script it runs predicts `runs` with
  `cant-tell`.
- **A package.json outside a workspace's patterns isn't read**, though CI runs its script in its
  directory: React's `fixtures/dom`, VS Code's chat library. Its jobs are `js-not-wrapped`.
- **The checkouts are partial.** A checkout holds at most 400 package.json files and no tsconfig,
  so a workspace with more packages, PostHog's and Prisma's among them, reads here as one scope
  where it would split on a full clone (a package named by a dependency is missing), and a
  tsconfig's `extends` and `references` aren't seen. The harness's own reading of the graph (A10)
  leaves out the same files.
- **GitLab's `parallel:` counts only on the job itself.** One that a job gets from `extends:` or
  a YAML anchor reads as one variant to init. Since Phase 3 that costs nothing: the run tells
  each instance by its job's name, which differs per instance, so it skips per instance as init
  predicts. No job of the corpus has one: git's GitLab jobs that extend `.msvc-meson` run nothing
  init wraps.

A fixture for each, and the fix, belong to the phase that reaches them; the corpus shows the
change when it comes.

## Porting from the survey

The survey's scripts (`method/`) hard-coded a scratch directory; here every path is a parameter.
`select_paths.py` and `patterns.py` are unchanged. `analyze.py` keeps the survey's reading and
classification as they were, and adds what the plan's later phases measure: which local composite
action a test line came from, and which of a job's variables a secret sets (A4). `extra.py`'s
repository facts are folded into it. `aggregate.py`'s text parser is `score.py`'s
`parse_init_text`, unchanged, and `export.py`'s categories are its walls.

The survey kept 12 characters of each commit; `fetch.py resolve` found the full ones once, from
each default branch's history, and `corpus.json` keeps them with the branch.

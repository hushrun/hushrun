# 0047. A skip never breaks a report

- **Status:** accepted
- **Date:** 2026-11-03
- **Rules:** C-5, C-14, C-15, C-22, C-26, §10.1

## Context

A skip runs nothing, so it writes no report. In a survey of 100 repositories' CI (2026-11-03), 145
of 823 test jobs have a later step that reads what the test wrote, in the same job: a coverage
upload, a test reporter, an artifact. After a skip:

- `dorny/test-reporter` fails on no results, since `fail-on-empty` is true by default;
- Codecov compares the pull request with an older base, or waits for uploads that never come
  (`after_n_builds`);
- an artifact upload with `if-no-files-found: error` fails;
- test analytics count fewer runs, and say nothing.

init meant to leave such lines as they are: `writes_a_report` reads a command's report flags. It
missed pytest-cov's `--cov`, as n8n's `ci-python.yml` writes it, `uv run pytest --cov=src
--cov-report=xml` before `codecov/codecov-action`, so init wrapped the line, and a skip left the
upload nothing to send. It also read `--no-coverage` as writing a report. And leaving a line costs
more than CI's skip: a laptop or an agent that runs the same check loses its skip too, though
nothing there reads the file. What reads the reports is the job, not the command.

## Decision

1. **init reads report flags right**, a fix that can ship alone: pytest-cov's `--cov`, `--cov=…` and
   `--cov-…`; RSpec's JUnit formatter, a value of `--format` or `-f`, and `--require
   rspec_junit_formatter`; .NET's coverage collectors (`--collect`), coverlet's
   `CollectCoverage=true` and `--results-directory`; and flags about reports, but for a reporter
   whose value only prints (`dot`, `spec`, `list`, `verbose`, `default`, `progress`, `min`, `tap`,
   `summary`, `basic`). A flag that turns a report off (`--no-cov`, `--no-coverage`,
   `--coverage=false`) writes nothing.
2. **One catalog of the steps that read reports,** `cmd/setup/readers.rs`, for init, `ready`,
   `doctor` and the run, so that they can't disagree: actions by name, each with the inputs that
   hold paths as its own `action.yml` names them; artifact uploads whose path names a report;
   actions it doesn't know whose input names a report file; commands that read reports in `run:`
   steps; and local composite actions, one level deep. A step that runs only on `failure()` never
   runs after a skip, and doesn't count. Adding a reader is adding a row, with an example the tests
   read.
3. **In CI, the run decides** (C-26). On GitHub Actions, once the lookup finds a pass that counts
   in this job, a check doesn't skip when a step of the job reads reports: it runs, records as
   usual, and says `a later step reads its reports (codecov/codecov-action), and a skip writes
   none`. A linter, formatter, type checker or vet still skips when every reader reads only
   coverage or test results, which its skip leaves as they were: the table of checks gains a
   `kind`, and a command it doesn't know counts as a test. The run reads the workflow file it runs
   in once, for this rule and the variants rule (ADR 0044); when it can't find its job, it runs
   already (C-14).
4. **Where nothing reads the file, a variable says it.** `HUSHRUN_REPORTS_READ=true` says a job
   reads reports, on any CI: every check of the job runs, since nothing says what the readers read.
   Nothing reads `.gitlab-ci.yml` at run time, so init writes the variable into the GitLab jobs it
   sets up that have `artifacts:reports` but `dotenv`, which passes variables to later jobs, or
   `artifacts:paths` that name a report. CI that init doesn't edit (CircleCI, Azure Pipelines,
   Buildkite, Jenkins, AppVeyor) is read as text: a package.json script that writes reports, run
   beside a reader there, is left, since hushrun can't see a skip there.
5. **init wraps CI's lines that write reports.** A laptop or an agent skips them on a pass; CI runs
   them every time when their job reads reports, and skips them when it doesn't. init's plan
   predicts which, with the run's own code, and its text says `CI runs <check> every time: a later
   step reads its reports (<reader>). Laptops and agents skip it on a pass.`
6. **A repository can let CI skip anyway:** `checks.<name>.reports.ci: skip`, when its readers
   handle a skip, such as an upload gated on the step's outcome. `reports.files` lists report files
   hushrun wouldn't find, for summaries. Neither is part of the check's definition (E-6).
7. **`ready` and `doctor` say so.** `ready` gives such a check `ci`, with why, as it does for a job
   with several variants; `doctor` lists them in a `reports` check, as `ok`, since it is safe.

## Consequences

- **CI runs the checks whose reports a later step reads,** and the uploads, reporters and artifacts
  after them get what they read. Six of the survey's 96 jobs that skipped stop skipping in CI:
  n8n's two, polars' two, Transformers' and Mattermost's. What CI spends on them, it spent before
  hushrun.
- **Laptops and agents keep skipping them.** Nothing on a laptop reads the file, and a skip there
  says so.
- **A reader the catalog misses still gets nothing after a skip:** an action it doesn't know whose
  inputs name no report file, or a script that reads reports. Each is a row to add, or
  `HUSHRUN_REPORTS_READ=true` in the job. A reader it adds wrongly costs a skip in CI, never a
  report.
- **The workflow decides, as it is when the run reads it.** A step added after init counts at once;
  init's plan only predicts.
- **An older hushrun skips where this one runs.** A repository on an older release keeps the old
  behavior until it updates, and an older hushrun refuses a `hushrun.yaml` with `reports`, as it
  does any key it doesn't know.
- **What comes next:** a pass records a summary of its tests and coverage, and a skip shows the
  summary of the pass it stood in for. Then `reports.ci: skip` is a choice a repository can make
  knowing what its readers get.

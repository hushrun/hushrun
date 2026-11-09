# 0048. A run keeps a summary of its reports

- **Status:** accepted
- **Date:** 2026-11-04
- **Rules:** C-5, C-10, C-22, C-26, C-27

## Context

A skip runs nothing, so it writes no report (ADR 0047). CI now runs a check when a later step of
its job reads the reports, but a laptop, an agent and every job without such a step skip on a
pass, and say only that it passed: not how many tests ran, how many were skipped, or how much of
the code they covered. A person who reads a skip has to trust a pass they can't see.

Build tools that cache results answer this by putting the files back: Nx, Turborepo and Gradle
keep a run's outputs and restore them on a hit. That needs storage for every output of every run,
and an upload of each one. hushrun keeps no files: a result is a small signed record, and the
hosted service charges for checks, not for storage. Uploading reports would also send what they
hold, test names, messages and paths, which a repository may not want to leave the machine.

## Decision

1. **A pass keeps a summary of its reports, never the reports.** After the command exits, pass or
   fail, `hushrun run NAME -- COMMAND` finds the test and coverage reports this run wrote and
   records their summary in the result's `reports` (C-27): test counts (total, failed, errors,
   skipped, how long they took) and coverage (lines or statements, and branches), with each file's
   path and format. Never a test's name, a message or a file's contents; at most 4 KiB. No report
   file leaves the machine.
2. **Formats are told by their content, never by their names.** JUnit XML and TRX for test results;
   LCOV, Cobertura, JaCoCo, Clover, OpenCover, Go's coverprofile, coverage.py's JSON, istanbul's
   summary and SimpleCov for coverage. Test results are summed over every file; coverage comes from
   one format only, the first a run wrote in that order, so coverage written in two formats isn't
   counted twice. XML is read as a stream, with no DTD and no entity expansion. A new crate,
   `hushrun-reports`, does it, with nothing but reading files, so that it can be fuzzed; an
   independent Python reference, `tools/refreports.py`, agrees with it on every vector.
3. **The files are those the run wrote:** the command line's own (`--junitxml=…`,
   `-coverprofile=…`, …), hushrun.yaml's `checks.<name>.reports.files`, the paths the CI job's
   readers read, and the places tools write to by default; regular files inside the repository,
   modified since the run started. The search is bounded, and a summary past its limits is no
   summary, never a partial count.
4. **A skip carries the summary of the pass it stood in for,** as it carries that pass's duration,
   and shows it: on its line, in GitHub's step summary, and in GitHub's outputs, which every run
   writes (`hushrun-result`, `hushrun-skipped`, and the counts).
5. **Wherever a pass is shown, so is its summary:** `hushrun ready`'s `passed` item, the
   dashboard's run, its history and its view.
6. **A repository that handles a skip lets CI skip** with `checks.<name>.reports.ci: skip`
   (ADR 0047): a skip writes `hushrun-skipped=true`, so a reader can be gated on it, and init's
   notes for Codecov say when that is safe.
7. **The readers rule narrows with what the job's own pass wrote.** A record says `filesComplete`
   when its `files` names every report file the search found, each read, and the search went
   everywhere the job's readers read: no link, and no directory too deep or unreadable, there. A run
   that found none records just that. When the deciding pass is this very job's on GitHub Actions,
   recorded by this version of hushrun, with `filesComplete`, in a checkout that is the job's
   workspace, and the workflow and the local actions its steps use are in the check's scope, a
   reader that reads only the paths it names, none of them a file the pass wrote or a directory
   above one, doesn't concern the check: in a job with two test checks and an upload of one's
   coverage, the other skips after its first CI pass there. A reader that names no path, reads
   other reports, is a command or an action the catalog doesn't know, or looks for more of its own,
   as Codecov's actions do unless `disable_search: true` or a `directory` bounds them, still
   concerns it, since hushrun can't tell what it reads (C-14).
8. **The server validates `reports` and answers it.** A record with an invalid one is refused, 400
   `invalid`, since its signature covers the body as sent; an older server keeps it as sent. The
   API gives each run's `reports` and `job`, and each check's `last_pass_reports`.

## Consequences

- **A skip says what it stood in for:** `hushrun: unit skipped: this exact code passed in CI 3
  hours ago (saves 1m 21s): 1,204 tests, 14 skipped, 87.3% of lines covered`.
- **What isn't kept stays on the machine:** names, messages, stack traces, per-file coverage and
  the files themselves. A summary is counts; to see which test failed, a person runs the check
  again with `--force`, or reads CI's own reports.
- **Readers still need a run in CI.** A summary is not a report: Codecov, a test reporter or an
  artifact upload needs the files, so ADR 0047's rule stands, narrowed by item 7.
- **Old CLIs and old servers read these records as before,** since readers ignore fields they
  don't know (C-10): an old server keeps `reports` as sent, an old CLI shows no summary.
- **A run that writes coverage of two code bases in two formats shows one of them,** the first in
  the order above.
- **Codecov's uploads keep their checks running in CI** unless the step bounds what it reads with
  `disable_search: true`, since Codecov adds the coverage it finds itself to `files`. init's notes
  say how to let CI skip such a check, and when that is safe.
- **init predicts that a check runs for its reports** as for a pass that wrote what the job's steps
  read: what a pass writes is known only once it ran. A check that writes none of it skips in CI
  after its first pass there, though the plan said it runs.
- **Each new version of hushrun runs such a check once in each job** before the rule narrows again,
  since another version may read the job's steps, or search for reports, otherwise.
- **The search costs a walk of the repository after every run,** bounded by 200,000 entries and a
  second; a repository past that shows no summary.

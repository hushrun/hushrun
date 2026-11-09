# 0044. A CI pass belongs to its job, and a job with variants doesn't skip

- **Status:** accepted; amended by [0049](0049-each-variant-is-its-own-job.md) on 2026-11-05:
  items 1, 3, 4, 7 and 8, and the Consequences. A lookup asks for the newest result that counts in
  the job, a job of several variants says which one runs in `HUSHRUN_VARIANT`, a called workflow's
  jobs name their file in `HUSHRUN_WORKFLOW`, and GitLab's instances are each their own
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-11-02
- **Rules:** C-9, C-10, C-12, C-14, C-15, C-22
- **Amends:** [0037](0037-any-language-from-cis-own-files.md), which let a pass in CI stand in for
  any other job's run

## Context

A wrong skip is hushrun's one unforgivable mistake, and 0.5.0 made it. init wrapped
`"test": "node t.js"`, which a matrix job over two systems and two versions of Node ran. The run key
holds no environment (C-9), so the first variant to pass skipped the rest. In a scratch repository
the same code passed on Node 16, then printed `skipped` and exited 0 on Node 25, where the check
itself exits 1. Two jobs with different `runs-on` have one key too, and skip the same way.

A first attempt worked out from CI's YAML which jobs were the same: it compared setups as text, and
followed runs through scripts. Reviewed against real workflows, nearly everything wrong with it came
from that inference. Following hushrun's own advice recreated the wrong skip, words that never reach
the key counted as telling variants apart, a directory written as `${{ github.workspace }}/web`
matched nothing, and a release workflow's `setup-node@v4` ended skipping for push CI.

CI already says which job a run is in. Nothing needs inferring.

## Decision

1. **The key doesn't change.** A step can't see its matrix, and a key that held the environment
   would end what C-9 exists for, a pass on a laptop standing in for a run in CI.
2. **A CI pass belongs to its job.** A result names its `job` as `<job>@<platform>`, such as
   `test@linux/amd64`. `<job>` is what CI calls it, `GITHUB_JOB` or `CI_JOB_NAME`, without the
   workflow's file. `<platform>` is measured from the running binary with `std::env::consts` and
   written as E-7 writes it, so it needs no `RUNNER_OS` and works on every provider. It is the only
   measured thing in `job`. For a run with a `job`, a pass recorded by CI counts only when its `job`
   is this run's. So a staging and a production workflow with one job id on one system share a pass,
   and a `test` job in a macOS nightly doesn't reuse a Linux `test` pass. This needs no parsing, so
   it holds for a CI line, a script, `make` or a nested run, and however a job is set up. Local
   passes count as C-9 says. A pass from a named CI provider with no `job`, from before this, doesn't
   count, which costs one run, once. `provider: generic` with no `job` stays as it was. Lookup and
   protocol don't change: a server already answers a result as it was sent.
3. **A job with several variants doesn't skip.** One job has one `job`, so rule 2 can't help. On
   GitHub Actions hushrun reads the file `GITHUB_WORKFLOW_REF` names, as YAML, and finds
   `jobs.<GITHUB_JOB>`. With several variants, or a matrix that can't be counted, no pass skips. On
   GitLab, `CI_NODE_TOTAL` above 1 or a `CI_JOB_NAME` that shows a matrix says so.
4. **When hushrun can't tell, no pass skips,** and a line says so: the job isn't in the file, as with
   a reusable workflow, or the file is unreadable, doesn't parse, or isn't UTF-8. So too when a file
   this one calls with a local `uses:`, or one that calls in turn, holds a job with this id or can't
   be read: CI names the caller's file and the called file's job, so a plain job with that id says
   nothing about the job running. wrap.rs has always promised that whenever hushrun can't tell, it
   runs.
5. **A name that differs per variant counts, by NAME only.** A `run:` of the job holds
   `hushrun run <NAME>`, with `${{ strategy.job-index }}`, or `${{ matrix.x }}` for every dimension,
   and this run's name is one CI can make of it: the index fills in digits, and a key one of the
   values it lists. So `test-${{ strategy.job-index }}` claims `test-3`, and not `test-unit`, which
   an earlier version let skip. An expression in a list can't be told, so nothing matches it. And a
   run counts by its name only when it is that line of the workflow: the command line gives NAME, no
   package.json script started it, no line of the job writes that very name, and no other
   `hushrun run` started it. `hushrun run` sets `HUSHRUN_INSIDE_RUN=1` on the command it starts,
   and only this decides by it. So a script `test:1` inside `test-${{ strategy.job-index }}`, or
   `test:unit` inside `test-${{ matrix.suite }}`, runs whatever its name, and the outer run's pass
   still skips the whole of it. `--scope`, COMMAND, directories and words after a script don't
   count: those are what let wrong skips through, since they don't always reach the key.
6. **The advice is `hushrun run test-${{ strategy.job-index }} --`,** with `npx` in front when hushrun
   came from npm, since only the setup step puts it on a job's PATH. It is always a valid name, and
   47 of 71 real matrix packages had a value a name refuses, such as `18.x` or `macOS-latest`. An
   index is a position, so after a matrix edit it can mean another variant, keeping its key only if
   the workflow's file is outside the scope, which D-7 and `hushrun doctor` already warn about.
7. **init wraps a script a job with variants runs, as it would any other,** and gives its job the
   setup and permissions any job gets. Rule 3 already stops CI skipping there, so leaving the script
   alone would add no safety, and it would cost laptops and agents their skips: refusing took 134
   real packages from 168 wrapped commands to 116, and left 39 of them with nothing wrapped. CI runs
   every variant and records each, and laptops and agents skip. It is safe rather than wrong, so
   `hushrun ready` and `hushrun doctor` say so as information, with how to name each variant to skip
   in CI too. A CI file's own line stays as it is in such a job, as ADR 0037 says, since writing the
   line is init's choice of one key for every variant. ADR 0040 item 3 stands.
8. **`hushrun ready` is advice, and short.** A check in a job with variants is `ci`, through the
   scripts a job starts too, unless the newest result here failed, which stays `missing`, so that an agent doesn't push what it knows
   fails. A pass from another CI job isn't `passed`, and it is `ci` rather than `missing`: a run here
   would skip on that pass and record nothing, so doing what `ready` says would never end. `ready`
   compares jobs by name, since a job's platform is measured where it runs, and `run`'s rules are
   what decide.

## Consequences

The figures below were measured on real repositories, on 2026-11-02.

- **`job` holds no workflow file, because that would end staging-to-production reuse.** Staging then
  production on identical files is the shape hushrun was built for (plan §1). In a sample of
  application repositories, a few run one check in two workflow files on the push path, each a
  staging and a production workflow. In all of them, the two jobs have one job id and one `runs-on`, so with these rules they still share, as
  run against those files. Among 82 npm libraries whose CI starts a script, it was 1.
- **The limit: two workflow files with one job id on one system that differ in tool versions share
  a pass.** Nothing in `job` tells them apart. It is the trade C-9 accepts for a laptop's pass, and
  [`counts_from`](0045-a-check-says-where-it-counts.md) (C-25) narrows it: a run on a version its
  rule doesn't take leaves no pass. Without a rule, a different job id or check name keeps them
  apart.
- **When two jobs run one check, the newest result can hide a job's own older pass,** so both run.
- A laptop's or an agent's pass still skips every job without variants, as C-9 accepts, on whatever
  system those run.
- **A check in a matrix job no longer skips in CI** until someone names its variants. That costs
  runs, and it is what they were getting wrong before. Laptops and agents still skip it.
- **Reusable workflows don't skip in CI yet.** The job isn't in the file CI names, so nothing says
  how many variants it has. Many repositories call one, often an organization's shared workflows.
- **A job called from another repository can read as a plain job of the caller.** With the id of a
  plain job there, it skips as that job does, whatever variants it has: its file can't be read, and
  CI names only the caller. Refusing would end skipping for every repository with a plain job beside
  such a call: 180 such calls exist on that disk, and almost none collide. C-14 lists it.
- **A file a step starts isn't read,** so a `hushrun run` in a shell script or a Makefile that writes
  out a NAME a variant could take, such as `test-1` beside `test-${{ strategy.job-index }}`, counts
  as that variant's. C-14 lists it.
- **A matrix built by an expression skips only with `strategy.job-index`** in the name.
- **A repository that gets hushrun from npm has none of this until its lockfile moves past 0.5.0.**
  Only the setup action and the installers take the newest release (ADR 0035). Until then it keeps
  skipping wrongly, and only a change to the dependency fixes that.
- GitLab has no name that gets round rule 3, since nothing reads `.gitlab-ci.yml` at run time. Its
  `job` already differs per instance, so a CI pass could safely count for its own instance; only a
  local pass needs refusing. Rule 3 is the simpler statement, and that is a later change.
- A check run twice in one job, around a step that changes what it runs on, shares one key, and
  skips. C-14 lists this and the other limits.
- `hushrun run --stage` is unchanged: evidence decides there.

## Alternatives considered

- **Comparing jobs' setups from CI's YAML.** See Context. It runs more where text differs for no
  reason, and less where the difference is one it can't read.
- **The variant in the key.** No step can see its variant, and `RUNNER_OS` wouldn't tell two
  versions of Node apart.
- **The workflow's file in `job`.** Production's CI would run all 9 checks above that staging's CI
  passed, unless a person or an agent passed them first. That gives up what hushrun is for, to rule
  out a wrong skip none of the 9 can make, since their two jobs run on one system.
- **A job's id alone, without the platform, as `job`.** It keeps all 9 too, and lets a `test` job in a
  macOS nightly skip on a Linux `test` pass. The platform costs them nothing and rules that out.
- **`runs-on`, or `RUNNER_OS`, instead of a measured platform.** The first is a label, which says
  nothing about the system on a self-hosted runner, and the second is GitHub's alone.
- **Comparing COMMAND with the `run:` line,** to tell a variant's run from another of its name. A
  line's words can hold expressions and reach hushrun through a script, so it would lose skips, and
  this is the text matching item 5 exists to avoid. A variable on the child says it outright.
- **Only a warning.** It leaves every repository already set up skipping wrongly.
- **Leaving a script alone when a job with variants runs it.** See item 7. It also read every
  workflow, so a matrix in a nightly stopped wrapping for push CI, and it left the job with no
  `id-token: write`, so the advice it printed couldn't work as written.
- **Hand parsing the matrix, as these files are read to edit them.** The YAML parser in
  `hushrun-core` reads anchors and flow style for free, and only reads: edits stay line by line.

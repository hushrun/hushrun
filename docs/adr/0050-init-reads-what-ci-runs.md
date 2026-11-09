# 0050. init reads what CI runs, wherever it is written

- **Status:** accepted
- **Date:** 2026-11-05
- **Rules:** C-10, C-14, C-15, C-22
- **Amends:** [0037](0037-any-language-from-cis-own-files.md): items 1, 2, 3 and 6, its reading rules
  for CI's lines

## Context

After ADR 0049, 133 of the 823 test jobs in the survey of 100 repositories' CI (2026-11-03) skip in
CI. Of the rest, init couldn't see the command, or saw it and left it:

- 62 test jobs keep their command in a composite action of the repository, `uses:
  ./.github/actions/run-tests`, whose `action.yml` init didn't read.
- 66 commands have `$` words, such as `pytest ${{ matrix.args }}` or `go test $PKGS`, which init
  left: "the shell makes its words what they are".
- 10 commands start with `NAME=value`, such as `GOARCH=386 go test ./...`, which init left: "it sets
  GOARCH=386 first".
- Most of the 219 test jobs whose command init didn't recognize run a `make`, `just` or `task`
  target outside the 10 names it knew, such as Node's `make test-ci` and Prometheus' `make
  ui-test`; a runner the table lacked, such as `ctest`, `python -m unittest` or `./gradlew build`;
  or a test script, such as `./scripts/test.sh`.
- 19 repositories run Bazel, Gradle with a remote build cache, Nx or Turborepo, which reuse their
  own results across machines: a wrap would cache twice, and hide the tool's own reporting.

Leaving the `$` words was safe, and more than safety needs. A key is the words as the shell passes
them to `hushrun run` (C-9): GitHub fills in `${{ }}` before the shell starts, and the shell fills in
the rest, so the key is exact whatever filled them in, and `-m fast` and `-m slow` are two keys. What
a word may hold is the real question: a result records its command, so a secret among the words
would go to the backend.

## Decision

1. **A line whose words CI or the shell fills in wraps when each is safe to record** (amends 0037
   item 3). init reads the workflow as YAML to tell what each holds.
   - **Safe:** `${{ matrix.* }}`, `${{ strategy.* }}`, `${{ runner.* }}`; `${{ github.* }}` but
     `github.token`; `${{ vars.* }}`, `${{ steps.*.outputs.* }}`, `${{ needs.*.outputs.* }}`;
     `${{ inputs.* }}`, as item 3 says; and `${{ env.X }}`, `$X` and `${X}` when what sets X is
     safe: the step's `env`, an earlier step's write of a literal to `$GITHUB_ENV`, the job's `env`,
     then the workflow's, nearest first, each followed through; or the runner's own `GITHUB_*` and
     `RUNNER_*`, but `GITHUB_TOKEN`, and `HOME`.
   - **Left, with why:** a secret or `github.token`, or a variable set from either: `it passes a
     secret on its command line (<the word>), which a result would record`. A variable an earlier
     step writes to `$GITHUB_ENV` from anything but a literal, or that nothing in the file sets, and
     that isn't the runner's: `an earlier step sets <X>, and hushrun can't tell it holds no secret`.
     A command's output, `$(…)` or backticks: `a command fills in its words`. What the shell makes
     into other words, a glob, `$1`, `$@` or `${X:-y}`, as before: `the shell makes its words what
     they are`.
   - **The key doesn't change:** it is the words as they ran. Each value of a matrix, or of a
     variable, has passes of its own, which ADR 0049's variant narrows to its job's variant.
2. **Variables set in front of a command go through `env`.** `A=1 cmd` becomes `hushrun run <name>
   -- env A=1 cmd`: the variables are words of the command, so of its key, and `GOARCH=386` never
   shares a pass with `GOARCH=amd64`. Their values follow item 1. Only a bash step takes it: one
   whose `shell:`, or its job's or workflow's `defaults.run.shell`, is bash or sh, or GitHub's
   default on a runner that can't be Windows, where it is pwsh. A step that may run on Windows is
   left: `it sets <A=1> first`. Variables that only change the output, such as `NO_COLOR`, stay in
   the key too: that is simpler, and safe.
3. **init reads the composite actions of this repository a job uses** (amends 0037 item 1). For
   `uses: ./<dir>`, which counts from the workspace, where the job checks this repository out,
   `<dir>/action.yml` or `action.yaml` with `runs.using: composite` is read: its `runs.steps`, as a
   job's steps are, each with its own `shell:`, and its `working-directory` from the workspace. An
   action it uses is read too, two levels in all. An action of another repository, or of another
   kind, isn't read.
   - **The wrap goes into the action's file,** once, whichever jobs use it, under one name. A line
     is wanted when a job of a workflow a push or a pull request starts reaches it within two
     actions. Then every job init reads that runs it, a nightly's too, gets hushrun's setup above
     its step that uses the action, the ID token, and its identity (ADR 0049), as any job does: the
     line runs `hushrun` in all of them. So init reads every action a job reaches, however deep,
     one that uses itself once.
   - **It has to suit every one of them,** or it stays as it is for all, each told where: a job
     that leaves it leaves it for the others too, `where job <id> of <file> runs it, <why>`. It
     stays too when a job the workflows read as YAML say may use the action isn't one init reads
     as doing so, such as one written on one line, since that job would run `hushrun` without
     having it; when a workflow that doesn't read as YAML names its directory; and when the action
     isn't under `.github/`, where another repository may use it, whose jobs install nothing.
   - **A line's words are read where they run:** the action's step `env` and what its earlier steps
     write, then the job's, as item 1 says. An action's input comes from its default and from what
     the step that uses it gives in `with:`, read in that job: a job that gives it a secret, or a
     variable set from one, leaves the line. The line is one line, so it is left for every job that
     uses the action, each told where: `where job <id> of <file> runs it, <why>`.
   - **A workflow's own input** comes from a caller, whose `with:` GitHub doesn't let hold a
     secret, or from a person; its default is read too, since it may be `${{ github.token }}`. An
     input in a line is recorded as the command is, unlike `HUSHRUN_INPUTS`, which is only hashed.
4. **At run time an action's step is the calling job's.** GitHub names that job in `GITHUB_JOB`
   and its workflow in `GITHUB_WORKFLOW_REF`, and the job's `env` reaches the action's steps. So
   ADR 0044's job, ADR 0049's variant and ADR 0047's readers, which already read a local composite
   action, all apply to the job that uses the action; when several do, `job` keeps their passes
   apart. The run needs no change for it.
5. **The plan shows an action's line under each job that runs it,** with `action`, the action's
   file, and `line` that file's (init-plan v1: a property added, nothing changed).
6. **`ready` and the hook read composite actions too,** and judge their lines by the job that uses
   the action. A line through hushrun whose words CI fills in has a key only its run can build, so
   its check stays with CI: `its command takes values only CI knows` (amends 0037 item 6).
7. **A `make`, `just` or `task` target is read by its recipe** (amends 0037 item 2). A target the
   table doesn't name is a check when its recipe, read from the file the runner would read, with
   its prerequisites and the targets it runs, three levels down, runs a check the table knows, and
   nothing that changes something outside the machine: pushing an image or to git, publishing,
   releasing, deploying, calling a cloud, copying to another machine, or a request that changes
   something. Variables are read as the file sets them, and nothing runs. Such a target is named
   after itself, and wrapped. One that runs a check and does such a thing stays, saying what it
   does: `its recipe pushes an image (docker push)`; one beside a part init can't read stays with
   `its recipe can't be read`. A target whose recipe runs no check says nothing, as any command init
   doesn't know: the plan left every other target with a reason, and on the corpus that put `its
   recipe can't be read` on 125 lines that were never checks, such as `make proto`.
8. **More runners** (amends 0037 item 2). The table gains `python -m unittest`, Gradle's `build`,
   Maven's `install`, `sbt test`, `rake` with no task, `ctest`, `meson test`, `dart test`, `flutter
   test` and `deno test`, and a Maven or Gradle task after `clean`. A build or `install` with
   another task after it stays as it is, since that task could publish, and so does a Maven phase
   whose tests `-DskipTests` turns off. A launcher's own options are read past, as `uv run
   --no-sync pytest` has, the commonest unseen test command (24 jobs in 8 repositories); one it
   doesn't know leaves the command no check. Each row was checked against the corpus's unseen
   commands: it is in when two repositories run it, or it is a language's own runner.
   `python -m test` (CPython's) and `runtests.py` (Django's) are neither, and stay unseen.
9. **Test scripts are suggested, never applied.** In a job named for its checks, a line that runs a
   test script of the repository, such as `./scripts/test.sh`, is listed under "Could also wrap",
   with the line to use, and init changes nothing for it: a script can deploy or write reports,
   which a skip wouldn't do, and only its people know. A line whose words CI fills in isn't
   suggested, since whether it may hold a secret is init's to tell.
10. **A tool that reuses its own results across machines steps aside** (D7). Once Turborepo, Nx,
    Gradle, Bazel or Pants has a remote cache set up, init leaves what runs through it as it is, CI's
    lines and package.json's scripts alike, saying `<tool> reuses its own results across machines
    (remote cache), so init leaves it to <tool>`, ahead of any other reason. Gradle's counts with a
    remote `buildCache` in its settings, or the Develocity plugin applied there, directly or
    through a conventions plugin such as Spring's, with caching on, as the plan reads D7: a wrong
    guess leaves a line to Gradle, which runs it as before. Without a remote cache nothing changes,
    and hushrun adds what every machine shares. A person can still wrap such a line by hand.
11. **A recorded command hides the secrets hushrun can tell** (C-10). In the record, never the key,
   a word is `[redacted]` when it is the value, of 8 bytes or more, of a variable of the run's
   environment whose name holds `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `PASS`, `KEY`,
   `CREDENTIAL`, `AUTH`, `PRIVATE` or `CERT`, in any case; and when it is shaped like a token:
   GitHub's (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`), GitLab's (`glpat-`), Slack's
   (`xox` with one of `abprs` and `-`), an AWS access key (`AKIA` and 16 uppercase letters or
   digits), an API key (`sk-` and 20 or more letters, digits, `-` or `_`), or a PEM block
   (`-----BEGIN`). In a word with an `=`, what follows the first `=` that leaves a secret is
   redacted, so `--build-arg=NPM_TOKEN=<secret>` shows as `--build-arg=NPM_TOKEN=[redacted]`. The
   key holds the command as given (C-9), so what skips never changes, and nothing reads a key back
   from a record. It is a second line of defense, behind items 1 to 3.

## Consequences

Measured on 2026-11-05.

- **On the survey's 100 repositories, 201 of 823 test jobs skip in CI,** 68 more than after ADR
  0049, and 25 run for their reports, 14 more. The change: 32 jobs whose `$` words item 1 judges
  safe, 18 of which skip and 14 run for their reports; 5 whose line sets a variable first (item
  2); and 46 init didn't see, read now by their runner, a launcher's options or a make target's
  recipe (items 7 and 8), 45 of which skip. Seven step aside to a remote build cache (item 10).
  The survey's "command init does not recognize" wall is 182, from 219: above the plan's 150 by
  Redis's 34 `./runtest` jobs, which init suggests (item 9) and doesn't wrap. A4 holds: no wrapped
  command passes a secret on its command line. No job that skipped stops (the corpus harness, A8).
- **Older pieces stay safe.** An older CLI in a job that uses a wrapped action runs the line as
  any wrapped line. A server from before this answers as before.
- **Limits.**
  - A variable an earlier step writes to `$GITHUB_ENV` any other way than `echo "NAME=value" >>
    "$GITHUB_ENV"`, or `printf` of one, may hold anything, and leaves a line that reads it.
  - A composite step doesn't get the job's `defaults.run`, as on GitHub, and an action's
    `$INPUT_*` aren't set for it: a line that reads one is left.
  - A line three actions in, or more, isn't wrapped, though the action is read. An action of
    another repository, or of another kind, is unseen.
  - A repository whose `.github/actions/` another repository uses, with `uses:
    owner/name/.github/actions/<dir>@<ref>`, needs hushrun installed there too once a line of it
    is wrapped: init can't see that use.
  - A recorded command still shows a secret inside a longer word, such as `-pSECRET` or a URL's
    `user:SECRET@`, one an expansion split into several words, and a `${{ secrets.X }}` value no
    variable holds and no shape shows: items 1 to 3 leave those lines, and a line written by hand
    is its author's to keep clean. The broad names redact some values that aren't secrets, such as
    `SSL_CERT_FILE`'s, in the record only.
  - The plan's `before` and `after` show a command's words joined, without its quotes. The edit
    keeps every byte.
  - Two checks of one name in a job, the second named after the job, read `test-test` when the job
    is `test`, as before.

## Alternatives considered

- **Leaving `$` words, as before.** Safe, and it cost 66 commands their skips for a risk item 1
  reads in the file.
- **Wrapping any `$` word, and redacting the record.** Redaction is a second line of defense
  (item 9): a word shaped like nothing hushrun knows, or a short secret, would still go out.
- **Setup only in the jobs init wraps a line of.** A shared action's line runs `hushrun` in every
  job that uses it, and a nightly that didn't install it would fail on it.
- **Setup inside the action.** Its steps can't know whether the job installed hushrun already,
  and it would run once per use. The job installs it once, above the step, as for a line of its
  own, and the action's file only gains the wrap.
- **A copy of an action per job.** A wrap per job would edit one line differently for each, which a
  file can't hold, and a shared line is the job's at run time anyway (item 4).
- **Hashing an input's value in the key.** The key is the command's words (C-9): a hash in place
  of a word would change every key, which a contract that only grows can't.

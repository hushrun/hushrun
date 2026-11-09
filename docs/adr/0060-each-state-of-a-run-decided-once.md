# 0060. A run's marks, content and credentials, each decided in one place

- **Status:** accepted, and built (not yet released)
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-11-07
- **Rules:** C-9, C-13, C-18, C-22, C-27; plan §10.4 (`run --stage`, the plan as the single source)
- **Amends:** [0045](0045-a-check-says-where-it-counts.md)'s marks and item 8, and the unrecorded pass of
  [0032](0032-quota-refusals-fail-open.md)

The audit of implicit state machines (2026-11-06, sections 4 and 5) found states that several
places decided, each a little differently. Each part below is one of them, made one decision. The
plan's rule (§0.1) settles each doubt: what runs more checks or deploys less.

## 1. A run's mark

### Context

`<cache dir>/unrecorded/<key>` was three states, an absent file, an empty one and one with text,
moved by four functions from three places. Nothing removed a stage's mark: `run --stage` wrote one
for a pass the org's plan wouldn't record, and `ready` read the mark before it read whether the
check had failed. So after a pass over the quota and a failure recorded later, `ready` said the
check passed here and couldn't be recorded, and the pre-push hook let an agent push code it knew
failed. A wrapped run's mark had the same hole with two refused records: a pass's, then a
failure's.

### Decision

- The mark is one type, `results::Mark` (`Unrecorded`, or `DidntCount` with its reason), and one
  function, `results::ran_here`, makes every move, from how the run ended: recorded (removed),
  passed and not recorded (`Unrecorded`), failed (removed), or under a rule this machine doesn't
  meet (`DidntCount`). A stage's check moves it by its check event's fate.
- A failure here removes the mark, recorded or not: it is this machine's newest word on the key.
- `ready` reads a stage's mark only when the check's evidence isn't a failure: a failure is
  `missing`, whatever the mark says. A wrapped run's failure already came first.

### Consequences

A check that failed here last is asked for again, and the hook refuses an agent's push for it, as
for any failure. A mark costs at most one more run: after a failure, a run that passes and can't be
recorded leaves the mark again.

## 2. Where a reader reads that the search didn't go

### Context

A run's `reports.filesComplete` says the search went everywhere the job's readers read (C-27), so
that a skip leaves them what a run would (C-26). The search never enters `.git`, `node_modules`,
`.venv`, `venv` or `__pycache__`, and no wildcard of a reader's path goes there or into a hidden
directory through `**`, by design (ADR 0048's tests say so). But a reader's path that names one of
those five, such as `node_modules/.cache/junit.xml`, reads where the search never went, and the
record still said `filesComplete`.

### Decision

A directory the search never enters that a reader's path names word for word, with no wildcard
standing for it, leaves the list incomplete. Wildcards stay as they were: `**/junit.xml` doesn't
go into `node_modules`, and `*/junit.xml` doesn't either.

### Consequences

Such a check runs whenever that reader is in its job, as it would with any record that isn't
complete. Nothing changes for a reader that doesn't name one of those directories.

## 3. Whose credential a CI job's results carry

### Context

The repository's ledger takes no person's login in CI (L-10): `App::personal_token_for` returns
none there, and events in CI sign only with a key the job names (T-12). Results chose their
credential on their own (`Backend::server`), and fell back to the login saved for the backend's
origin wherever hushrun ran. On a self-hosted runner where someone once ran `hushrun login`, every
CI job's results went out as that person, and were signed with the key the sign-in left there.

### Decision

- In CI (§11.2's detection: a named provider, or `CI` without `HUSHRUN_AGENT`, unless
  `HUSHRUN_PROVIDER=local`), results send no saved login. What the environment names still goes:
  `HUSHRUN_AGENT`, `HUSHRUN_TOKEN` as C-13 says, and GitHub's ID token.
- A personal `HUSHRUN_TOKEN` in CI signs its results with the key the job names only, as events
  do; without one, the result isn't sent, and the run says what to set.

### Consequences

A CI job on such a runner reads and records as CI: with GitHub's ID token, or nothing, which the
run's last line says (C-11). Outside CI nothing changes.

## 4. `ready` registers no machine

### Context

A person's machine registers in an org with its first result there (C-18), unless a sign-in did.
`ready` asked whether this machine could sign through the same code a result uses, and that code
registered the machine, with `POST {base}/v0/machines` and the `hostname` it starts for its name
(ADR 0045 item 8 noted the `hostname`). So the pre-push hook, on any push of a repository whose
backend is another org of the person's, spent one of the person's machines there (hushrun cloud
takes 20), though nobody meant to record anything.

### Decision

`ready`, and so the hook, find the key that would sign without registering it: no key, an agent's
key the environment doesn't name, or, in CI, a personal token with no key the job names, is still
`ci`, with what to do. A key the server may not have yet is `missing` as any check, and the run
registers it with its result. `ready` starts no `hostname`.

### Consequences

When the server then refuses the registration, as 409 `machine_limit`, the run says so, records
nothing, and leaves its mark (C-22): `ready` asks for that run once, not never.

## 5. The content a stage's check runs on

### Context

`run --stage` runs a check's `run` on the files as they are, and records its evidence for the
digest of `HEAD`, `--rev`'s revision, `--digest`'s digest or a plan's. Nothing compared the two,
unlike `--isolated`, which exports the revision and refused a digest that wasn't the export's.
So a pass on uncommitted edits counted for `HEAD`, and `ready --rev X`'s command, `hushrun run
--stage <stage> --rev X <checks>`, recorded a pass for X on whatever was checked out. A plan file
checked the commit only: a plan made with `--worktree` and carried out after an edit, or at `HEAD`,
recorded evidence for a digest the files no longer had. A promotion could then rest on content that
never ran.

`hushrun run NAME -- COMMAND --rev X` is the same shape, but COMMAND is the caller's: hushrun
can't tell what it reads. A release gate can run
`hushrun run gate-<check> --config … --scope <scope> --rev <sha> -- bash gate.sh --one …`,
whose COMMAND tests a `git archive` export of `<sha>`, from a checkout that is legitimately
elsewhere: a release gates `main^` while `main` holds the version commit, whose `Cargo.toml` and
`CHANGELOG.md` are in the scope.

### Decision

- **A stage's check, without `--isolated`,** runs only on its subject's content, checked before
  anything runs, else exit 2 and nothing recorded: `--digest`'s digest must be the scope's here;
  and at a revision, each of the scope's files there must be in the worktree as it is at the
  revision (`git diff <commit>`: changed, deleted or of another kind). A file the revision doesn't
  have is left out, staged or not, since a CI job's own steps write files, such as
  `google-github-actions/auth`'s credentials file in the workspace, which a whole-tree comparison
  would turn into a failed job. The error says to check out the revision, commit or stash what's
  different, or pass `--worktree`.
- **Carrying out a plan** checks the content as well as the revision: each of the plan's scopes
  must digest here to the plan's digest. That covers `run`, `attest`, `mark`, `guard` and
  `artifact resolve`, for a plan made with `--worktree` too.
- **`hushrun run NAME --rev X -- COMMAND` and `--digest`** stay the caller's word, now said in
  C-9: hushrun reads no file here for them, before or after COMMAND. They are for a COMMAND that
  tests that code itself, as the gate does; a COMMAND that tests the files here runs without them,
  and is keyed on the worktree, with the after-run check it has always had. `check-local.sh` needs
  no change.

### Consequences

- `ready --rev X`, and the pre-push hook for a branch that isn't checked out, still name `hushrun
  run --stage <stage> --rev X <checks>` for a check that runs where it is, with the note to commit
  or stash; that command now exits 2, saying which file differs, until X's files are here.
  A script's or a CI file's command `ready` names carries no `--rev`, as before.
- A pipeline whose steps change a tracked file of the scope before `run --stage`, such as an
  `npm install` that rewrites the lockfile, now exits 2 there: its evidence was about other files.
- Not covered: a check that changes the scope's files while it runs (a wrapped run checks that;
  a stage's doesn't), and a file the revision doesn't have that a check reads.

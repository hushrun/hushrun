# 0067. Results stay on this machine until someone shares them

- **Status:** accepted
- **Date:** 2026-11-10
- **Rules:** C-11, C-13, C-14, C-15, C-17, C-22, C-23, C-24, L-14

## Context

`hushrun run NAME -- COMMAND` already works with no account: with nothing naming a server,
`Backend::select` ends at `<cache dir>/results`, a file ledger (L-3) that only this machine reads,
and a second run on the same files skips. Everything around it assumed an account:

- **`hushrun init` always signed in** (C-15 step 3). Someone who wanted a faster laptop and an agent
  that runs its checks before pushing had to make an org first, and init then wrote the org's URL
  into package.json or hushrun.yaml, gave CI's jobs `id-token: write` and registered a machine.
- **`hushrun ready` and the pre-push hook gave up on a directory.** Every wrapped check was `ci`,
  `results stay on this machine, so CI can't see a pass here`, even after it passed here, so the
  hook never held an agent's push to its checks without a server.
- **A teammate with no login got no reuse.** In a repository whose package.json or hushrun.yaml
  names a backend, a run with no credential asked the server, was refused (401), ran, recorded
  nothing, and said `sign in to share results`. The next run on the same code did it all again.
- **Nothing pruned the directory.** It only grew.
- **CI read and wrote the directory too.** A job with no server kept its results on the runner,
  and a cache step that saved `<cache dir>/results` between jobs gave a whole repository's CI
  shared reuse from unsigned files that anyone who can write that cache could forge, with no
  server, signature or org to say who recorded them.

And several commands decided "is this a server or this machine" for themselves: `ready` by
`Place::Directory`, `whoami` and `repository usage` by their own branches, `off_reason` by reading
`HUSHRUN_BACKEND` again, and `repository stats --local` by joining the cache directory itself.
Plain `repository stats` failed with `no repository URL` in a repository with only local results.

## Decision

1. **One function decides, every reader asks it** (C-11). `Backend::select` returns a `Selection`:
   `On(Backend)`, a server's or this machine's directory, which `Backend::place` and
   `Backend::local` tell apart; or `Off(Off)`, with why: `HUSHRUN_BACKEND` is empty, there is no
   cache directory, or (item 3) CI. `run`, `ready`, the hook, `whoami`, `doctor`, `repository usage`
   and `repository stats` read it from there. `Local` says why results stay here: `Unnamed`, as
   nothing names a server, or `SignedOut { base, named_by }`.
2. **A repository's backend with no credential at all is this machine** (C-11, C-13). When the
   repository's package.json or hushrun.yaml names the backend, nothing would go with its requests
   (no token goes there, `HUSHRUN_TOKEN` isn't withheld from it, no login for its origin, no
   GitHub Actions ID token, no ADR 0052 lookup as the repository's CI), no agent is set (C-17), and
   this isn't CI, results go to `<cache dir>/results`, and the run asks the server nothing. Its
   last line stays `hushrun: sign in to share results with <org>: npx hushrun login` at a
   terminal; elsewhere it is `hushrun: results aren't shared with <org>: nobody is signed in to it
   here, so they stay on this machine`. It never falls back for a backend `HUSHRUN_BACKEND` or
   `HUSHRUN_AGENT` names, for a token the server refuses, beside a withheld `HUSHRUN_TOKEN`, or in
   CI: each of those is someone's choice of a server, and a refusal is what they need to see.
3. **CI keeps no results in a directory** (C-11, C-14). In CI (`hushrun_ci::in_ci`), a selection
   that would end at the directory is `Off(InCi)` instead, and `run` says so once, before the
   command, unless `-q` or inside another `hushrun run`: `hushrun: <NAME> running without
   skipping: results are off: in CI, nothing names a backend, and a runner's own directory isn't
   shared: run hushrun init to share results with CI, or set HUSHRUN_BACKEND`. A repository's CI
   that names a backend is unaffected: it is a server, as before.
4. **This machine forgets a result 30 days after its run** (C-11). After each run, `run` sweeps its
   directory, at most once a day, by the command's clock (`--now` in tests): every file of
   `results/v0/<key>/` and `skips/v0/<key>/` whose name is a ULID older than 30 days goes, and so
   does a key directory left empty. The stamp, `<cache dir>/results/swept`, holds the time the last
   sweep began, written whole (temporary file and rename) before anything goes; a sweep within a
   day of it, by the same clock, does nothing, and a clock behind it sweeps by its own time. A
   stamp that can't be written means no sweep, so a broken cache never makes every run sweep.
   Nothing about it fails or slows a run, and what goes wrong is only logged. Several runs at
   once may each sweep: they delete the same files, never one newer than their cutoff, since a
   result's name is its finishing time; and a file ledger whose directory a sweep removed while it
   wrote makes it again, once.
5. **`ready` and the hook go by what passed here** (C-22, C-23). With results on this machine,
   each wrapped check is looked up in the directory by `run`'s own key (C-9), with no `as=ci`
   query: `passed` when its newest result is a pass, whatever job recorded it; `missing` with
   `it failed here on this exact code last time`, or `no pass here yet for this exact code`, and
   the command that runs it. Its text says `passed  <check>  here`, and `<code> isn't ready: N
   checks haven't passed here.`; its JSON has `"results": "local"` (or `shared`, or `off`). A note
   says `Results stay on this machine, so CI still runs these checks: hushrun init shares results
   with CI and teammates.`, or `hushrun login shares results with <org>'s CI and teammates.` for a
   repository that names its backend. The hook refuses a coding agent's push until they pass here,
   as with a server, and its lines say `haven't passed here` where they said `CI would run … again`
   and `CI skips`.
6. **Only the rules about this machine hold here** (C-22). A job's variants (ADR 0044, 0049), a
   later step that reads a check's reports (ADR 0047), which CI job a pass belongs to (C-10) and
   what CI runs outside hushrun (ADR 0040) are about CI's runs, and are left out. A key that can't
   be told, a scope that can't be read, a job that needs what this machine lacks, a tool that isn't
   installed, a `counts_from` rule this machine doesn't meet (C-25), and marks of runs that didn't
   count or weren't recorded (C-22) stay as they are.
7. **`hushrun init --local`** (C-15, C-24). It is init's plan with no org in it: the same wraps of
   package.json's scripts and CI's files, the same agents' files, hook, package and hushrun.yaml
   for scopes or a rule, and no sign-in, no `backend` in package.json or hushrun.yaml, no
   `id-token: write` for CI's jobs, no GitLab token to make, no machine key, no request to any
   server or to `gh`. `--dry-run` shows that plan, as text or as `hushrun.init-plan/v1`, whose first
   note says so. Without a terminal it needs `--yes`. It doesn't go with `--org` or the pipeline's
   flags, and in a repository whose files name a backend it exits 2: that repository shares
   already, and a run there keeps results here until someone signs in (item 2).
   The agents' files are one text either way: AGENTS.md's block says that a pass skips "on this
   machine, and in CI once the repository shares its results", so it holds after `--local`, after
   the plain init that follows, and for a member who isn't signed in. Which of the two is true here
   is `hushrun ready`'s to say, not a second copy of it in a file.
8. **One question, before any sign-in** (C-15). At a terminal, without `--local`, `--yes`,
   `--dry-run` or `--org`, with no login saved for the origin, no agent set, and a repository that
   names no backend, init asks `Share results with CI and teammates (signs in), or keep them on
   this machine only? [S/l]`. `l` is `--local`; Enter, or anything else, signs in, as init did
   before it asked. `Sharing` is that choice, decided once in `run`, and the plan, the sign-in and
   the last line read it.
9. **A plain init later upgrades** (C-15). A repository set up with `--local` names no backend, so
   plain `hushrun init` takes it as any repository: it signs in, adds the backend and the ID tokens,
   connects the repository and gets its ledger. The wraps are there already (`Wrapped`), the
   install script names `hushrun hook`, the hook and AGENTS.md's block are hushrun's, so nothing is
   added twice.
10. **Stats where results stay here** (L-14). Plain `hushrun repository stats` counts this
    machine's results, as `--local` does, when results stay on this machine and no repository URL
    names a server's events; with one, it counts the events, as before.
11. **whoami says why** (C-17). For results kept here because nobody is signed in, the text says
    which backend the repository names and to sign in, and `hushrun.whoami/v1` has `signed_out`,
    the URL, with `named_by` the file that names it.

## Consequences

- A person or an agent with no account gets reuse on their machine, a hook that holds an agent's
  push to what passed here, and a `ready` that says so, from `npx hushrun init --local`.
- A teammate who hasn't signed in skips what they passed themselves, and asks the server nothing,
  where before every run was a refused request and a rerun.
- A CI job without a server never skips, says why once a run, and records nothing anywhere. A
  pipeline that cached `<cache dir>/results` between jobs loses that reuse: it was unsigned, and
  is what this ADR closes. hushrun's own CI names its backend in hushrun.yaml and gives its jobs
  ID tokens, so it is a server, as before.
- hushrun's tests of CI's own rules (variants, reports, `ranAnyway`, summaries, CI files' lines,
  the merge queue) share a fake results server instead of the directory, since CI keeps none.
- `hushrun.ready/v2` and `hushrun.whoami/v1` gain an optional field each, `results` and
  `signed_out`; readers that ignore fields they don't know read them as before.
- A pass older than 30 days no longer skips on this machine, at the latest a day after it turns
  30. The directory holds a `swept` file.
- At a terminal, a first init asks one more question. Scripts, agents and `--yes` see nothing new.

## Alternatives considered

- **A SQLite database or a local server for this machine's results.** The directory already
  creates each object atomically, needs no daemon, and is what `run` uses; a second store would
  be a second answer to the same question.
- **A variable that keeps the directory in CI.** It would bring back what item 3 closes, for a
  forgeable cache, and the honest way for CI to share is a server, which needs no secret on
  GitHub Actions.
- **Falling back whenever the server refuses.** A refused token is someone's credential that no
  longer works, and moving its results to a laptop would hide that; the refusal says what to do.
- **Falling back beside a withheld `HUSHRUN_TOKEN`.** A token is set, and C-13's line says it isn't
  sent: that is the person's to fix, and a quiet switch to this machine would make two problems
  look like none.
- **Expiring at lookup instead of sweeping.** A lookup has no clock of its own, and the sweep
  reclaims the space anyway; a pass between 30 days and 30 days and a day old may still skip,
  which costs nothing a skip on 29 days wouldn't.
- **Sweeping before the command.** It would delay the command's start on the day it runs; after the
  command, the same work delays only hushrun's exit.
- **Asking the question in a repository that names a backend.** The team chose to share; a person
  there who wants no account gets local results without asking (item 2), and `--local` there has
  nothing to set up.

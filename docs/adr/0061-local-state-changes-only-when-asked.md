# 0061. What the CLI keeps on a machine changes only when someone asks

- **Status:** accepted
- **Date:** 2026-11-07
- **Rules:** C-11, C-13, C-15, C-17, C-20, C-21, C-23, C-24, L-11, T-12; §11.2 (detection)
- **Amends:** [0043](0043-a-clone-gets-the-hook-from-hushrun.md) item 5, under which `hushrun init`
  took back a `hushrun hook uninstall`

## Context

The audit of implicit state machines (2026-11-06, `reports/2026-11-06-implicit-state-machines.md`
§5) found state the CLI keeps on a person's machine that one command sets and another resets
without being asked.

**The pre-push hook's mode.** Who the hook refuses, `--refuse`, `--warn` or the default, is kept
nowhere but in the flag its lines pass. Every `hushrun hook install` wrote the lines for the mode
it was given, and a package.json's `prepare`, `hushrun hook install || exit 0`, gives none. So the
next `npm install` after `hushrun hook install --warn` made the hook refuse agents again, and
`--refuse` stopped refusing people. `hushrun init` did the same, with the default, and also took
back a `hushrun hook uninstall`, which ADR 0043 had made hold against `prepare` but not against
init. init is often run again in a repository set up long ago, to move it to another org or to
pick up what agents get, by someone who isn't thinking of the hook at all.

**A saved login that stopped working.** init doesn't sign in when a login is saved for the origin
and the repository names its org. When the backend no longer took that login (revoked from the
dashboard, or signed out on another machine), `GET {base}/v0` answered 401, which init didn't
tell apart from a server error: it went on as a member, warned once for that and once for each
request after it, wrote what it writes, and exited 0 having done nothing on the server.

**The device sign-in's poll.** `hushrun login` and init poll for the token every few seconds while
someone approves the code in a browser. One poll that met a host restarting (a 503 from a deploy or
a cold start, or a connection refused) ended the sign-in, though the approval may have been in
already, and the person had to start again. `slow_down` made the next poll wait longer and skipped
the deadline, so a server that kept saying it would keep hushrun polling past the code's life. And
`hushrun login <org URL>` saved the org as the backend before it asked the org whether the person
was a member: a mistyped or foreign org replaced a backend that worked, and every later run said
its results weren't shared.

**Two answers to "is this CI".** Detection (§11.2) knew GitHub Actions, GitLab CI, Buildkite,
CircleCI and `CI=true`; the newer-release notice kept its own list of 16 variables. Jenkins, Azure
Pipelines, TeamCity and AWS CodeBuild set no `CI`, so a job there was `local` to every command but
the notice: it recorded `provider: local`, `hushrun ready` and `hushrun run` installed a pre-push
hook in the job's workspace, and a login saved on the agent spoke for it. And `from_a_fork`,
`asks_as` and `workflow_job` read `GITHUB_ACTIONS` themselves, so a sandbox inside a GitHub
Actions job that says `HUSHRUN_PROVIDER=local` still asked the server as the repository's CI.

**Two files that name the backend.** package.json's `hushrun.backend` wins over hushrun.yaml's
`backend` (C-11), silently. `hushrun init --org` writes hushrun.yaml first and package.json last,
so a move it didn't finish leaves the two naming two orgs, and so does a hand edit of one: results
went to one org while the other file, which people and the docs read too, said another.

**The monitor's anchor.** `hushrun repository monitor --state FILE` remembers the newest
checkpoint it verified, so that the next run can tell a log that was rewritten. A remembered
checkpoint the log key didn't verify was a warning, and the run then replaced it with the newest
one: a changed log key, or a changed file, reset the anchor without anyone deciding to. The file
was also written in place, so a run cut short could leave half of it.

**A Windows update that can't go back.** On Windows `hushrun update` renames the running
`hushrun.exe` aside, then renames the new one in. When the second rename failed, it tried to move
the old one back, ignored whether that worked, and said `hushrun is unchanged`, though there might
now be no `hushrun.exe` at all.

**A notice that sent npm's installs to a refusal.** The newer-release notice told every install to
run `hushrun update`, which leaves a hushrun that came with npm alone and exits 2, saying to update
the package: two steps where the notice could have said the one.

**The Sigstore cache, a file at a time.** `hushrun sigstore refresh` wrote the TUF roots,
`targets.json` and `trusted_root.json` one after another, each through a temporary file of a fixed
name. They verify together, so a refresh cut short between two of them left a cache that verified
no more, and two refreshes at once could write each other's temporary file. Readers fail closed,
with a warning and the snapshot in the binary, or an error for another TUF repository, until the
next refresh.

**Two sign-ins at once.** `hushrun login`, init's sign-in and `hushrun logout` each read the
credentials file, changed one part and wrote it all back. Two at once lost the first one's change,
or failed on each other's temporary file; and `logout` read the file before it asked the server to
revoke the token, and wrote it after, erasing whatever a sign-in saved meanwhile.

## Decision

1. **Only someone running `hushrun hook install` changes the hook's mode or takes back an
   uninstall** (C-23). It writes the mode its flags say, the default with none.
2. **Everything else that puts the hook in keeps what is there**: a package manager's `prepare` or
   `postinstall` and `hushrun init`. A hook of hushrun's keeps the mode its lines pass; lines with
   no flag, an older hushrun's included, are the default. A new hook gets the mode `prepare`'s
   flags say, or init's default. A clone with `hushrun.hook` set gets no hook, and init's plan
   doesn't list one (C-24).
3. **The mode stays in the lines.** It isn't copied into git's configuration or a file of its own:
   the lines are what git runs, so a second place could only disagree with them. `Mode::of` reads
   it back, and `Asked` says who is installing, in one place.
4. **A login the backend answers 401 is no login** (C-15 step 3). init says
   `Your login for <origin> no longer works, so init signs in again.` and signs in, once, as it
   does when no login is saved. A 403 or 404 is still someone outside the org, and anything else
   still a warning, since connecting and the ledger never fail init.
5. **The poll's answers are one enum** (`Polled`, C-13): pending, slow down, busy, machine revoked,
   ended. Busy (429, 5xx, or no answer) is asked again like pending, said once on stderr; the
   deadline applies after every answer, so nothing polls past `expires_in`. A code that expires
   while the host was failing exits 6, like a host that can't be reached; one nobody approved,
   1.
6. **An org that refuses the person doesn't become the backend** (C-13): with 403 or 404 from
   `GET {base}/v0`, the backend stays the one saved before, and the warning says where results
   go now. Any other refusal, or an error, is a warning as before, and the backend is the org's.
7. **One detector.** `hushrun_ci::detect` also knows Jenkins (`JENKINS_URL` with `BUILD_ID`),
   Azure Pipelines (`TF_BUILD`), TeamCity (`TEAMCITY_VERSION`), AWS CodeBuild
   (`CODEBUILD_BUILD_ID`), Travis CI, Bitbucket Pipelines, Drone, AppVeyor and Semaphore, as the
   generic provider; an agent set there doesn't make the job a sandbox, as for a named CI (C-17).
   `hushrun_ci::in_ci` is its yes or no, and everything that asks goes by it: the run's context,
   the hook, init, tokens and the notice. The GitHub helpers ask it too, so `HUSHRUN_PROVIDER`
   overrides them as it does the rest. The notice still keeps quiet where `CI` says yes, since a
   sandbox detection calls local has no person to read it either (C-21).
8. **Two backends are said** (C-11). Which wins doesn't change: package.json's. hushrun says
   once a command, as a warning that `-q` silences, that the two differ, where results go, and
   that `hushrun init --org <org>` finishes the move. It isn't an error: a run never stops for
   what can only cost a skip (C-14), and init needs the state to finish the move from.
9. **The monitor moves its anchor only past what it checked** (L-11). A remembered checkpoint the
   log key can't verify stays in `--state`, and its warning says to remove the file to start
   again. The file is written through a temporary file and a rename.
10. **A replacement says what it left** (C-20): `NotReplaced` is `Unchanged` or `MovedAside`, and
    only `Unchanged` says hushrun is unchanged. `MovedAside` names `hushrun.exe.old` and says to
    rename it back.
11. **The notice says what update would** (C-21): to a hushrun in a `node_modules` directory, to
    update the package with its package manager.
12. **The Sigstore cache is written as one set** (T-12): into a new `set-…` directory, then a
    rename puts `current` on it; readers follow `current`, and a cache without one, from before,
    holds the files at the top. The set before the one replaced is kept for a reader that may
    still be in it, older ones go.
13. **The credentials file changes under a lock** (C-13): `Credentials::update` takes an
    exclusive lock on `<file>.lock` (std's `File::lock`), reads, changes and writes, and every
    writer goes through it. `logout` revokes first and removes the login under the lock, only
    if it is still the token it revoked. A file system without locks changes it as before.

## Rejected alternatives

- **Have `prepare` pass the mode.** The script is in package.json, which the team shares, and the
  mode is one person's choice for one clone.
- **Keep the previous backend when a sign-in names no org.** C-13 says such a login forgets it,
  and that stays: the new token may be someone else's on the same origin, for whom the old org
  is a refusal on every run.
- **`JENKINS_URL` alone, or `BUILD_NUMBER`, as the notice had them.** People export
  `JENKINS_URL` for Jenkins' own CLI, and `BUILD_NUMBER` is set by build scripts of every kind: a
  laptop taken for CI loses its hook and its login. A job sets `BUILD_ID` beside it.
- **Make bare `hushrun hook install` keep the mode too.** Then nothing would bring a hook back to
  the default, which has no flag of its own.

## Consequences

- A Jenkins, Azure Pipelines, TeamCity or CodeBuild job is CI now, `provider: generic`: it
  records with an agent token (C-17), as CI outside GitHub does, and gets no hook.
- An older hushrun on the same machine reads the Sigstore cache's files at the top, which a
  refresh by this one no longer changes: they verify or not as before, and fall back to the
  snapshot in the binary when they don't.
- The credentials file has a `.lock` file beside it, empty, which stays.
- `npm install` and `hushrun init` print nothing new, and change nothing in a clone whose hook is
  already hushrun's.
- A team that wants `--refuse` in every clone can write `hushrun hook install --refuse || exit 0`
  as its `prepare`: a new clone gets it, and a person who chose `--warn` keeps `--warn`.
- In a linked worktree `.git` is a file, so `ready`'s first stat never finds the hook (ADR 0043's
  consequences): that is a cost, not a wrong answer, and stays as it is.

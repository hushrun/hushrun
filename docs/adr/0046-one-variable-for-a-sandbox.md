# 0046. One variable for a sandbox

- **Status:** accepted
- **Date:** 2026-11-02
- **Rules:** C-11, C-13, C-14, C-17, C-18, C-23, L-10, T-12, P-3, E-12, §10.1, §11.2
- **Amends:** [0028](0028-machines-registered-at-sign-in.md), which gave a sandbox the token and the
  key as two variables

## Context

A sandbox needed three variables: `HUSHRUN_TOKEN`, `HUSHRUN_MACHINE_KEY`, a whole PEM block over
several lines, and `HUSHRUN_PROVIDER=local`. The PEM block is what goes wrong: secrets forms and
masks don't take it, and a sandbox that drops `HUSHRUN_PROVIDER` where it sets `CI=true` records as
CI. An agent in a sandbox should need one variable, one command and one line of output.

It can't be a longer token. 0.5.0 sends whatever `HUSHRUN_TOKEN` holds as a Bearer header on every
request, so a token with a key inside would send the private key to the server.

hushrun can't set variables either: the edition is 2024, where `set_var` is unsafe, every crate
forbids unsafe code, and it would hand the decoded key to the command `hushrun run` starts.

## Decision

1. **One string in a new variable.** `HUSHRUN_AGENT` holds `hushrun_agent_v1_`, then the unpadded
   base64url of the JSON object `{"backend", "key", "token"}`: the org's URL, the agent's private key
   as PEM, and the agent token. Only `[A-Za-z0-9_-]`, with no newline, so a shell and a secrets form
   take it as it is, and GitLab can mask it. The prefix says what it is, starts with no `hra_`, so
   no hushrun takes it for an agent token, and carries the version, so an older hushrun can say to
   update. One matcher decides what is a string, anywhere in a value, since one goes astray in the
   quotes `docker --env-file` keeps, after `Bearer `, behind its own name or without its prefix as
   easily as alone: `hushrun_agent_v<digits>_`, or `eyJiYWNrZW5kIjoi`, the base64url of
   `{"backend":"` that a string starts with once its prefix is gone, in either case with at least 48
   characters of base64url in all after the prefix. That's about a tenth of a real string, and the
   key's bytes start past it, so a cut string that matters still matches, and a file called
   `hushrun_agent_notes.txt`, a test or a `grep` for the prefix doesn't. It stops mistakes: someone
   who sets out to get past it with uppercase, more base64 or percent-encoding will. Around a string
   in `HUSHRUN_AGENT`, quotes and spaces are forgiven; anything else is an error.
2. **It is read in one place.** `Env`, hushrun's snapshot of the environment, reads the string and
   answers `HUSHRUN_TOKEN`, `HUSHRUN_MACHINE_KEY` and `HUSHRUN_BACKEND` from it. Every reader of
   hushrun's variables goes through `Env` or `hushrun_ci::var`, which answers any other variable
   exactly as the process does. So every reader sees it, and a run without it reads what it always
   read.
3. **It overrides bare `CI=true`, and nothing else.** Set at all and with no `HUSHRUN_PROVIDER`, the
   run is `local` where only `CI=true` says otherwise, since that is what a sandbox sets. Everywhere
   else `detect` is as it was: `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE` and `CIRCLECI` beat it, and
   so does an explicit `HUSHRUN_PROVIDER`. A string reaches named CI easily, in a job-level secret
   for an agent's step or a GitLab project variable, and a job that took itself for a sandbox would
   lose ADR 0044's guards and skip wrongly. A sandbox inside such a CI, such as an agent's step in a
   GitHub Actions job, sets `HUSHRUN_PROVIDER=local` and gives up those guards knowingly. Without
   the string nothing changes: real CI outside GitHub sets bare `CI=true` with `HUSHRUN_TOKEN` and
   `HUSHRUN_MACHINE_KEY`, which is what a sandbox set up by hand looks like.
4. **The backend in it is the environment's,** so a repository with no `backend:` still shares
   results, and the token goes to it (C-11, C-13). A string ties the token to its backend: its token
   goes to that origin and to no other, hushrun's host included, on results and ledger requests alike.
5. **It wins, whole.** A token from one place and a key from another don't go together, and a
   leftover variable shouldn't beat the one a person just set. With a string hushrun can read,
   `HUSHRUN_TOKEN`, `HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE` and a different
   `HUSHRUN_BACKEND` are ignored, and one line says so, once: `-q` silences it, and a `hushrun run`
   inside another doesn't repeat it. Only an empty `HUSHRUN_BACKEND` stays, which turns results off.
6. **Nothing leaves the process.** The key isn't in any header, body, argument, log or message.
   Every error about the string is a fixed sentence with no value in it, since `serde_json`'s errors
   and a URL's quote what they read; `Debug` on the string and on `Env` shows no value; a URL's error
   shows no value that holds a string, and a URL that holds one is refused, since URLs get printed
   and sent, so one in `package.json` stops there. A `hushrun.yaml` that holds one anywhere is refused
   too, before anything can quote it, since its rules' values get printed. Only `token create` prints
   the string or the key.
   A value of any other `HUSHRUN_*` variable that holds a string is dropped, with a line that names
   the variable, and never reaches what would print or send it. A command line that holds one stops
   before clap reads it, which would quote it, and before a run, which would record it: exit 2, and
   nothing ran. The ledger client refuses to send one in a token from anywhere, a token command or a
   login file included, its last check before a header. A `counts_from` rule (ADR 0045) reads variables
   through the same seam, so a string dropped from one isn't there, and its `env` can't name
   `HUSHRUN_AGENT`, or `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY`, which hushrun answers from the string:
   a rule compares, and those it would compare to a secret.
7. **A string hushrun can't read costs a line, never a check** (§10.1): it says what's wrong and the
   command runs, with old agent variables beside it carrying on.
8. **Set at all, it is an agent.** That holds for a string hushrun can't read and for one in another
   variable, since whoever set it meant an agent. `hushrun approve` refuses, the pre-push hook takes
   the pusher for an agent, before any `HUSHRUN_INVOKED_BY=human:…`, which is only a word, and signing
   in refuses, since a sign-in would register the agent's key as a person's machine, which could then
   approve. `hushrun token` refuses, or an agent beside a login could mint agents. Whatever the file
   holds, every login reads as signed out, through the one method all readers use: `init`, as a
   signed-out init does, wants a sign-in, which refuses, so it says so and sends nothing, and a plan
   shows what signing out would. Beside a string that can't be read, a personal token in
   `HUSHRUN_TOKEN` and the key a sign-in left beside the credentials stay out too, so a run there
   records nothing. So a typo gives no one a person's powers.
9. **The command `hushrun run` starts keeps `HUSHRUN_AGENT`,** by inheriting it, as it inherited
   `HUSHRUN_TOKEN`: a check's own `hushrun run` needs it to record, and `HUSHRUN_INSIDE_RUN` exists
   because checks do nest. It gets nothing hushrun decoded: no `HUSHRUN_TOKEN` or
   `HUSHRUN_MACHINE_KEY` appears in its environment. An isolated run never passes it in: it is a
   secret by name (E-12), which no pattern would have caught.
10. **`token create` keeps stdout as it was: the token alone.** Scripts hold that, and a change would
    be silent and dangerous: a script that stored a string as `HUSHRUN_TOKEN` would send a private key
    through any 0.5.0 it met. The string goes where a person reads it: when stdout and stderr are
    terminals, outside CI, the first line of stderr is `HUSHRUN_AGENT=<string>`. The token and the key
    follow the same rule, since a piped stderr is often a log, and a job's always is: the key shows
    only on a terminal's stderr outside CI, and the token only with the string. Anywhere else stderr
    says what to do, and `--format json` is how scripts get all three. The two-variable form stays,
    because CI isn't a sandbox: GitLab pipelines and other CI keep storing `HUSHRUN_TOKEN` and
    `HUSHRUN_MACHINE_KEY` or `HUSHRUN_MACHINE_KEY_FILE`, and sandboxes already set up that way keep
    working.
    `HUSHRUN_AGENT=$(hushrun token create …)` gets the token, which hushrun says, with what to do.
    A string never goes on a command line, so nothing takes one there, `token revoke` included.

## Consequences

- A sandbox needs one variable, and what existing scripts read from stdout doesn't change. A script
  that read the key from stderr must move to `--format json`, as the changelog says: a token made
  that way, with its key nowhere, is no use, and stderr says to revoke it.
- An old hushrun ignores `HUSHRUN_AGENT`: it shares nothing, and treats a sandbox with `CI=true` as CI.
  `hushrun init` pins `^<its version>`, and a caret on 0.x stays in that minor, so `npm ci` in a
  repository set up on 0.5.0 never reaches 0.6.0, and init leaves a declared dependency alone. The
  docs name 0.6.0 as the minimum and how to move: `npm install --save-dev hushrun@latest`, which is
  what `hushrun update` says too.
- The string can hold more than a CI job should use. A named CI stays CI beside it, with ADR 0044's
  guards, recording as that CI with the agent's token. A CI that sets bare `CI=true` and nothing else
  can't be told from a sandbox: with the string it runs as local, which is why CI keeps the two
  variables, and the docs say so.
- A check can read `HUSHRUN_AGENT`, as it could read the two variables. The repository's code runs in
  the sandbox, so this isn't a boundary, and an agent token can do little: its org's results, signed
  as that agent, until it is revoked.
- The form is a contract between the CLI, the dashboard and anyone who builds one by hand. A change
  to it is a new version, `hushrun_agent_v2_`, which older hushruns refuse by name.

## Alternatives considered

- **A longer `HUSHRUN_TOKEN`.** It sends the key to every server 0.5.0 talks to.
- **Precedence to the old variables.** It keeps a stale secret working over the one a person just
  set, and a mix of the two signs with a key the token doesn't own, which the server refuses with
  `machine_unknown`.
- **`HUSHRUN_BACKEND` beats the string's backend.** C-11's order would stay as it reads, but the token
  in the string would go to a URL it didn't come with. Binding them is what makes one string safe to
  hand out. An empty one stays, since it asks for nothing to be sent.
- **Printing the string on stdout,** as a first version did. It makes `$(…)` hold the one value a
  sandbox wants, and changes what every existing script holds, silently, with a private key in it.
- **`HUSHRUN_AGENT` means local everywhere.** A first version did, as `HUSHRUN_PROVIDER=local` does.
  In review, a GitHub matrix job with the string set skipped its failing variant, with exit 0. A
  provider override is a person's decision; a secret that reached a job isn't.
- **`token revoke` taking the string.** Convenient, and it invites the key into `argv`.
- **Matching the prefix alone.** A first version did, and `grep -r hushrun_agent_ src` exited 2 claiming
  a private key, as did a test named after the feature, and a remote URL naming it let a push through
  unchecked. A check that cries wolf gets turned off, and these are the words people use for it.
- **Matching uppercase, nested base64 or percent-encoding.** Each is someone going out of their way,
  and the check's job is mistakes: a sandbox's agent can read the variable and do as it likes anyway.
- **Taking `HUSHRUN_AGENT` out of the command's environment.** Nested runs would lose their
  credential, and the sandbox has it anyway.
- **Decoding into the process with `set_var`.** Unsafe, forbidden here, and the decoded key would
  reach every child.
- **A binary encoding.** A third shorter, but the dashboard has to write it too, and JSON holds a
  PEM block of either key type as it is.
- **Refusing a run on a bad string.** A variable never stops a command (§10.1), and a check that ran
  without sharing costs a run, not a wrong skip.

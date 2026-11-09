# 0032. Refusals for the org's plan fail open, and teams can read their volume

- **Status:** accepted
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-26
- **Rules:** L-3, L-5, L-10, L-14 (new), C-12, C-14, C-19 (new); ADR 0011 item 6 (which writes
  count), ADR 0027 item 5 (the limit per token, which stays as it is)

## Context

The hosted service's plans include a number of **recorded checks** per period, some with a
soft overage up to a spend cap. A recorded check is a new `check` event that a hosted repository stores (from
`run --stage` or `attest --check`, pass or fail), or a new `pass` or `fail` result that `hushrun
run NAME -- COMMAND` records at `/o/{org}/v0/results/{key}`, counted once per period by a dedupe
key. Everything else is free: every other event, policies, skips, and every read, so reuse costs
nothing.

When an org has used its allowance or reached its spend cap, the server
answers a new `check` event or a `pass`/`fail` result with **402** and the code
`quota_exceeded`, a message a person can act on, the plan, the period's numbers and an upgrade
URL. Reads, skips and every other event are still accepted, and a key
that exists still answers 412, so a retried create isn't refused.

The CLI had no answer for it. L-10 mapped any other 4xx, 402 included, to `Invalid`, a usage
error: `run --stage` would have exited 2 at its first check's write, running no other check and
printing nothing, and `attest` too, before its other writes, such as an artifact's: pipelines
whose checks passed would fail. `hushrun run` would have said `couldn't be recorded (…: 402:
…)`. And a team had no way to see where it stood, or, on its own server, how much it records.

## Decision

1. **`Quota`, a ledger error** (L-3, L-10). A 402 whose `error.code` is `quota_exceeded` is
   `LedgerError::Quota`, carrying the server's message and, when the answer has them, the
   upgrade URL, the plan, `used`, `limit` and `period_end`. Only the message is required: the
   server's fields are read at the top level, or inside `error`, and a missing message has a
   default. The message is kept on one line of at most 300 characters, control characters
   turned to spaces, and the upgrade URL only when it is an `http://` or `https://` URL without
   spaces or control characters, since both reach a terminal. A 402 is never retried, as no
   4xx but 429 is (L-4). Any other 402 stays `Invalid`.

2. **Pipelines fail open** (L-5). In `run --stage` and `attest`, a `check` event that gets
   `Quota` isn't recorded, and the command goes on:
   - it prints one warning per command, `warning: check results aren't recorded: <the server's
     message>; upgrade: <upgrade_url>`, at the first refusal, which stays true for later ones in
     the same command; on GitHub Actions the warning is an annotation instead (§10.2), which the
     run's log and summary show;
   - its other checks still run, and its other events are still written: an artifact, a
     `started`, and the check events the server takes, such as a retried run that its dedupe
     key admits;
   - it exits as if the event had been recorded: `run --stage` 0 when its checks passed and 8
     when one failed, `attest` 0;
   - its output marks what wasn't recorded: `run --stage` ends the check's `detail` with `;
     not recorded` and gives it `event`, `custody` and `environment` null in `hushrun.run/v1`,
     whose schema has no other field for it (`additionalProperties: false`); `attest` prints
     `not recorded check: <name>` in text and in the step summary, and leaves the check out of
     `hushrun.attest/v1`'s `events`, which list what was written.

   Only `Quota` behaves this way, and only for a `check` event. Every other failure keeps its
   behaviour (exit 6, L-5), and so would a `Quota` for any other write: the server refuses none
   for the plan, so one that did is a failure to report, not a plan to work around.

3. **`hushrun run` keeps COMMAND's exit code** (C-14). A 402 `quota_exceeded` to `POST
   {base}/v0/results/{key}` is a refusal of its own kind, and the run's last line is `hushrun:
   <NAME> passed in <time>; not recorded: <the server's message>; upgrade: <upgrade_url>`, or
   `failed in <time> (exit <code>)`, in the words C-14 uses for the other refusals of a record.
   `; upgrade: …` is left out when the answer has no upgrade URL, or the message already names
   it, so the URL isn't printed twice. The next run on the same code finds no pass, and runs.

4. **`hushrun repository usage`** (C-19, C-12) resolves the backend exactly as `hushrun run`
   does (C-11), asks `GET {base}/v0/usage` with the same credential (C-13), and shows the org,
   the plan, the period, the checks recorded against those included and the limit, the overage,
   this repository's share, and the dashboard. This repository is the one `hushrun run`'s
   results name (C-10), matched ignoring case, as hosts and GitHub's names are. Results on this
   machine are metered nowhere, and it says so; a server that answers 404 or 405 doesn't report
   usage. It exits 0 in those cases, and 6 with the server's message when the server can't be
   reached, refuses the credential or answers anything else: §10.3's code for a server that is
   unavailable or denies access. (`whoami` exits 1 on a refusal, since who the credential is is
   what it asks.) The command is `hushrun repository usage`, not the plan's `hushrun ledger
   usage`: the admin command is `repository` (ADR 0029), and people never read "ledger".

5. **`hushrun repository stats [--since 30d] [--local]`** (L-14) counts volume from **one
   listing** of `v1/events/`, reading no object: a key carries its scope, digest, kind, stage
   and, in its ULID, its time (L-2). It counts the events, by kind, the distinct digests and the
   `check` events by stage, from `--since` ago on. With `--local`, it counts this machine's
   `hushrun run` results in the cache directory instead, by verdict, by check, the distinct run
   keys and the time skips saved, and sends nothing. Its help says what it is for: sharing
   volume without telemetry. A team on its own server can paste it; hushrun sends nothing new
   anywhere.

No dependency is added: the 402's body is read with `serde_json`, which `hushrun-ledger` already
has, and the outputs use the CLI's.

## Consequences

- A team over its plan never has a pipeline fail or a deploy blocked for it: checks run as they
  did, artifacts and deploy markers are recorded, and evidence already recorded is still reused.
  What it loses is reuse of the checks that weren't recorded, which run again next time; the
  warning, the `not recorded` marks and the upgrade URL say so where it shows.
- `LedgerError` has a sixth variant. Code that matches it exhaustively must handle `Quota`;
  The hosted service builds against these crates but doesn't match on it.
- In `hushrun.run/v1`, a check that wasn't recorded has `event: null` and a `detail` ending in
  `; not recorded`. A field of its own, such as `recorded`, would need `hushrun.run/v2`, since
  the schema allows no other property.
- Two schemas are new contracts: `hushrun.repository.usage/v1`, whose field names follow the
  server's usage answer and `whoami`'s (snake case), and `hushrun.repository.stats/v1`, which
  follows `repository gc`'s (camel case).
- The warning is said at the first refusal, so it names no check: the output's marks do. A
  command that fails later for another reason has said it already.
- A server other than hushrun cloud may refuse what it likes with `quota_exceeded`; the CLI fails
  open only for `check` events and results, the writes the plan meters.
- The testkit's fake ledger can answer a status with a given JSON body on a given method and
  path (`Fault::Answer`), and a fake results server (`fake_results::FakeResults`) records, looks
  up, reports usage and answers what a test injects; `quota_exceeded()` builds the 402's body.

## Alternatives considered

- **Fail closed: exit 6, as for any refused write.** A free org that used its 2,000 checks would
  have its pipelines fail mid-way, its deploys blocked by a billing state. The plan's promise is
  that nothing is blocked; refusing a write that only enables reuse costs a re-run, never a
  wrong deploy.
- **Keep 402 as `Invalid`, and let recipes ignore exit 2.** Exit 2 means a usage error; recipes
  would have to ignore real ones too, and `attest` would still skip its later writes.
- **A new exit code for "not recorded".** Pipelines would need changes to treat it as success,
  and the plan asks for the codes they already handle: the run did what it was asked, and its
  result is the checks'.
- **Retry, or queue the refused events locally for later.** The refusal lasts until the period
  ends or the org upgrades, longer than any retry; a queue would record evidence with a time
  that isn't when it was written, and send what the team didn't choose to send.
- **A warning per refused event.** A run of twenty checks would print twenty times the same
  sentence; one says it, and the output marks each check.
- **Name the checks in the warning, printed at the end.** Clearer for a command that finishes,
  but a command that fails later for another reason would never say it; the first refusal says
  it at once, and the per-check marks name them.
- **Stats by reading every event.** A request per hundred events, for what the keys already
  say.
- **Telemetry for volume.** hushrun sends nothing it didn't need before; a team decides what to
  share by pasting `stats`.

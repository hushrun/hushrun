# 6. Spec: ledger

> Normative. Copied from the implementation plan (2026-10-13) in M0; this file is now the source of truth.
> MUST, SHOULD and MAY follow RFC 2119. Rules tagged `[Mn]` belong to milestone n.

**L-1 URLs.** [M18] `https://host/path`, a ledger served over the protocol of L-10, such as the hosted one; `http://` only on a loopback host (`localhost`, `127.0.0.1`, `[::1]`), for local development. [ADR 0022] Every other scheme fails to parse with one message, "only https:// ledgers are supported (the hosted ledger)": the bucket ledgers of M4 (`gs://`, `s3://`, `az://`), directories (`file://`), `mem://` and `oci://` are gone.

One `https://` URL holds exactly one repository. Its signers answer (L-13) names the repository, and hushrun refuses a ledger whose answer names another one than `hushrun.yaml`'s (C-2).

**L-2 Layout**, relative to the URL:

```
v1/events/<scope>/<digest hex>/<ULID>.<kind>.<stage or _>.json
v1/deployed/<scope>/<stage or _>/<inverted ms>.<ULID>.json
v1/policies/<sha256 hex>.yaml                                     [M18] hushrun.yaml files that plans evaluated
```

- `<inverted ms>` is `9999999999999 − unix_ms`, zero-padded to 13 digits, so the newest deployment sorts first.
- Each `deployed` event is written twice, with the same bytes: first under `events/`, then under `deployed/`. [ADR 0056] When the second write fails, the event is in `events/` only, and the planner doesn't read it as what the stage runs (L-8). `mark deployed` run again for the same attempt and the same deployment (artifacts and target) then writes the missing `deployed/` entry with that event's bytes, at the key its time and ULID give, instead of a second event; once both copies exist, a deploy recorded again is a new event.
- `_` replaces the stage when an event has none, in both paths: `v1/events/…/<ULID>.<kind>._.json`, and `v1/deployed/<scope>/_/` for a `deployed` event without one. 0.8's keys, with a stage, stay readable beside them.
- [M18] `plan --start` uploads the exact `hushrun.yaml` bytes it evaluated to `v1/policies/<sha256 hex>.yaml`, create-only, before its first `started` event, whose `policyDigest` (E-4) is that hex. It does so only when it records `started`. A key that exists counts as success, and any other error is a warning: readers that decide ignore these keys, which serve readers that show what each stage required, such as the index.
- Names of scopes, stages, checks and artifacts match `^[a-z][a-z0-9_-]{0,62}$`, validated when the policy loads. Where a name appears in an environment variable or dotenv key, it is uppercased and `-` becomes `_`. Two names that collide after this mapping are a validation error.

**L-3 Backend contract** (trait `Ledger`):

- `append(key, bytes)` is create-only. It uses a conditional create where the store supports one; otherwise a plain put is safe, because keys are unique by construction. If a retried create returns `AlreadyExists`, the backend reads the object back: identical bytes mean the first attempt succeeded, so it returns success.
- `list(prefix)` returns every key under the prefix, sorted ascending. A backend whose store doesn't sort, sorts itself.
- `list_first(prefix, n)` returns the first `n` keys in ascending order.
- `get(key)` and `delete(key)`, which GC uses only.
- [ADR 0026 amendment 2] There are no configuration objects: the root versions, signed trust and inventories they held are gone with signed trust (T-8), and so are `put_config`, `create_config` and `get_config`.
- Errors: `NotFound`, `AlreadyExists`, `Denied`, `Unavailable` (after retries), `Invalid`, and [ADR 0032] `Quota`, a write a server refuses for the org's plan (L-10).

**L-4 Retries.** Idempotent requests are retried up to 3 times, with exponential backoff and jitter, on timeouts, 429 and 5xx. The per-request timeout is 10 s. `plan` has a 60 s overall budget for its requests, not counting the waiting it does by design (upstream grace, `--wait-timeout`).

**L-5 Failure policy.** If the ledger can't be read, `plan` exits 6 (§10.3) without a decision. It never silently falls back to `full`; a stage may opt in with `on_repository_error: full`. [ADR 0056] That fallback keeps P-8, which needs no events: on a branch the stage doesn't deploy from, the plan is `blocked` as when the ledger answers. A failed write makes `attest` or `mark` exit 6, and the recipe decides whether that fails the job (§15).

- [ADR 0032] **Fail open on the org's plan.** A `check` event the server refuses for the org's plan (`Quota`, L-10) isn't recorded, and `run --stage` and `attest` go on: they say so in one warning per command, `warning: check results aren't recorded: <the server's message>; upgrade: <upgrade_url>` (an annotation on GitHub Actions, §10.2), write their other events and checks as before, and exit as if it had been recorded: `run --stage` 0 when its checks passed and 8 when one failed, `attest` 0. `run --stage` marks each such check `not recorded` (its `detail` ends with `; not recorded`, and its `event`, `custody` and `environment` are null in `hushrun.run/v1`); `attest` prints `not recorded check: <name>` for each, and leaves it out of `hushrun.attest/v1`'s `events`. Later runs find no evidence for these checks and run them again: nothing is blocked. Only `Quota`, and only for a `check` event, behaves this way: hushrun's server refuses no other write for the plan, and a command whose other write got `Quota` would exit 6, as for any failed write.

**L-6 Access control.** CI identities, and [ADR 0028] people's and agents' registered machines, may create and read objects under `v1/`, and nothing else. They can't overwrite or delete objects. This matters: deleting a `failed` event could unblock a digest, so the ledger must be append-only for them.

- [M18] **HTTP ledgers:** the server enforces the roles of L-10 itself. CI's ID tokens have the writer role.
- **Admin rights** are needed for GC. [ADR 0031] Who signs for the repository is the server's answer (L-13), from its org's members, agents and the repository's CI (T-15).

**L-7 Consistency.** A ledger's server gives strong read-after-write and list-after-write consistency: a create it answered 201 for is in every later read and listing. No locks are needed, because objects are immutable and keys never collide.

**L-8 Deployed index reads.** The planner reads the first 20 entries of `v1/deployed/<scope>/<stage>/` (`_` for deployments without a stage), verifies them (E-8 step 4), and uses the newest trusted event by `time`; on a tie, the one with the smaller key. Keys are written by the signer's clock, so their order is only a hint. If none of the 20 is trusted, it reads further pages, up to 200 entries. [ADR 0056] Trusted means it verifies and its signer counts for the stage (T-16), or, under `_`, against every period of the signers answer (L-13): at a CI-only stage, 20 entries that verify but don't count are read past.

**L-9 GC.** `hushrun repository gc --older-than <duration>` deletes events older than the threshold, except the newest deployed entry per (scope, segment) and the events of its digest, where the segment is the path's stage or `_`, whichever the ledger lists, so a stage that hushrun.yaml no longer names keeps its newest deployment too: [ADR 0056] the one the planner reads (L-8, P-10), and the newest that verifies whoever signed it, ties broken as L-8 does. It is admin-only; `--dry-run` lists what would go. GC also removes old verdicts, so a digest whose events are all gone is treated as never seen; choose the threshold with that in mind (the docs suggest 180 days). [M18] GC keeps every `v1/policies/` object, whatever its age: they are small, and any remaining event may name one.

[M18] On an HTTP ledger that keeps a transparency log (L-11), a deletion is a tombstone leaf, and an administrator signs each one: `repository gc --key <PEM>` (default `HUSHRUN_SIGNING_KEY` or `HUSHRUN_SIGNING_KEY_FILE`) signs a *GC statement* per key, a DSSE envelope (E-1) of payload type `application/vnd.hushrun.gc+json` around `{"_type": "hushrun.gc/v1", "ledger": "<the ledger URL>", "key": "<key>", "time": "<RFC 3339>"}`, and the delete carries it in `X-Hushrun-Gc` (standard base64). [ADR 0031] The signer is a machine the repository's signers mark `gc`: one of a current admin's or owner's of the org (L-13), from `--key` or the command's signer (T-12). Its signature counts as an event's would (E-8 step 2), and `repository gc` checks each statement against the signers before the delete goes out, refusing a key that isn't such a machine. Without the log key (T-9) it deletes unsigned, with a warning. Monitors report every tombstone without a statement that verifies so as an unsigned deletion.

**L-14 Volume [ADR 0032].** `hushrun repository stats [--since <duration>] [--local]` counts what hushrun recorded, so that a team can share its volume without telemetry: it sends nothing beyond the one read below, and prints what the team may choose to paste. `--since` takes a duration as `gc --older-than` does, by default `30d`, and counts what was recorded from that long before now on.

- **The repository's events.** One listing of `v1/events/` on the repository's server, at the URL the other `repository` commands use (T-4), and no object read: each key carries its event's scope, digest, kind and stage, and its ULID the time it was written (L-2), which `--since` compares. It counts the events, the events by kind, the distinct digests (a scope and its digest), and the `check` events by stage, `_` for one without; a key under `v1/events/` that isn't an event's isn't counted. A listing that fails exits 6 (L-5).
- **`--local`: this machine's results.** [ADR 0067] Also without `--local`, when results stay on this machine (C-11) and no repository URL (T-4) names a server's events. `hushrun run`'s results in `<cache dir>/results` (C-11), by the time of each file's ULID, when the run finished, and nothing is sent: the results, by verdict (`pass` and `fail` under `results/v0/`, `skip` under `skips/v0/`), the distinct run keys (C-9), the results by check, and the time the skips saved, the sum of their `savedMs`. [ADR 0074] When some run said what its test files did (C-10's `units`): how many runs said, the test files they ran and reused, and the time reusing saved, the sum of their `units.savedMs`, as `test files       <ran> ran, <reused> reused, in <n> runs` and `saved by reuse   <time>`, and in JSON `testFiles: {runs, ran, reused, savedMs}`; without any, nothing. A file that isn't a result hushrun reads counts as none (C-10); without a cache directory, there is nothing to count.
- **Output.** Text, one line per count; with `--format json`, `hushrun.repository.stats/v1` (`docs/schemas/repository-stats.v1.schema.json`), whose `source` is `repository` or `local`. It exits 0, 2 for a usage error, and 6 when the listing fails.

**L-10 HTTP protocol [M18].** An `https://` ledger (L-1) speaks `hushrun-ledger-http/1`. Every endpoint is relative to `{base}`, the ledger's normalized URL. Storage carries no trust here either (E-8): nothing in the protocol lets the server make a reader accept a fact none of the repository's signers signed (T-15); who those signers are is the server's signed answer (L-13).

- **Normalization**, as T-4 compares URLs and as ID token audiences use them: clients refuse a URL with userinfo, a query or a fragment; they lowercase only the scheme and host, drop one trailing `/`, and keep a port exactly as written; path segments use unreserved characters only. `https://host:443/l/a/b` and `https://host/l/a/b` are different ledgers.
- **Tokens.** Every request carries `Authorization: Bearer <token>`. The CLI takes it, in order, from:
  1. `HUSHRUN_REPOSITORY_TOKEN`;
  2. what `HUSHRUN_REPOSITORY_TOKEN_COMMAND` prints, reused for 5 minutes; [M19] it runs with `HUSHRUN_REPOSITORY_URL` set to the ledger's normalized URL, so one command can print each ledger's token;
  3. in GitHub Actions, when `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN` are set (the job has `permissions: id-token: write`), an ID token requested with `audience=<base>`, reused until a minute before it expires;
  4. [ADR 0028] `HUSHRUN_TOKEN` when it is an agent token (`hra_…`) and the ledger is on hushrun's host (C-13), in CI as outside it; [ADR 0046] with `HUSHRUN_AGENT`, its token when the ledger is on its backend's origin, and on no other; else, outside CI, the personal token `hushrun login` saved for the ledger's origin;
  5. otherwise nothing, and the server answers 401.

  Like `HUSHRUN_REPOSITORY_URL`, the first three come only from the environment, never from the repository (T-4), and so does where they go: such a token goes only to a ledger named in `HUSHRUN_REPOSITORY_URL` or `--repository-url`, never to one that only `hushrun.yaml` names. Two exceptions keep the rule: [ADR 0031] in CI without `HUSHRUN_REPOSITORY_URL` or `HUSHRUN_REPOSITORY_BASE`, the ledger `hushrun.yaml` names gets the ID token, whose audience names it, but never the others (T-4); and the fourth goes only to its own origin, where nothing else can use it, even to a ledger only `hushrun.yaml` names: the server on hushrun's host takes an agent token for its org only, and a login is its origin's. No other token, such as a `HUSHRUN_TOKEN` that isn't an agent's, ever follows a URL that only a repository file names. So a GitLab pipeline needs no setting beyond its agent token to reach its repository on hushrun's host. [ADR 0046] A client refuses to send a token that holds a `HUSHRUN_AGENT` string, anywhere in it, wherever it came from: it holds a private key. Tokens are never logged, and only the ledger's origin receives one: clients follow only 307 and 308 redirects, which keep the method and the body, and only within the origin they started from.
- **Roles.** `reader`: `GET {base}`, read objects, list, batch-get. `writer`: also create objects under `v1/`. `admin`: also delete objects (GC, L-9). How a server maps tokens to roles is its own; hushrun cloud's (ledger-http-v1 §2) makes [ADR 0028] a member's personal token a reader of the org's ledgers, and an agent token a reader of the ledgers its scope covers, and either a writer of an object under `v1/` only when the object is an event signed by a machine registered for that token's owner (C-18). Readers never depend on it: storage carries no trust (E-8).
- **Keys** are 1 to 1024 bytes of `/`-separated segments matching `[A-Za-z0-9._-]+`, none `.` or `..`, so they go into URLs as they are. Writers create keys under `v1/` only, in one of the shapes of L-2; a policy key's hex must be the SHA-256 of its body. Configuration names follow the same character rules.
- **Endpoints:**

  | Request | Answer |
  |---|---|
  | `GET {base}` | 200 `{"protocol": "hushrun-ledger-http/1", "ledger", "repository", "org", "name"}`; `doctor` checks the protocol and the repository |
  | `PUT {base}/objects/{key}` with `If-None-Match: *` | 201 `{"key", "sha256", "size"}`. 412 `already_exists` when the key exists, with the stored bytes' hash as a top-level `sha256`: equal to the client's means a retried create succeeded (L-3); without it, the client reads the object back. 428 without the header. [M18] Both carry the object's `receipt` (L-11) |
  | `GET {base}/objects/{key}` | 200 with the bytes and `X-Hushrun-Sha256`; 404 |
  | `GET {base}/objects?prefix=&after=&limit=` | 200 `{"keys": […], "next": <key> \| null}`: keys in byte order, after the exclusive `after`, at most `limit` (1 to 1000, default 1000). `list` follows `next`; `list_first(prefix, n)` asks for one page of `limit = n`, and follows `next` only when the page is shorter. A `next` must be a key under the prefix, past `after` and past the page's keys, on a page that isn't empty; clients refuse a listing that breaks this, or that takes more than 20,000 pages. [M18] The page is read at a checkpoint, which it carries as `checkpoint`, with the ledger's signed `statement` about it (L-11) |
  | `POST {base}/batch-get` `{"keys": […]}` | 1 to 100 keys; 200 `{"objects": [{"key", "data": <standard base64>} \| {"key", "error": "not_found"}]}` in request order. `get_many` sends batches of 100, 16 in flight (§12.4); another `error` makes that key unavailable |
  | `DELETE {base}/objects/{key}` | Admin. 204, or 404, which clients treat as success because GC retries |

- **Writes count only with the protocol's answers:** 201 with the stored bytes' `sha256` for an object create, 204 for a delete. Any other 2xx, such as a proxy's page, is `Unavailable`: nothing is known to be written.
- **Errors.** A non-2xx answer carries `{"error": {"code", "message"}}`. 400, 413 and 428 (`invalid`, `too_large`) are `Invalid`; 401 (`unauthenticated`) and 403 (`denied`) are `Denied`; 404 is `NotFound`, except from `GET {base}`, listings and batch-gets, whose endpoints always exist: there it means that no ledger answers at the URL, which is `Unavailable` (L-5); 412 is `AlreadyExists`; 429 and 5xx are retried (L-4), honoring `Retry-After` on 429 and 503 when the wait fits what is left of the budget (60 s at most without one), then `Unavailable`. A redirect to another origin is `Unavailable`, and names only the origin.
- [ADR 0032] **The org's plan.** A server may limit what an org's plan records, as hushrun cloud limits its recorded checks per period: it answers a create it won't record for the plan with 402 and the code `quota_exceeded`, which is `Quota`, never retried:

  ```json
  {"error": {"code": "quota_exceeded", "message": "<one line a person can act on>"},
   "plan": "free", "used": 2000, "limit": 2000, "period_end": "2026-11-01T00:00:00Z",
   "upgrade_url": "https://app.hushrun.com/acme/-/billing"}
  ```

  Only `error.message` is sure to be there; `plan`, `used`, `limit`, `period_end` and `upgrade_url` are top-level (a client also reads one inside `error`), and a client keeps an `upgrade_url` only when it is an `http://` or `https://` URL without spaces or control characters, and the message as one line of at most 300 characters. hushrun cloud refuses only new `check` events so (a key that exists still answers 412), and still takes reads and every other event, so evidence is still reused and deploys go on (L-5). Any other 402 is `Invalid`.
- **Limits and versions.** Bodies are at most 1 MiB, batch-gets 100 keys and pages 1000 keys. Clients read at most 1 MiB and one byte of an object, so an oversized one is rejected by E-8 step 1 instead of failing the read. Additive changes, such as new fields or endpoints, stay in version 1, and clients ignore what they don't know; a breaking change is a new version.

**L-13 Signers [ADR 0031].** `GET {base}/signers`, for any role, answers the signers whose events count for the repository (T-15): 200 `{"signers": <document>, "statement": "<signed note>" | null}`. hushrun asks once per command that verifies events, and `hushrun repository signers [--out FILE]` prints or saves the answer.

- **The document** is `hushrun.signers/v1`: `{"schema": "hushrun.signers/v1", "repository": "<host/owner/name>", "time": "<RFC 3339>", "ci": [{"issuer", "repository_id"}], "machines": [{"keyid", "key", "name", "owner", "registered", "revoked", "gc"}], "ci_only": [{"stage", "periods": [{"from", "until"}]} | {"from", "until"}]}`. `ci` lists the CI identities the server verifies for the repository (GitHub Actions with the repository's id, once the server knows it). `machines` lists every machine of the org's members (`owner: "user:<email>"`) and of the agent tokens whose scope covers the repository (`agent:<name>`), revoked ones included, with `revoked` the time it stopped counting (logout, a removed member, a revoked or expired agent token, a revoked machine), null while it counts. `gc` marks the machines of the org's current admins and owners that count, which sign GC statements (L-9). [ADR 0031 amendments 1 and 2] `ci_only` lists the stages that are or were CI-only for the repository (T-16), one per stage name, sorted, each with every period it was: from `from` until `until`, or still CI-only when `until` is absent (or null). A stage's periods are the union of the org's (the stage listed for every repository) and the repository's (the stage it marks): overlapping or touching periods are one, so they are sorted, apart and non-empty, and only the last may be open. The list is left out when no stage ever was CI-only. The list may also hold periods without a stage, `{"from", "until"}`, in the same order rules, after the stages: the repository was CI-only then whatever the stage, so they apply to every stage. A question without a stage (a `deployed` event under `_`, say) reads the union of every period listed, a stage's or not: never looser than any stage was. A reader applies it as T-16 says, and refuses a document that lists a stage twice, names an invalid stage, gives a stage no period, or periods that end before they begin, overlap, touch, are out of order or are open before the last.
- **The statement** is a C2SP signed note by the log key (T-9, L-11): `hushrun-signers/v1`, `origin <the log's origin>` and `document <hex of SHA-256 of the document's JCS form>`, one per line. A ledger that keeps a log signs every answer. hushrun checks it against `hushrun.yaml`'s `transparency_log.key` when it names one, and refuses an answer whose statement doesn't verify, or names another document, as `Unavailable` (exit 6); without a pinned key, it warns and uses the answer unchecked. A saved answer (`verify --signers`) is checked the same way, offline.
- An answer whose `repository` isn't `hushrun.yaml`'s exits 2.

**L-11 Transparency log [M18].** An HTTP ledger may keep an append-only Merkle log of everything it stores, as the hosted ledger does (RFC 0001 §4.7, H1), so that a reader can catch what it withholds: every deploy decided from an incomplete listing is found after the fact, with the ledger's own signed statement as proof, and a ledger that shows two histories is caught when a reader meets a checkpoint from the other one. `hushrun-tlog` implements every format below; ADR 0017 records the choices.

- **Leaves.** One per object created and per deletion (and, in logs from before ADR 0026 amendment 2, per configuration write), in the order the ledger sequences them, and [M19] one map leaf at the end of every batch (L-12). A leaf is RFC 6962's hash of an *entry*, `SHA-256(0x00 || entry)`, where the entry is the text below, every line ending in a newline: a stored object's key and the SHA-256 of its bytes. A deletion drops the bytes, never the leaf.

  ```text
  hushrun-tlog-entry/v1
  object | config | tombstone
  <the key, or the configuration object's name>
  <64 lowercase hex: SHA-256 of the bytes stored, or deleted>
  gc <64 hex: SHA-256 of the GC statement> | gc none           (tombstones only, L-9)
  ```

  [M19] A map leaf's entry is three lines: `hushrun-tlog-entry/v1`, `map`, and the 64 lowercase hex digits of the prefix map's root (L-12).

- **Tree, proofs and checkpoints.** The tree, inclusion proofs and consistency proofs are RFC 6962's (`PATH`, `PROOF`), verified as RFC 9162 §2.1 describes; hashes travel in standard base64. A checkpoint is a C2SP `tlog-checkpoint` note, signed with the ledger's *log key* (C2SP `signed-note`, Ed25519): its origin is the ledger's URL without the scheme, and the key is named after it. Signed trust names the log key (T-9), so the ledger never vouches for itself.
- **Read-after-write.** The ledger sequences writes in batches of at most about a second, and answers a create or a delete only once its leaf is in a signed checkpoint; listings are read at the latest checkpoint. L-7 holds.
- **Receipts.** A create's 201, and a retried create's 412, carry `receipt: {index, checkpoint, proof}`. When `hushrun.yaml` names the log key (T-9), the client checks it before reporting the create as done: the checkpoint is signed by the log key under its origin, and the proof leads from the object's leaf to its root. Otherwise the write isn't known to be in the log: `Unavailable`, exit 6 (L-5).
- **Signed listings.** A listing page carries `checkpoint` and `statement`, a signed note by the log key that says which keys were live in that tree, under the prefix, after `after` and up to `next` (to the end of the prefix without one):

  ```text
  hushrun-listing/v1
  origin <origin>
  size <tree size>
  root <base64 root>
  prefix <prefix>
  after <after, or empty>
  next <next, or empty>
  keys <count> <64 hex: SHA-256 of each key, in order, each followed by a newline>
  ```

  With the log key, the client refuses (`Unavailable`) a page without a statement, a statement about another page, or one read at a tree smaller than one the command already verified.
- **One history per command.** All the checkpoints a command meets, from receipts, listings, the log's latest, or events, must be one history: each is joined to the newest one verified by a consistency proof the ledger serves. When there is none, the command exits 6 and names both signed checkpoints, which together prove the fork.
- **Checkpoints in events.** Every event written to such a ledger carries the newest checkpoint its writer verified (`checkpoint`, E-4). A reader that finds one checks it as above, so a ledger that showed staging's writer one history and production's reader another fails production's plan. A rejected event's bytes must be the ones the log holds (`log/inclusion?key=`), and a listed key the ledger doesn't serve fails the read (E-8): the ledger can't drop an event by serving other bytes or none.
- **Recorded listings.** `plan --start` records, in each `started` it writes, the statements and keys of the listings its decision read for that scope (`started.listings`, E-5). On an HTTP ledger, `mark deployed` needs this attempt's `started` from `plan --start`; without it, it exits 2 and records nothing.
- **The monitor.** `hushrun repository monitor [--state FILE]` replays the log's leaves, checks that they hash to the latest signed checkpoint and that it extends the one `--state` remembered, and checks against them every checkpoint and listing that `started` events recorded, every deletion's GC statement (L-9), and that no object was overwritten. It reports a listing that left out keys the log already held with the ledger's statement as proof. [ADR 0061] It remembers the latest checkpoint in `--state` only when the run found nothing wrong and could check the one remembered there: a remembered checkpoint the log key doesn't verify is an `unverifiable_checkpoint` warning, and stays in the file, so that each run says so until someone removes it. The file is written whole, through a temporary file and a rename. JSON output `hushrun.repository.monitor/v1`; exit 0 when the ledger agrees with its log, 5 when it doesn't, 6 when the ledger can't be read, 2 when trust names no log key.
- **Endpoints**, for the reader role: `GET {base}/log/checkpoint` (the latest signed checkpoint, as text); `GET {base}/log/consistency?from=&to=` (`{"from", "to", "proof"}`); `GET {base}/log/inclusion?index=&size=` (`{"index", "size", "proof"}`), or `?key=` for the live object's `{"key", "sha256", "receipt"}`; `GET {base}/log/leaves?start=&limit=` (`{"leaves": [{"index", "kind", "name", "sha256", "gc", "statement"}]}`, at most 1000, as far as the latest checkpoint; [M19] a map leaf has kind `map`, an empty name, and the map's root as `sha256`). `GET {base}` names the log: `"log": {"origin", "key", "size"}`, a hint, never trusted.
- **Migration.** Until `hushrun.yaml` names the log key, the client checks nothing, warns once per command that the ledger keeps a log `hushrun.yaml` doesn't name, writes no `checkpoint` into events, and still records listings, whose statements verify later.

**L-12 Proven listings [M19].** An HTTP ledger that keeps a log (L-11) may also commit, in every checkpoint, to the listings a plan reads, as the hosted ledger does (RFC 0001 §4.7, H2): each listing page then comes with a proof that it is exactly the keys the committed map holds, and the monitor checks that every map follows from the log. `hushrun-tlog` implements every format below; ADR 0020 records the choices, and [ADR 0033] removed the witnesses it also added.

- **Map prefixes.** A key under `v1/events/<scope>/<digest>/` or `v1/deployed/<scope>/<stage or _>/` (L-2) is listed under that prefix, its first four segments and a `/`: the listings `plan` reads (L-8, P-7). Other keys are under no map prefix.
- **Key trees.** A map prefix's live keys, in byte order, are the leaves of an RFC 6962 tree whose leaf hashes are `SHA-256(0x00 || key)`. A key is live in the log's tree of n leaves from its create's leaf to its tombstone's (L-11).
- **The map.** A sparse Merkle tree over the 256 bits of `SHA-256(prefix)`, most significant first, compacted: an empty subtree hashes to 32 zero bytes, a subtree with one entry to that entry's hash, and any other to `SHA-256(0x01 || left || right)`. Each map prefix with live keys has one entry, whose hash is `SHA-256(0x00 || entry)`, every line ending in a newline:

  ```text
  hushrun-map-entry/v1
  <the prefix>
  <how many live keys it has, decimal>
  <64 lowercase hex: the root of its key tree>
  ```

  The map's root depends only on its entries; the empty map's root is 32 zero bytes.
- **Map leaves.** The ledger ends every batch with a map leaf (L-11) that records the root of the map of every leaf before it, so a checkpoint's last leaf commits to the map at that checkpoint. The empty log has no map leaf; its map is empty.
- **Listing proofs.** A listing page of a map prefix carries `proof`: the map's root, the inclusion proof of the map leaf (the checkpoint's last) in the checkpoint's tree, the map's path to the prefix (the siblings, deepest first, and the entry where the path ends: the prefix's own, another whose position shares the path, or none), and, when the map holds the prefix, the page's place in its key tree: the index of the page's first key, the key before it when there is one, and the complete subtrees of the keys before that and after the page (a range proof). With the key before the page at most `after`, and the page reaching the tree's end when `next` is empty, the proof says that the page is exactly the live keys after `after` up to `next`.
- **What readers check.** [ADR 0033] What L-11 says. A listing page's proof isn't checked: it would show only that the page is what the ledger's map holds in the history the ledger showed this reader, and a history shown to one reader alone is found only when it meets another (L-11). A map that leaves keys out is found by the monitor, as a listing that left keys out is.
- **The monitor** also checks every map leaf: the root it records must be the map of the log's leaves before it (`map_mismatch`, exit 5).
- **Witnesses** [M19]. [ADR 0033] Removed: `hushrun witness`, cosigned checkpoints, the witnesses and quorums of `transparency_log` (T-13), and `log/leaves?size=`. Nothing was released with them.
- **Migration.** Map leaves change no key, so monitors from before this rule stop at the first one (exit 6, an unknown kind): upgrade them with the ledger.

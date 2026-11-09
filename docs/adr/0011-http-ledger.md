# 0011. The HTTP ledger and policy upload

- **Status:** accepted
- **Date:** 2026-10-15
- **Rules:** L-1, L-3, L-4, L-5, L-6, L-10, T-4, E-8, §11.3, §12.4

## Context

RFC 0001 (accepted on 2026-10-14) adds an optional hosted ledger behind the `Ledger` trait
(§4.7). Phase 1 of the hosted product builds it now, ahead of M18's transparency log, so the
ledger is an ordinary store for the moment: the CLI verifies everything it reads (E-8), and
signed trust (T-8, [ADR 0012](0012-signed-trust.md)) keeps the ledger from serving the trust
that verifies it. The protocol was agreed with the server's implementation as a contract; L-10
is now its normative copy. This ADR records how the client implements it. Following plan §0.1,
each choice runs more checks, deploys less, or keeps credentials safer.

## Decision

1. **One backend, `HttpLedger`**, in `hushrun-ledger` behind the `cloud` feature, like the
   object stores. It blocks on the same kind of runtime and spends the same budget (ADR 0007
   items 2 to 4), now shared as `runtime::Io`.
2. **URLs** (L-1). `https://` anywhere; `http://` only on `localhost`, `127.0.0.1` and `[::1]`,
   for local development and tests. Normalization refuses userinfo, a query or a fragment rather
   than dropping them, lowercases only the scheme and host, drops one trailing `/`, keeps a port
   exactly as written, and allows only unreserved characters in the path. The normalized URL is
   compared by T-4 and is the audience of ID tokens, so it must be a single exact string.
3. **Tokens go only to a ledger named outside the repository.** Locally, `hushrun.yaml`'s
   `ledger` is the ledger when nothing else names one (T-4), and a change to it could point
   somewhere else. So an HTTP ledger gets the environment's credentials only when
   `HUSHRUN_LEDGER` or `--ledger` names it, which CI always does; otherwise its requests carry no
   token, with a warning that says how to name it. A `--trust` URL gets them only when it is
   inside that ledger.

   **Tokens come from the environment only**, in L-10's order: `HUSHRUN_LEDGER_TOKEN`, then
   `HUSHRUN_LEDGER_TOKEN_COMMAND` (reused for 5 minutes, as for `gs://` and `az://`), then a
   GitHub Actions ID token for the ledger's audience. The ID token is reused until a minute
   before its `exp`, read from the JWT's payload without verifying it: only the cache uses it,
   and the server verifies the token. The token endpoint must be `https://` (or loopback
   `http://`). The binary reads all of these in one place (`App::ledger_options`).
4. **Secrets stay out of output.** Tokens are a `Secret` type whose `Debug` hides them; reqwest
   marks the `Authorization` header sensitive; debug logs show the method and URL only. Errors
   name the ledger and the key, and for the token endpoint or a redirect, only the host: those
   URLs may carry query parameters that matter.
5. **Redirects** are followed only when they are 307 or 308, which keep the method and the
   body, and only within the origin the request started from (scheme, host and port), at most 5
   times. A 301, 302 or 303 would turn a write into a `GET` of another page. Anything else is
   `Unavailable`. reqwest's default would follow a redirect elsewhere with the token removed;
   refusing it is stricter, and the contract asks for it.
6. **Retries** (L-4): four attempts (the first and three retries), 10 s per request and 5 s to
   connect, backoff from 200 ms doubling with jitter, on timeouts, dropped connections, 429 and
   5xx. `Retry-After` is honored on 429 and 503 when the wait fits what is left of the budget,
   or 60 s for commands without one; otherwise the retries stop, and the error says why. Creates
   are retried too: a 412 whose top-level `sha256` equals the sent bytes' hash is a create that
   had succeeded (L-3). The client also accepts the hash inside `error`, and reads the object
   back when the answer has neither. Writes count only with the protocol's answers: 201 with
   the stored bytes' hash for an object create, 204 for configuration writes, root creates and
   deletes. Any other 2xx, such as a proxy's maintenance page, is `Unavailable`, so `attest` or
   `mark failed` never exits 0 for an event nobody stored.
7. **Reads are bounded.** An object is read up to 1 MiB and one byte, so an oversized one is
   rejected by verification (E-8 step 1) and the rest of the read goes on. Listing pages are read
   up to 4 MiB, batch-gets up to 150 MiB (100 objects of 1 MiB in base64), anything else up to
   64 KiB.
8. **Keys are checked before sending**, against L-10's alphabet, so an invalid key never
   reaches the server and keys need no percent-encoding.
9. **Listings** follow `next`, and refuse with `Unavailable` a `next` that isn't under the
   prefix, doesn't advance past `after` and the page's keys, or comes with an empty page, and a
   listing of more than 20,000 pages or ten million keys: a server must not keep a command
   forever. Keys outside the prefix are dropped and the result is sorted, whatever the server
   sends. `list_first(n)` asks for one page of n keys, and follows `next` only when the server
   answers a shorter page, so that the deployed index's 20 and 200 entries (L-8), and GC's
   choice of what to keep, never come up short.
10. **`get_many`** sends batch-gets of 100 keys, 16 in flight (§12.4). A batch whose answer
    doesn't list the requested keys in order fails as a whole.
11. **`delete` treats 404 as success**, because GC retries (L-9). From `GET {base}`, a
    listing or a batch-get, whose endpoints always exist, a 404 means that no ledger answers at
    that URL: it is `Unavailable`, like a file ledger whose directory is missing (ADR 0005
    item 9), never an empty answer. An `https://` ledger without `root/1.json` is told apart
    from such a URL the same way, by one listing.
12. **`create_config` joins the trait**, for root versions, which never change (T-8). On file,
    mem and object-store ledgers it is `append` under the same name; on an HTTP ledger, a
    create-only `PUT` of the configuration name.
13. **`doctor`** reads `GET {base}`. A protocol other than `hushrun-ledger-http/1`, or another
    repository than `hushrun.yaml`'s, is an error; a ledger that calls itself by another URL than
    the one in use is a warning, because CI's ID tokens would name the wrong audience. The clock
    check uses the `Date` of that request, which needs no token.
14. **Tests run against a fake.** `hushrun-testkit` has a small HTTP/1.1 server on the
    loopback, and on it a fake ledger that implements L-10: roles, L-2's shapes, create-only
    puts, pages, batch-gets, and faults to inject (statuses with `Retry-After`, dropped
    connections before or after the work, delays, redirects). The ledger conformance suite now
    uses keys in L-2's shapes, because a server that follows the contract refuses others, and
    runs on `HttpLedger` too. `hushrun-ledger` dev-depends on the testkit, which depends on it;
    the fake's API uses only standard types, so the two builds of the crate never meet.

**Policy upload** (the contract's §8, L-2):

15. **`plan --start` uploads the policy it evaluated** to `v1/policies/<sha256 hex>.yaml`,
    create-only, before it writes its first `started`, and only when it writes one: a plan that
    records nothing produces no event that names a policy. The bytes are the file's own, so the
    hex is the events' `policyDigest` (E-4).
16. **The upload never fails a plan.** A key that exists is a success: it is content-addressed.
    `Denied` is a warning, for ledgers whose IAM grants CI less than L-6's `v1/`. So are
    `Invalid`, from a server that doesn't know the key yet, and `Unavailable`: the `started`
    write right after it reports the outage itself (exit 6), within the same budget. Readers
    that decide ignore the key; only the index reads it, and the next plan uploads it again.
17. **GC keeps policies.** `ledger gc` lists only `v1/events/` and `v1/deployed/`, so policies
    are never candidates. Each is a few kilobytes, one per `hushrun.yaml` revision that
    deployed, and any remaining event may name one: without it, the index couldn't show what an
    old deployment required. Collecting the unreferenced ones would mean reading every remaining
    event, because `policyDigest` is inside them.

## Consequences

- An `https://` ledger is on the deploy path when a customer chooses it (CP-2): when it is
  down, plans exit 6 (L-5), as for a bucket.
- Nothing yet detects a ledger that withholds events: that is M18's log and M19's proofs (RFC
  0001 §4.7). Until then, an HTTP ledger is trusted for completeness as much as its operator.
- `HUSHRUN_LEDGER_TOKEN` is a new environment variable, and `ACTIONS_ID_TOKEN_REQUEST_URL` and
  `ACTIONS_ID_TOKEN_REQUEST_TOKEN` are read for HTTP ledgers.
- The conformance suite runs on the fake in every test run; nothing tests against the real
  hosted ledger yet.
- The ledger layout gains `v1/policies/`. L-6's grants to CI already cover creates under
  `v1/`, so no store needs new permissions.

## Alternatives considered

- **Following redirects to other hosts, without the token** (reqwest's default). It would still
  send requests where nobody configured a ledger. Rejected.
- **A separate `http` cargo feature.** It would save only `object_store`, and the binary needs
  both.
- **`wiremock` for the fake**, which the plan lists. A few hundred lines of standard library need
  no new dependency, and the CLI's tests can start the same fake.
- **Verifying ID tokens in the client.** The client doesn't trust the token; the server does.
- **Failing `plan --start` when the policy upload fails.** The policy informs the index and
  protects no decision, so failing would only add a way to block deploys.

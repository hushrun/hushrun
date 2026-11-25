# 0144. No time limit decides what a build reuses

- **Status:** proposed; built
- **Date:** 2026-11-25
- **Rules:** B-2, B-23, B-26, B-29, B-31, B-34, B-40, B-45, B-60, B-63, B-75 and B-80 changed

## Context

Under load, two-machine tests in this repository's CI started failing on pull requests that
didn't touch them: machine B reported "29 packages: 28 reused, 1 built". The failures came from
the product, not the tests. Six things let the machine's speed decide what B reused:

| # | Where | What a slow machine did |
|---|---|---|
| G1 | the end of a command (`upload::wait_at_end`) | On a person's machine, no upload not yet claimed went up once the command ended, and those in flight got 5 s. In CI they got up to a quarter of the compiles' cost, at most 20 s, or 2 minutes for trusted blocks. What was left was missing from the run's manifest, and so missing for every other machine. |
| G2 | `named_fetch`, the C/C++ manifest question | A Go, Gradle, C or C++ fetch waited at most 1 s, or half its compile, for `open` and the manifest. |
| G3 | `wait_for` | A download was given up once its projected finish passed half the block's compile cost. That is milliseconds for a small package. |
| G4 | `wait_for` | Until the first download had measured the link, every other queued fetch was a miss. |
| G5 | `named_fetch` | The manifest was taken under one lock and its prefetch marked as under way under another. A fetch between the two missed whatever the timing. |
| G6 | `Relay::uploads` | The upload loop marked itself running only once first polled. An end before that waited for no upload. |

The same kind of bound sat everywhere a build waits on something:
- a CI job's `open` (5 s, after which the run didn't share at all);
- a vouch (half the compile);
- a baked store's check (5 s);
- a sandbox's B-23 check (2 s, after which crates were left unchecked);
- `hushrun build fetch` (10 minutes);
- a wrapper's notice (2 s, after which a built block never got a marker);
- each adapter's own wait (half the compile plus 2 s);
- the Go cache program's watchdog (40 s);
- every backend request (a total deadline).

What one machine reuses of another's then depended on how loaded either was. A team's CI, a
person's laptop and an agent's sandbox all see this. Making only the tests deterministic would
leave the same behaviour in front of every client.

## Decision

1. **A wait ends on an event, never on a time budget.** Each wait ends on an answer, a failure or
   a stop. How long the work takes doesn't change what the run does.
   - **The end of a command** has no deadline unless a stop signal came (B-49). On every machine
     (a person's, a sandbox, a CI job), the upload loop goes on claiming what the run built until
     each upload is done or an answer puts it off. Then `close` names what went up. Asked to stop,
     the run cuts its uploads off at once and gives `close` 2 seconds, as before. A bake's build
     waits the same way. On a terminal, the run says `uploading N blocks before the run ends
     (Ctrl-C leaves the rest for the next run)`. The end also stops waiting once the relay rests
     after failures, or stops: what is left waits for the next run.
   - **A download** is waited for to its end, whatever its cost or this machine's rate. The rate
     meter and the machine's speed samples, which existed only to decide give-ups, are gone.
   - **A Go, Gradle, C, C++ or Xcode fetch** waits for `open`, its manifest and the manifest's
     prefetch, with no bound.
   - **A vouch** (B-34) waits for `open` and for its lookup. Resting after failures, a block no
     lookup named is a miss at once, since no new request goes.
   - **A CI job's wait for `open`** (B-2), the baked store's check (B-63), a sandbox's B-23 check
     and `hushrun build fetch` all run to their end.
2. **The one timer left is a liveness detector: silence.** It ends a wait only when the other side
   says nothing for a while. Each byte that comes resets it. It never ends a wait because the work
   is slow.
   - **The relay heartbeats.** While it works on an answer, it writes a space every second
     (`relay::HEARTBEAT`). JSON reads leading spaces as nothing, so the wire format is unchanged.
     A panicking answer stops its heartbeats, and the wrapper gets the connection's end, a miss.
   - **Every client of the relay** waits on 30 s of silence (`relay::WAIT`), whatever the compile
     cost: the rustc wrapper, its notices, the Go cache program, Gradle's cache, the C/C++
     launcher, tsc and Xcode. A frozen relay still costs a run one wait (B-40). The extra question
     that told a slow relay from a frozen one is gone, since the heartbeats tell them apart.
   - **The Go cache program's watchdog** answers a request after 40 s with no sign of life. Each
     heartbeat of the relay its handler waits on counts as life. Its end waits for busy handlers
     while any shows life.
   - **Gradle's cache** writes `HTTP/1.1 102 Processing` each heartbeat while a `GET` is worked
     on. Gradle's HTTP client reads past interim responses, and its socket timeout resets on
     them. No Gradle setting is changed.
   - **Backend and object-store requests** wait on silence, 30 s by default (`http::SILENCE`, the
     three `Tuning` silences): the head, then each piece of the body. A PUT's body is streamed a
     piece at a time and fails when it stops moving for the silence. A `Retry-After` within the
     silence is followed, since it is the server's own word. The ID token mint waits on its
     endpoint's silence too.
3. **Failures keep their rules.** Three failures in a row rest the relay for 30 s (B-26). A rest
   follows failures, never slowness. A server silent for the silence is one such failure.
4. **The two races are fixed.** The manifest is taken and its prefetch marked under one lock (G5),
   with a guard that clears the mark however the prefetch ends. The C/C++ prefetch counter gets the
   same guard. The upload loop is marked running when it is started, not when first polled (G6).

## Consequences

- What a machine reuses of another's is a function of what the backend answered. The two-machine
  tests need no sleep, no switch and no retry. A test that holds every racy point past its old
  bound (`every_package_is_reused_on_the_other_machine_however_slowly_the_backend_answers`) fails
  on any machine if a time-bounded wait comes back.
- **Slower in some cases, by choice.**
  - On a slow link, a build waits for downloads that compiling would have beaten.
  - A person's command waits for its uploads before exiting, and Ctrl-C leaves them for the next
    run.
  - A CI job waits for every upload, so a slow store makes it longer.
  - A backend that stops answering costs 30 s of silence for each kind of request, then the rest.
- **A frozen relay** costs a run one 30 s wait (B-40), as it did, and a hung handler of Go's cache
  program 40 s.
- **Residual bounds**, each a decision of its own:
  - B-31 mints no ID token once the end of a command has begun. A CI drain that outlives the
    token held fails its remaining uploads, visibly: they are left out of the manifest, and the
    run says so.
  - A bake's whole build has its own budget, a job's limit, not a reuse decision.
  - Publishing a baked image keeps its own (`PUBLISH_WITHIN`).
  - Xcode's own client of the compilation cache keeps whatever timeouts it has.
- `HUSHRUN_CC_MANIFEST_WAIT_MS` is gone: nothing reads it. `HUSHRUN_CC_RELAY_WAIT_MS` and
  `HUSHRUN_GOCACHE_BUDGET_MS` stay, for tests of a frozen relay and of the watchdog, and now set a
  silence, not a budget.

## Alternatives considered

- **A test-only switch that settles the waits** (`HUSHRUN_BUILD_SETTLE`, prototyped first). It
  made the tests deterministic and left clients exactly as exposed. Rejected: the bug is the
  product's.
- **Longer bounds.** Each is still a race, only rarer, and it costs every slow case the longer
  wait.
- **Re-running the first machine until nothing waits to upload** (a test helper). A retry, and it
  covers only G1.
- **Amending a run's manifest when its uploads finish later** (`hushrun flush` for build blocks).
  It would let a person's command exit before its uploads finish without the manifest falling
  short. It needs a protocol change and the backend's support. It is worth doing later, on top of
  this, not instead: a machine that reads before the amendment still misses, by timing.
- **Deciding a download's worth by the link, measured once per run, not as it goes.** Still
  measured by time, and a loaded machine still measures a slower link.

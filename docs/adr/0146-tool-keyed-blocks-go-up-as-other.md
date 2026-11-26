# 0146. Blocks a build tool keys go up as class `other`

- **Status:** proposed; built
- **Date:** 2026-11-25
- **Rules:** B-30, B-81 and B-175 changed

## Context

Gradle's entries (`gradle/1`, ADR 0104) and Maven's modules (`maven/1`, ADR 0113) are keyed by
the tool, not by hushrun, and their bytes needn't be the same from one execution to the next: a
test report holds its dates, and a jar holds its files' dates. Both ADRs say a difference between
two copies is expected and "logged and nothing else" (B-81, B-175).

Both went up as class `library`. At `begin`, when the trusted scope holds a `library` under the
same key with other outputs, the server does more than refuse the second copy with `differs`: it
suspends the key in the repository for 7 days, deletes the stored trusted block, and counts the
suspension against the run, whose uploads are refused once it has suspended 20 keys
(`docs/contracts/build-v0.md` §4 and §6, in cloud). A trusted `begin` of a `library` also deletes
the key's differing copies in the repository's `ci/` scopes. On `differs` the CLI discarded its
own copy of a library too. For Maven, two honest builds on main would differ almost every time:
main's second build would empty the store of the first one's modules and, after 20 modules, have
its uploads refused.

Neither kind is released; the newest release is 0.19.1.

## Decision

1. **The wire class is `other` for a kind the build tool keys** (`Kind::tool_keyed`: Gradle and
   Maven). The server already takes `other`, stores it, and never compares it, so no server
   change is needed. The block's own record on the machine keeps `library`, and the CLI's class
   enum (what a compiler call produces) gains no `other`.
2. **`differs` for such a block leaves the block built here as it is.** It is neither discarded
   (a library's answer) nor kept to this machine with B-34's line (a linked output's answer,
   whose words are about link environments). The marker is done.
3. **No report about such a block says it differs.** The server's report route suspends a key
   on a trusted job's `differs` report whatever the block's class. Gradle's and Maven's caches
   report only a rejected download today; the relay drops a `differs` or `link_differs` report
   for these kinds wherever it comes from, so a later caller can't send one either.
4. **A manifest of such a kind is prefetched whole, in its order.** The prefetch took only
   libraries, proc macros and build scripts from a manifest; a block of class `other` waited to
   be asked for. Gradle and Maven ask for each entry as they reach its task or module, so each
   would wait for its own download.

## Alternatives

- **A server rule by kind** (never suspend `gradle/1` or `maven/1`): a server change, and every
  later tool-keyed kind would need one too. The class says what the server should do with a
  difference; the kind needn't.
- **`program`**: the server doesn't compare programs either, but the CLI's own rules treat a
  program as a linked output (B-34), and the dashboard would count Gradle's entries as programs.
- **Keeping `library` and asking Gradle and Maven for reproducible outputs**: neither tool
  promises it, and a build that turns it on is the user's choice, not hushrun's.

## Consequences

- Two trusted builds of one Gradle task or Maven module with other bytes no longer suspend its
  key, delete the trusted copy or count toward a run's flag. The second copy is refused as
  `differs` and stays on the machine that built it.
- A trusted Gradle or Maven block no longer deletes a branch's differing copy in a `ci/` scope.
  That is wanted: neither copy is wrong.
- A client older than this one, if one were released with these kinds, would still send
  `library`. None was released.
- The server needs a test that holds both sides of the rule: two trusted `begin`s of class
  `other` differ and suspend nothing, and the same two of class `library` suspend (cloud).

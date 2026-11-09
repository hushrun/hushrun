# F36: a package graph hushrun can't read

**Pattern.** A pnpm workspace whose `web` gets `@acme/core`, a package of the workspace, from
`github:acme/core`: hushrun can't tell whether that is the package beside it.

**Mirrors.** A dependency on a sibling by a protocol hushrun doesn't place.

**Expected after Phase 5.** One scope holds the repository, and the plan says why. Each package's
`test` is still wrapped, with no scope: its skip waits for no change anywhere, as a
repository of one scope's does.

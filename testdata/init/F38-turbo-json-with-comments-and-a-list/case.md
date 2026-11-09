# F38: a turbo.json with comments and a list of its own

**Pattern.** An npm workspace on Turborepo 2, whose turbo.json has comments, trailing commas and
a `globalPassThroughEnv` of its own. CI runs `npx turbo run lint test` itself.

**Mirrors.** turbo.json files as n8n, Prisma, PostHog and Tailwind CSS write them: Turborepo
reads JSON with comments.

**Expected after Phase 5.** Each package's `lint` and `test` are wrapped with its scope. The list
gets `ACTIONS_ID_TOKEN_REQUEST_*`, `GITHUB_*` and `HUSHRUN_*` after its own entries, and the rest
of the file, comments and trailing commas included, keeps its bytes. CI's line stays as it is,
and the job lists the packages' scripts, each predicted to skip.

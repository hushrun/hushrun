# F03b: reusable workflows that run a wrapped script

**Pattern.** `ci.yml` runs on pushes and pull requests and calls `uses: ./.github/workflows/unit.yml`,
whose job runs `npm test`; `shared.yml` runs `npm test` too, and only other repositories call it.
package.json's `test` is `vitest run`.

**Mirrors.** The survey's 113 reusable test jobs, here with a wrapped script. n8n's
`test-workflow-scripts-reusable.yml` reads like one to init, but its
`npm test --prefix=.github/scripts` runs another package's `test`, and it checks out with
`useblacksmith/checkout`, which init doesn't count as a checkout.

**Expected today (Phase 0).** init wraps the script. In a reusable workflow, GitHub names the
caller's file, so the run can't find its job and runs: `unit` says so in the run's own words,
naming `ci.yml`; `shared`'s caller isn't here, so the plan says `runs` with `reusable`, and no text.

**Expected after Phase 3.** `HUSHRUN_WORKFLOW` in each called job's `env` names its own file, and
the scripts skip, each on its own pass.

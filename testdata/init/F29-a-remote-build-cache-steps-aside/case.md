# F29: a remote build cache already reuses results

**Pattern.** A Turborepo whose workflow sets `TURBO_TOKEN` from a secret, and `TURBO_TEAM`, in its
`env`: job `test` runs `pnpm turbo run test`, and the root's `test` script is `turbo run test`. Job
`android` runs `./gradlew test --build-cache` in `android/`, whose `settings.gradle.kts` sets up a
remote `buildCache`.

**Mirrors.** Turborepo's remote cache on Vercel, and Gradle builds with a remote build cache: 19 of
the survey's repositories run Bazel, Gradle's cache, Nx or Turborepo.

**Expected after Phase 4.** init steps aside: each tool already reuses its own results across
machines, and wrapping it would cache twice. It leaves `pnpm turbo run test` and the `test`
script, saying `Turborepo reuses its own results across machines (remote cache), so init leaves it
to Turborepo`, and `./gradlew test --build-cache`, with check `test`, saying the same of Gradle.
Each has `ciWhy: stepped-aside`. The token comes from a secret, and the reason is still the
remote cache's. With nothing to wrap, init writes no edit. Without the token and the remote
`buildCache`, init would wrap the script and the Gradle line, as before.

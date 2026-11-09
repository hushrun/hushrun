# F20: a workflow another repository holds

**Pattern.** `ci.yml` calls `uses: acme/shared/.github/workflows/ci.yml@v1`, a workflow of another
repository, and has a job of its own that runs `go test ./...`.

**Mirrors.** Organizations that keep their CI in a shared repository.

**Expected after Phase 3.** The called file isn't here, so init has nothing of it to read or write,
and the calling job gets nothing: a run there can't read its file, so it runs, as today. The job of
this file is wrapped and skips on its own pass.

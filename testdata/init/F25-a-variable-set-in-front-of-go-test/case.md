# F25: a variable set in front of `go test`

**Pattern.** `go test ./...`, then `GOARCH=386 go test ./...` in the same job, on Linux.

**Mirrors.** Go projects that also test on 32-bit, such as many standard-library-style modules.

**Expected after Phase 4.** Both wrapped. The second becomes
`hushrun run test-test -- env GOARCH=386 go test ./...`, so the variable is part of the command and
of the key: a pass with one `GOARCH` never skips another. Its name is the job's and the check's, as
for any second check of one name in a job.

# F06: a pnpm workspace whose root `test` only fans out

**Pattern.** The root package.json's `test` is `pnpm -r test`, which runs each package's own
`test`; `lint` is a plain `eslint .`.

**Mirrors.** Vite and Vitest: in the survey, 8 of 188 JavaScript test jobs could skip.

**Expected today (Phase 0).** init leaves `test` ("it only runs other scripts or the package
manager"), wraps `lint`, and the job lists the lint script only. The packages' own scripts aren't
read.

**Expected after Phase 5.** Each package's `test` is wrapped, under a scope of its own.

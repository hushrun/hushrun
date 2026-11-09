# F26: make targets by their recipe

**Pattern.** `make test-ci -j1 V=1`, whose recipe runs pytest through a variable and whose
prerequisite runs ruff; `make lint-ci` in a job with a Codecov upload; `make -C web ui-test`,
whose recipe runs a package's test script after a `cd`; and `make test-deploy`, which runs the
tests, then pushes an image.

**Mirrors.** Node's `make test-ci`, Gitea's `make test-backend` and Prometheus' `make ui-test`;
the last, a negative.

**Expected after Phase 4.** init reads each target's recipe, and its prerequisites. It wraps
`make test-ci`, `make lint-ci` and `make -C web ui-test`, each named after its target, and each
predicts `ci: skips`: `lint-ci` too, since every check its recipe runs is a linter, and Codecov
reads only coverage. The pytest line beside it runs for its reports, as in F14. It leaves
`make test-deploy` with `its recipe pushes an image (docker push)`.

Node's own `test-ci` runs `tools/test.py` and `out/Release/cctest`, which the table doesn't know,
so init leaves it unseen there; Gitea's and Prometheus' are wrapped.

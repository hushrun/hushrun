# F27: more runners

**Pattern.** A job each for `./python -m test` (CPython's own suite), `python -Wall
tests/runtests.py -v2` (Django's own script), `python -m unittest discover -s scripts`,
`./gradlew build` and `bundle exec rake`.

**Mirrors.** CPython, Django, the script tests of PyTorch, Codex and LangChain, Spring Boot and
Rails.

**Expected after Phase 4.** init wraps `python -m unittest` (the standard library's runner),
`./gradlew build` (a build runs the tests) and `bundle exec rake` (its default task, the tests),
and predicts `ci: skips` for each. `python -m unittest` and `bundle exec rake` are both called
`test` at first, so the second becomes `rails-test`, after its job. It leaves `./python -m test` and `tests/runtests.py` unseen: the table holds
what at least two repositories of the survey run, or a language's own runner, and each of these is
one repository's own. And `python -m` is a launcher, so a row for `python -m test` would be a row
for the shell's `test`.

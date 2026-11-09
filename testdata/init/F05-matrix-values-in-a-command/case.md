# F05: a matrix value in the command line

**Pattern.** `pytest ${{ matrix.args }}`, in a job whose matrix lists the arguments.

**Mirrors.** CPython and many others: 66 of the commands init 0.6.0 left had `$` words.

**Expected today (Phase 0).** init leaves the line: CI fills in its words, so init can't tell the
command (`status: left`).

**Expected after Phase 4.** Wrapped, since a matrix value is safe; the key then differs per value.

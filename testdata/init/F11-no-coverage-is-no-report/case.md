# F11: a flag that turns coverage off

**Pattern.** `vendor/bin/phpunit --no-coverage`, and no step after it that reads reports.

**Mirrors.** Laravel's `tests.yml`, where init read `--no-coverage` as a flag that writes a
coverage report, and left the line.

**Expected after step 1.1.** init wraps the line (`status: wrapped`) and predicts that CI skips it
(`ci: skips`): the line writes no report, and no step reads one.

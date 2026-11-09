# F12: RSpec's JUnit formatter, published

**Pattern.** `bundle exec rspec --format RspecJunitFormatter --out rspec.xml`, then
`EnricoMi/publish-unit-test-result-action` reading `rspec.xml`.

**Mirrors.** Ruby projects that publish RSpec's results.

**Expected after Phase 1.** init reads `--format RspecJunitFormatter` as writing a report (step
1.1), wraps the line (step 1.5), and predicts `ci: runs` with `ciWhy: reports`.

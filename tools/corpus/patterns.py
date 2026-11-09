"""Regex vocabulary for classifying CI commands and actions.

Ported unchanged from the survey's method/patterns.py. It is the harness's own reading of CI files,
kept apart from hushrun's on purpose: A1 compares this reader catalog (COV_UPLOAD_*, TEST_REPORT_*,
the artifact rule in analyze.py) with the CLI's, and the harness is only useful where they can
disagree.
"""
import re

I = re.IGNORECASE

# ---------------------------------------------------------------- test commands
# (family, regex). Applied to one logical shell line at a time.
TEST_CMDS = [
    ("js-pm-script", re.compile(r"(?<![\w-])(npm|pnpm|yarn|bun)\s+((-[\w-]+(=\S+)?|--filter[= ]\S+|-F\s+\S+|-C\s+\S+|--dir[= ]\S+|--workspace[= ]\S+|-w\s+\S+|workspaces\s+foreach(\s+-\S+)*|-r|--recursive|--if-present|--parallel|--stream|--silent|-s)\s+)*(run(-script)?\s+)?(test|t|tests|ci|check|unit|e2e|coverage|cov|vitest|jest)([:_.-][\w:.-]+)?(?![\w:-])")),
    ("js-pm-script", re.compile(r"(?<![\w-])(npm|pnpm|yarn|bun)\s+(run(-script)?\s+)?(test|tests?|unit|e2e|spec|ci-test)[:_.-][\w:.-]+")),
    ("bun-test", re.compile(r"(?<![\w-])bun\s+test\b")),
    ("deno-test", re.compile(r"(?<![\w-])deno\s+(task\s+)?test\b")),
    ("node-test", re.compile(r"(?<![\w-])node\s+(\S+\s+)*--test\b")),
    ("jest", re.compile(r"(?<![\w/-])(jest|react-scripts\s+test|craco\s+test)(\s|$)")),
    ("vitest", re.compile(r"(?<![\w/-])vitest(\s|$)")),
    ("mocha", re.compile(r"(?<![\w/.-])(mocha|ava|uvu|karma\s+start|ng\s+test)(\s|$)|node\s+Makefile\s+mocha")),
    ("playwright", re.compile(r"(?<![\w/-])playwright\s+test\b")),
    ("cypress", re.compile(r"(?<![\w/-])cypress\s+run\b|cypress-io/github-action")),
    ("turbo", re.compile(r"(?<![\w/-])turbo\s+(run\s+)?[\w:,-]*test")),
    ("nx", re.compile(r"(?<![\w/-])nx\s+(run-many|affected|run|test)\b[^\n]*test|(?<![\w/-])nx\s+test\b")),
    ("lerna", re.compile(r"(?<![\w/-])lerna\s+run\s+test")),
    ("pytest", re.compile(r"(?<![\w/-])(py\.test|pytest)(\s|$)|-m\s+pytest\b")),
    ("tox", re.compile(r"(?<![\w/-])tox(\s|$)")),
    ("nox", re.compile(r"(?<![\w/-])nox(\s|$)")),
    ("python-unittest", re.compile(r"-m\s+(unittest|test(\s|$)|test\.regrtest)|runtests\.py|(?<![\w/-])spin\s+test\b|(?<![\w/-])hatch\s+(run\s+)?[\w:-]*test")),
    ("go-test", re.compile(r"(?<![\w/-])go\s+test\b|(?<![\w/-])gotestsum\b|(?<![\w/-])ginkgo\b")),
    ("cargo-test", re.compile(r"(?<![\w/-])cargo\s+(\+\S+\s+)?(-\S+\s+)*(test|nextest|llvm-cov|tarpaulin|miri\s+test|hack\s+(\S+\s+)*test|insta\s+test|xtask\s+test|t)\b|(?<![\w/-])cross\s+test\b|(?<![\w/.-])\./x(\.py)?\s+(\S+\s+)*test\b|(?<![\w/-])x\s+test\b")),
    ("gradle", re.compile(r"(?<![\w/.:-])(\./)?gradlew?(\.bat)?(?=\s)")),
    ("maven", re.compile(r"(?<![\w/.:-])(\./)?(mvnw?|build/mvn|[\w./-]*mvn_\w+\.(ps1|sh))(\.cmd)?(?=\s)")),
    ("sbt", re.compile(r"(?<![\w/-])(build/)?sbt\b[^\n]*test")),
    ("bazel-test", re.compile(r"(?<![\w/-])(bazel|bazelisk)\s+(\S+\s+)*(test|coverage)\b")),
    ("ctest", re.compile(r"(?<![\w/-])ctest\b|(?<![\w/-])meson\s+test\b|(?<![\w/-])ninja\s+(\S+\s+)*(test|check)\b|cmake\s+--build[^\n]*--target\s+(test|check|unit)\b(?![\w-]*deps)")),
    ("make-test", re.compile(r"(?<![\w/-])(make|gmake|nmake|mingw32-make)\s+((-\S+|\w+=\S+)\s+)*(test|check|tests|unittest|unit-test|unit|test-[\w-]+|tests-[\w-]+|[\w-]*-tests?|testall|test_[\w]+|ci)(?![\w-])")),
    ("just-task", re.compile(r"(?<![\w/-])(just|task|mage)\s+((-\S+)\s+)*(test|check|ci|unit)[\w:-]*\b")),
    ("ruby", re.compile(r"(?<![\w/-])(rspec|rails\s+test|bin/rails\s+test|bin/rspec|bin/test|rake(\s+\S+)*\s+(test|spec)[\w:]*|bundle\s+exec\s+(rake|rspec|rails\s+test|ruby\s+-Itest))\b|(?<![\w/-])brew\s+tests\b|bin/turbo_rspec|bin/rake\s+(test|spec)")),
    ("php", re.compile(r"(?<![\w/-])(phpunit|paratest|pest)(\s|$)|vendor/bin/(phpunit|pest|paratest|simple-phpunit)|composer\s+(run(-script)?\s+)?test\b|simple-phpunit")),
    ("dotnet", re.compile(r"(?<![\w/-])dotnet\s+test\b|(?<![\w/-])(\./)?(build|eng/build)\.(sh|cmd|ps1)\b[^\n]*\s-test\b|Invoke-Pester|Start-PSPester|(?<![\w/-])dotnet\s+(build|msbuild)\b[^\n]*-t:?Test\b")),
    ("binary-test", re.compile(r"(\$\{\{\s*matrix\.bin\s*\}\}|(?<![\w-])bin/\S+)\s+--test\b")),
    ("docker-compose", re.compile(r"docker[- ]compose\b[^\n]*\brun\b[^\n]*\b(build|test)[\w-]*")),
    ("swift", re.compile(r"(?<![\w/-])swift\s+test\b|xcodebuild\b[^\n]*\b(test|test-without-building)\b|(?<![\w/-])fastlane\s+(scan|test|tests)\b")),
    ("elixir", re.compile(r"(?<![\w/-])mix\s+(test|coveralls[\w.]*)\b")),
    ("dart", re.compile(r"(?<![\w/-])(flutter|dart)\s+test\b|(?<![\w/-])dart\s+(\S+\s+)*dev/bots/test\.dart|dev/bots/test\.dart")),
    ("zig", re.compile(r"(?<![\w/-])zig\s+build\s+test")),
    ("script", re.compile(r"(?<![\w-])(\./|bash\s+|sh\s+|python3?\s+|pwsh\s+|powershell\s+|ruby\s+|node\s+)?[\w./${}-]*(?<![\w])(run[-_]?tests?|test[-_]?runner|runtests?|run[-_]test[-_]suite|test[-_]all|unit[-_]?tests?|ci[-_]?test|test[-_]ci|tests?)(\.(sh|py|ps1|bash|js|mjs|ts|rb|cmd|bat))\b")),
    ("script", re.compile(r"^(\w+=\S+\s+)*(time\s+)?(\./|bash\s+|sh\s+)?[\w.-]*/?[\w./-]*(?<![\w])(test|tests|runtest|runtest-\w+)(\s|$)")),
]

# ---------------------------------------------------------------- coverage generation
COV_GEN = [
    ("pytest-cov", re.compile(r"--cov(\b|=)|--cov-report|(?<![\w-])coverage\s+(run|xml|combine|lcov|json|report)\b|-m\s+coverage\b", I)),
    ("js-coverage", re.compile(r"(?<![\w-])(c8|nyc|istanbul)\s|--coverage(\b|=)|--experimental-test-coverage|coverage\.enabled|(?<![\w-])test[:-]cov(erage)?\b|:coverage\b|\bcoverage:\w+|run\s+coverage\b")),
    ("go-cover", re.compile(r"-coverprofile|-covermode|-coverpkg|(?<![\w-])-cover(\s|$)|go\s+tool\s+cover|goveralls|gcov2lcov|-test\.gocoverdir|GOCOVERDIR")),
    ("rust-cov", re.compile(r"llvm-cov|tarpaulin|grcov|instrument-coverage|cargo-llvm-cov")),
    ("jvm-cov", re.compile(r"jacoco|kover|scoverage|coverageReport\b|cobertura-maven", I)),
    ("ruby-cov", re.compile(r"simplecov|COVERAGE\s*[:=]\s*['\"]?(true|1|yes)", I)),
    ("php-cov", re.compile(r"--coverage-(clover|xml|text|html|cobertura|php|crap4j)|XDEBUG_MODE[:=]\s*['\"]?coverage|coverage:\s*['\"]?(pcov|xdebug)|\bpcov\b")),
    ("dotnet-cov", re.compile(r"coverlet|XPlat Code Coverage|CollectCoverage=true|--collect[:= ]\"?Code Coverage|dotnet-coverage", I)),
    ("c-cov", re.compile(r"(?<![\w-])(gcov|lcov|gcovr|llvm-profdata|fastcov)\b|-fprofile-arcs|-ftest-coverage|ENABLE_COVERAGE|-DCOVERAGE|--enable-coverage|CODE_COVERAGE|COVERAGE_BUILD|coverage-build|=coverage\b")),
    ("swift-cov", re.compile(r"-enableCodeCoverage|--enable-code-coverage|xccov|slather")),
    ("elixir-cov", re.compile(r"mix\s+test\s+(\S+\s+)*--cover|excoveralls|mix\s+coveralls")),
    ("dart-cov", re.compile(r"(flutter|dart)\s+test\s+(\S+\s+)*--coverage|coverage:format_coverage|package:coverage")),
    ("bazel-cov", re.compile(r"bazel(isk)?\s+(\S+\s+)*coverage\b|--combined_report")),
    ("deno-cov", re.compile(r"deno\s+coverage|deno\s+test\s+(\S+\s+)*--coverage")),
]

# ---------------------------------------------------------------- JUnit / machine-readable test reports
JUNIT_GEN = re.compile(
    r"--junit-?xml|--junit_xml|junit_family|jest-junit|JEST_JUNIT|reporters?[= ]\S*junit|--reporter[= ]junit|"
    r"mocha-junit-reporter|go-junit-report|--junitfile|gotestsum[^\n]*--jsonfile|--output-junit|"
    r"rspec_junit_formatter|RspecJunitFormatter|--log-junit|--logger[: =]\"?(trx|junit|nunit|xunit)|"
    r"xcpretty[^\n]*-r\s+junit|-resultBundlePath|PLAYWRIGHT_JUNIT|--reporter[= ]\S*(json|blob)|"
    r"--json-report|--report-log|ctrf|test-results?/|surefire-reports|TEST-\*\.xml|junit\.xml|"
    r"junit-report|junit_report|\.trx\b|nextest[^\n]*--profile[= ]ci|--message-format\s+json|"
    r"testlogs|--test_output|-json\s|go\s+test[^\n]*-json|--format\s+junit|--junit\b|junit-reporter|"
    r"--report-format|xunit|test_results|test-reports?/|TestResults/", I)

# ---------------------------------------------------------------- actions (uses:)
COV_UPLOAD_ACTIONS = [
    ("codecov", re.compile(r"^codecov/codecov-action", I)),
    ("codecov", re.compile(r"^codecov/test-results-action", I)),  # same vendor; counted separately below
    ("coveralls", re.compile(r"^coverallsapp/github-action|coveralls", I)),
    ("sonar", re.compile(r"^SonarSource/|sonarcloud|sonarqube", I)),
    ("codeclimate/qlty", re.compile(r"^paambaati/codeclimate-action|^qltysh/qlty-action|codeclimate", I)),
    ("codacy", re.compile(r"^codacy/", I)),
    ("deepsource", re.compile(r"deepsource", I)),
    ("datadog", re.compile(r"^DataDog/|^datadog/", I)),
]
COV_COMMENT_ACTIONS = re.compile(
    r"^(py-cov-action/python-coverage-comment-action|MishaKav/(pytest|jest)-coverage-comment|"
    r"ArtiomTr/jest-coverage-report-action|davelosert/vitest-coverage-report-action|"
    r"irongut/CodeCoverageSummary|5monkeys/cobertura-action|insightsengineering/coverage-action|"
    r"orgoro/coverage|romeovs/lcov-reporter-action|zgosalvez/github-actions-report-lcov|"
    r"madrapps/jacoco-report|cicirello/jacoco-badge-generator|gwatts/go-coverage-action|"
    r"vladopajic/go-test-coverage|fgrosse/go-coverage-report|k1LoW/octocov-action|"
    r"livewing/lcov-job-summary|barecheck/code-coverage-action|danhunsaker/clover-reporter-action|"
    r"slavcodev/coverage-monitor-action|ewjoachim/coverage-comment-action|"
    r"anuraag016/Jest-Coverage-Diff|AthleticNet/comment-test-coverage|smokeshow|samuelcolvin/coverage-badge)", I)
TEST_REPORT_ACTIONS = [
    ("dorny/test-reporter", re.compile(r"^dorny/test-reporter", I)),
    ("EnricoMi/publish-unit-test-result-action", re.compile(r"^EnricoMi/publish-unit-test-result-action", I)),
    ("mikepenz/action-junit-report", re.compile(r"^mikepenz/action-junit-report", I)),
    ("test-summary/action", re.compile(r"^test-summary/action", I)),
    ("ctrf-io/github-test-reporter", re.compile(r"^ctrf-io/", I)),
    ("phoenix-actions/test-reporting", re.compile(r"^phoenix-actions/test-reporting", I)),
    ("scacap/action-surefire-report", re.compile(r"^scacap/action-surefire-report", I)),
    ("pmeier/pytest-results-action", re.compile(r"^pmeier/pytest-results-action", I)),
    ("codecov test analytics", re.compile(r"^codecov/test-results-action", I)),
    ("datadog test optimization", re.compile(r"^DataDog/(junit-upload-github-action|test-visibility-github-action)|^datadog/", I)),
    ("trunk flaky tests", re.compile(r"^trunk-io/analytics-uploader", I)),
    ("buildpulse", re.compile(r"^buildpulse/", I)),
    ("other junit action", re.compile(r"junit|test-report|test-result|testreport|unit-test-result|test_result", I)),
]
# CLI-level uploads in run: text
COV_UPLOAD_CLI = [
    ("codecov", re.compile(r"codecov\.io/bash|uploader\.codecov\.io|codecovcli|codecov-cli|(?<![\w-])codecov\s+(upload|do-upload|upload-process|create-commit|-[a-zA-Z])|cli\.codecov\.io|codecov/codecov@|codecov/upload|CODECOV_TOKEN", I)),
    ("coveralls", re.compile(r"(?<![\w-])(coveralls|goveralls|cargo\s+coveralls|python-coveralls|php-coveralls)\b|COVERALLS_REPO_TOKEN", I)),
    ("sonar", re.compile(r"sonar-scanner|sonar:sonar|gradlew?\s+[^\n]*\bsonar(qube)?\b|SONAR_TOKEN", I)),
    ("codeclimate/qlty", re.compile(r"cc-test-reporter|qlty\s+coverage", I)),
    ("codacy", re.compile(r"codacy-coverage-reporter|CODACY_PROJECT_TOKEN", I)),
    ("datadog", re.compile(r"datadog-ci\s+(coverage|junit)\s+upload", I)),
]
TEST_REPORT_CLI = [
    ("codecov test analytics", re.compile(r"report[_-]type[:= ]+['\"]?test_results|do-upload[^\n]*--report-type\s+test_results|codecovcli[^\n]*test_results", I)),
    ("datadog test optimization", re.compile(r"datadog-ci(@\S+)?\s+junit|setup-datadog-ci|DD_CIVISIBILITY|--ddtrace\b|DD_TEST_|ddtrace-run", I)),
    ("custom test-stats pipeline", re.compile(r"upload[-_]test[-_]stats|test[-_]stats[-_]upload|testgrid|resultdb", I)),
    ("trunk flaky tests", re.compile(r"trunk\s+flakytests|trunk-analytics-cli", I)),
    ("buildpulse", re.compile(r"buildpulse", I)),
    ("launchable", re.compile(r"launchable\s+(record|subset|verify)", I)),
    ("buildkite test engine", re.compile(r"test-collector|BUILDKITE_ANALYTICS_TOKEN|test-engine|bktec", I)),
    ("develocity build scan", re.compile(r"DEVELOCITY_ACCESS_KEY|GRADLE_ENTERPRISE_ACCESS_KEY|com\.gradle\.develocity|com\.gradle\.enterprise|develocity-maven-extension|gradle-enterprise-maven-extension|gradle/develocity-actions|develocity-injection|\bge\.(spring|apache|quarkus|elastic|solutions-team|gradle|junit|micronaut|hibernate)\.(io|org|com)|develocity\.[\w-]+\.(io|org|com)|gradle-enterprise\.[\w-]+\.(io|org|com)|gradlew?\b[^\n]*\s--scan\b")),
    ("currents/cypress cloud", re.compile(r"currents\.dev|@currents/|currents-dev|CURRENTS_(PROJECT_ID|RECORD_KEY|API_KEY)|cypress\s+run[^\n]*--record|CYPRESS_RECORD_KEY")),
    ("allure", re.compile(r"allure", I)),
]
STEP_SUMMARY = re.compile(r"GITHUB_STEP_SUMMARY|core\.summary|\$env:GITHUB_STEP_SUMMARY")
PR_COMMENT_ACTIONS = re.compile(
    r"^(marocchino/sticky-pull-request-comment|thollander/actions-comment-pull-request|"
    r"peter-evans/create-or-update-comment|mshick/add-pr-comment|edumserrano/find-create-or-update-comment|"
    r"actions/github-script)", I)

# ---------------------------------------------------------------- caches, change detection, merge queues
BUILD_CACHE = [
    ("bazel", re.compile(r"(?<![\w/-])(bazel|bazelisk)\s+(build|test|run|coverage|query)|bazel-contrib/setup-bazel|--remote_cache|--remote_executor|bazelbuild/setup-bazelisk|BAZEL_", I)),
    ("nx", re.compile(r"(?<![\w/-])nx\s+(affected|run-many|run|test|build|lint)\b|NX_CLOUD|nx-cloud|nrwl/nx-set-shas", I)),
    ("turbo", re.compile(r"(?<![\w/-])turbo\s+(run\s+)?\w|TURBO_TOKEN|TURBO_TEAM|TURBO_REMOTE|turbo-cache|vercel/turbo", I)),
    ("gradle-cache", re.compile(r"gradle/actions/setup-gradle|gradle/gradle-build-action|--build-cache|org\.gradle\.caching|develocity|gradle-enterprise", I)),
    ("sccache/ccache", re.compile(r"sccache|ccache|hendrikmuhs/ccache-action|mozilla-actions/sccache-action", I)),
    ("rust-cache", re.compile(r"Swatinem/rust-cache", I)),
    ("actions/cache", re.compile(r"^actions/cache|^actions/cache/(restore|save)", I)),
]
CHANGE_DETECT = [
    ("paths-filter action", re.compile(r"^dorny/paths-filter|^tj-actions/changed-files|^step-security/changed-files|^Ana06/get-changed-files|^jitterbit/get-changed-files|^trilom/file-changes-action|^marceloprado/has-changed-path|^MarceloPrado/has-changed-path|^fkirc/skip-duplicate-actions|^getsentry/paths-filter", I)),
    ("git diff script", re.compile(r"git\s+diff\s+[^\n]*--name-only|git\s+diff\s+--name-only|changed[-_]files|files[-_]changed|affected|--since[= ]|nx-set-shas|base[-_]sha", I)),
]
MERGE_QUEUE_FILES = {
    ".mergify.yml": "mergify", ".github/mergify.yml": "mergify", ".mergify/config.yml": "mergify",
    "bors.toml": "bors", "rust-bors.toml": "bors", ".kodiak.toml": "kodiak",
    ".github/.kodiak.toml": "kodiak", ".aviator/config.yml": "aviator",
}

GH_HOSTED = re.compile(
    r"^(ubuntu|windows|macos)-(latest|\d+(\.\d+)?)(-arm|-arm64|-intel|-large|-xlarge|-amd64)?$|"
    r"^ubuntu-(24|22|20)\.04(-arm)?$|^windows-11-arm$|^ubuntu-slim$", I)
THIRD_PARTY_RUNNER = re.compile(
    r"namespace-profile|nscloud|blacksmith|depot-|buildjet|warp-|ubicloud|runs-on=|cirrus-|"
    r"actuated|arc-runner|codebuild|gha-runner-scale-set|cw-|equinix|aws-|gcp-|azure-", I)

"""Pick the CI and test-config files worth reading from a repository's full file list.

Ported unchanged from the survey's method/select_paths.py, so that a checkout holds the same files.

Usage: python3 -I select_paths.py FILELIST > sparse-patterns
Reads one path per line (git ls-tree -r --name-only HEAD) and prints anchored, escaped
sparse-checkout patterns (non-cone mode) for the files to fetch.
"""
import re
import sys

ROOT_FILES = {
    # CI providers
    ".travis.yml", ".gitlab-ci.yml", ".cirrus.yml", ".cirrus.star", "appveyor.yml",
    ".appveyor.yml", ".drone.yml", "bitbucket-pipelines.yml", "azure-pipelines.yml",
    ".ci.yaml", ".bazelci.yml", "cloudbuild.yaml", "cloudbuild.yml", "wercker.yml",
    ".semaphore.yml", "codemagic.yaml",
    # merge bots
    ".mergify.yml", "bors.toml", "rust-bors.toml", ".kodiak.toml", "triagebot.toml",
    ".prow.yaml", ".ci-operator.yaml",
    # coverage / quality services
    "codecov.yml", ".codecov.yml", "codecov.yaml", ".codecov.yaml", ".coveragerc",
    ".coveralls.yml", "sonar-project.properties", ".nycrc", ".nycrc.json", ".nycrc.yml",
    ".c8rc", ".c8rc.json", ".deepsource.toml", ".codeclimate.yml", ".qlty.toml",
    ".simplecov", "tarpaulin.toml", ".tarpaulin.toml",
    # build / task / test configs
    "package.json", "pnpm-workspace.yaml", "nx.json", "turbo.json", "lerna.json", "rush.json",
    "moon.yml", "Makefile", "makefile", "GNUmakefile", "justfile", "Justfile", ".justfile",
    "Taskfile.yml", "Taskfile.yaml", "pyproject.toml", "setup.cfg", "tox.ini", "noxfile.py",
    "pytest.ini", "Cargo.toml", "go.mod", ".golangci.yml", ".golangci.yaml", "build.gradle",
    "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "gradle.properties",
    "pom.xml", "Gemfile", "Rakefile", ".rspec", "phpunit.xml", "phpunit.xml.dist",
    "phpunit.dist.xml", "composer.json", "global.json", "Directory.Build.props",
    "Package.swift", "mix.exs", "MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel", ".bazelrc",
    ".bazelversion", "BUILD.bazel", "Jenkinsfile", "deno.json", "deno.jsonc", "bunfig.toml",
    "karma.conf.js", ".mocharc.json", ".mocharc.js", ".mocharc.yml", "Dockerfile",
    "meson.build", "CMakeLists.txt", "CMakePresets.json", "x.py", "build.sbt",
    "dart_test.yaml", "analysis_options.yaml",
}
ROOT_PREFIXES = (
    "jest.config.", "vitest.config.", "vitest.workspace.", "vite.config.", "playwright.config.",
    "azure-pipelines", "Jenkinsfile", "cypress.config.", "karma.conf.", "codecov.",
)
DIR_PREFIXES = (
    ".github/", ".circleci/", ".buildkite/", ".azure-pipelines/", ".gitlab/", ".cirrus/",
    ".jenkins/", ".teamcity/", ".woodpecker/", ".bazelci/", ".config/", ".cargo/", ".aviator/",
    ".moon/", ".devcontainer/", "eng/pipelines/", "build/azure-pipelines/", ".pipelines/",
    ".vsts-ci/", ".ci/", ".travis/", ".mvn/",
)
# A directory named like this at depth <= 3 counts as CI scripts: ci/, scripts/ci/, src/ci/ ...
CI_DIR_NAMES = {"ci", ".ci", "ci_scripts", "ci-scripts", "cicd", "buildkite", "pipelines"}
NESTED_NAMES = {"package.json", "codecov.yml", ".codecov.yml", "jest.config.js", "jest.config.ts",
                "vitest.config.ts", "vitest.config.mts", "vitest.config.js", "vitest.config.mjs",
                "project.json", "pyproject.toml", "nextest.toml", "phpunit.xml.dist",
                "phpunit.xml", "Cargo.toml", "karma.conf.js", "playwright.config.ts"}
SKIP_PARTS = {"node_modules", "fixtures", "fixture", "__fixtures__", "testdata", "test-data",
              "vendor", "third_party", "third-party", "thirdparty", "external", "examples",
              "example", "samples", "templates", "e2e-fixtures", "__tests__", "test", "tests",
              "playground", "benchmarks", "docs", "website", "sandbox", "sandboxes", "demo",
              "demos"}
MAX_CI_DIR_FILES = 600
MAX_NESTED = 400
TEXT_EXT = re.compile(r"\.(ya?ml|json|jsonc|toml|sh|bash|ps1|py|star|bzl|cfg|ini|groovy|"
                      r"kts|gradle|js|mjs|cjs|ts|mts|rb|txt|xml|props|psm1|cmd|bat|mk|"
                      r"just|nu|go|properties)$|(^|/)(Makefile|Dockerfile|Jenkinsfile)[^/]*$")


def escape(path: str) -> str:
    return "/" + re.sub(r"([\\*?\[\]!#])", r"\\\1", path)


def main() -> None:
    paths = [p.rstrip("\n") for p in open(sys.argv[1], encoding="utf-8", errors="replace")]
    picked: list[str] = []
    ci_dir: list[str] = []
    nested: list[str] = []
    for p in paths:
        if not p:
            continue
        parts = p.split("/")
        if len(parts) == 1:
            if p in ROOT_FILES or p.startswith(ROOT_PREFIXES):
                picked.append(p)
            continue
        if p.startswith(DIR_PREFIXES):
            if TEXT_EXT.search(p) or (p.startswith(".github/") and "/" not in p[len(".github/"):]
                                      and not re.search(r"\.(png|jpe?g|gif|svg|webp|excalidraw|ico|mp4)$", p, re.I)):
                picked.append(p)
            continue
        dirs = parts[:-1]
        if any(d in CI_DIR_NAMES for d in dirs[:3]) and not any(d in SKIP_PARTS for d in dirs):
            if TEXT_EXT.search(p):
                ci_dir.append(p)
            continue
        if parts[-1] in NESTED_NAMES and len(parts) <= 4 and not any(d in SKIP_PARTS for d in dirs):
            nested.append(p)
    # keep the shallowest CI-dir files first when capping
    ci_dir.sort(key=lambda s: (s.count("/"), s))
    nested.sort(key=lambda s: (s.count("/"), s))
    out = picked + ci_dir[:MAX_CI_DIR_FILES] + nested[:MAX_NESTED]
    for p in out:
        print(escape(p))


if __name__ == "__main__":
    main()

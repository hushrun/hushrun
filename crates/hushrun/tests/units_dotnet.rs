//! \[ADR 0118\] Test projects with `dotnet test` (C-30 to C-34, C-39, C-98 to C-101): a unit is a
//! test project for one target framework and configuration, a test one test method by its fully
//! qualified name, what a project read is what MSBuild says it and the projects it references
//! compile, and hushrun's VSTest target leaves out only what it was told, after checking it again.
//!
//! The fixtures are generated solutions of an xUnit, an NUnit and an MSTest project over one
//! library, for the .NET SDK `HUSHRUN_TEST_DOTNET` names (the `dotnet` program), whose major
//! version names their target framework. Without it they say so and pass. Their packages, at the
//! exact versions below, come from NuGet's global packages folder `HUSHRUN_TEST_DOTNET_PACKAGES`
//! (else one in the system's temporary directory), filled from nuget.org the first time.

#![cfg(unix)]

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "HUSHRUN_DOTNET",
    "HUSHRUN_DOTNET_VSTEST_TARGETS",
    "VSTestTargets",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// The `dotnet` to test with and the target framework its SDK builds, or none, said.
fn dotnet() -> Option<(PathBuf, String)> {
    let Some(path) = std::env::var_os("HUSHRUN_TEST_DOTNET").map(PathBuf::from) else {
        eprintln!("note: skipping: HUSHRUN_TEST_DOTNET names no dotnet");
        return None;
    };
    let out = Command::new(&path)
        .arg("--version")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .output()
        .expect("HUSHRUN_TEST_DOTNET starts");
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let major = version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .expect("dotnet --version says a version");
    Some((path, format!("net{major}.0")))
}

/// NuGet's global packages folder the fixtures restore into.
fn packages() -> PathBuf {
    std::env::var_os("HUSHRUN_TEST_DOTNET_PACKAGES")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hushrun-test-nuget"))
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn out(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// hushrun's own lines.
    fn lines(&self) -> Vec<&str> {
        self.stderr
            .lines()
            .chain(self.stdout.lines())
            .filter(|l| l.starts_with("hushrun: "))
            .collect()
    }

    /// The tests the console logger says passed or failed, by their names as it prints them.
    fn ran(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .stdout
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                let rest = l
                    .strip_prefix("Passed ")
                    .or_else(|| l.strip_prefix("Failed "))?;
                let name = rest.rsplit_once(" [").map_or(rest, |(n, _)| n);
                Some(name.to_string())
            })
            .collect();
        out.sort();
        out
    }

    /// How many tests VSTest ran of each assembly, by its file name, from its summary line
    /// (`Passed!  - Failed: 0, Passed: 3, Skipped: 0, Total: 3, Duration: 9 ms - A.dll (net8.0)`).
    fn totals(&self) -> std::collections::BTreeMap<String, u64> {
        let mut out = std::collections::BTreeMap::new();
        for line in self.stdout.lines() {
            let Some((counts, dll)) = line.rsplit_once(" - ") else {
                continue;
            };
            let Some(total) = counts.split("Total:").nth(1) else {
                continue;
            };
            let total: u64 = total
                .trim_start()
                .split(',')
                .next()
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(0);
            let dll = dll.split_whitespace().next().unwrap_or("").to_string();
            *out.entry(dll).or_insert(0) += total;
        }
        out
    }
}

struct Project {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
    home: PathBuf,
    path: std::ffi::OsString,
    framework: String,
}

impl Project {
    fn new(dotnet: &Path, framework: &str) -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut path = std::ffi::OsString::from(dotnet.parent().unwrap());
        path.push(":/usr/bin:/bin");
        let p = Project {
            repo,
            cache: root.join("cache"),
            home: root.join("home"),
            path,
            framework: framework.to_string(),
            _dir: dir,
        };
        for (file, text) in calc(framework) {
            p.write(&file, &text);
        }
        git::init(&p.repo, "sha1");
        git::commit_all(&p.repo, "calc");
        p
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// `dotnet <args>` in the repository, as COMMAND's would run.
    fn dotnet(&self, dotnet: &Path, args: &[&str]) -> Ran {
        let output = Command::new(dotnet)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("DOTNET_CLI_HOME", &self.home)
            .env("NUGET_PACKAGES", packages())
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_NOLOGO", "1")
            .env("MSBUILDDISABLENODEREUSE", "1")
            .env_remove("VSTestTargets")
            .output()
            .expect("dotnet");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn commit(&self, message: &str) {
        git::commit_all(&self.repo, message);
    }

    fn hushrun(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("PATH", &self.path)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.home.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.home)
            .env("DOTNET_CLI_HOME", &self.home)
            .env("NUGET_PACKAGES", packages())
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("MSBUILDDISABLENODEREUSE", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// `hushrun run unit -- dotnet test <args>`.
    fn test(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut all = vec!["run", "unit", "--", "dotnet", "test"];
        all.extend_from_slice(args);
        self.hushrun(&all, env)
    }

    fn unit(&self, project: &str) -> String {
        format!("{project} ({}, Debug)", self.framework)
    }
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/calc\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A solution of one library and three test projects over it, one per test framework, each at
/// the versions pinned here.
fn calc(framework: &str) -> Vec<(String, String)> {
    let test_project = |packages: &[(&str, &str)]| {
        let refs: String = packages
            .iter()
            .map(|(name, version)| {
                format!("    <PackageReference Include=\"{name}\" Version=\"{version}\" />\n")
            })
            .collect();
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n    <ImplicitUsings>enable</ImplicitUsings>\n    <IsPackable>false</IsPackable>\n    <IsTestProject>true</IsTestProject>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"Microsoft.NET.Test.Sdk\" Version=\"17.12.0\" />\n{refs}  </ItemGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"../../src/Calc/Calc.csproj\" />\n  </ItemGroup>\n</Project>\n"
        )
    };
    vec![
        (
            ".gitignore".into(),
            "bin/\nobj/\nTestResults/\n".into(),
        ),
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (
            "nuget.config".into(),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<configuration>\n  <packageSources>\n    <clear />\n    <add key=\"nuget.org\" value=\"https://api.nuget.org/v3/index.json\" />\n  </packageSources>\n</configuration>\n".into(),
        ),
        (
            "Calc.sln".into(),
            solution(&[
                ("Calc", "src/Calc/Calc.csproj"),
                ("Calc.XTests", "tests/Calc.XTests/Calc.XTests.csproj"),
                ("Calc.NTests", "tests/Calc.NTests/Calc.NTests.csproj"),
                ("Calc.MTests", "tests/Calc.MTests/Calc.MTests.csproj"),
            ]),
        ),
        (
            "src/Calc/Calc.csproj".into(),
            format!(
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n  </PropertyGroup>\n</Project>\n"
            ),
        ),
        (
            "src/Calc/Ops.cs".into(),
            "namespace Calc;\n\npublic static class Ops\n{\n    public static int Add(int a, int b) => a + b;\n    public static int Mul(int a, int b) => a * b;\n}\n".into(),
        ),
        (
            "tests/Calc.XTests/Calc.XTests.csproj".into(),
            test_project(&[("xunit", "2.9.2"), ("xunit.runner.visualstudio", "2.8.2")]),
        ),
        (
            "tests/Calc.XTests/AddTests.cs".into(),
            "using Xunit;\n\nnamespace Calc.XTests;\n\npublic class AddTests\n{\n    [Fact]\n    public void Adds() => Assert.Equal(3, Ops.Add(1, 2));\n\n    [Theory]\n    [InlineData(1)]\n    [InlineData(2)]\n    public void Zero(int a) => Assert.Equal(a, Ops.Add(a, 0));\n}\n".into(),
        ),
        (
            "tests/Calc.XTests/MulTests.cs".into(),
            "using Xunit;\n\nnamespace Calc.XTests;\n\npublic class MulTests\n{\n    [Fact]\n    public void Muls() => Assert.Null(Environment.GetEnvironmentVariable(\"CALC_FAIL\"));\n}\n".into(),
        ),
        (
            "tests/Calc.NTests/Calc.NTests.csproj".into(),
            test_project(&[("NUnit", "4.2.2"), ("NUnit3TestAdapter", "4.6.0")]),
        ),
        (
            "tests/Calc.NTests/AddTests.cs".into(),
            "using NUnit.Framework;\n\nnamespace Calc.NTests;\n\npublic class AddTests\n{\n    [Test]\n    public void Adds() => Assert.That(Ops.Add(1, 2), Is.EqualTo(3));\n\n    [TestCase(1)]\n    [TestCase(2)]\n    public void Zero(int a) => Assert.That(Ops.Add(a, 0), Is.EqualTo(a));\n}\n".into(),
        ),
        (
            "tests/Calc.MTests/Calc.MTests.csproj".into(),
            test_project(&[("MSTest.TestAdapter", "3.6.4"), ("MSTest.TestFramework", "3.6.4")]),
        ),
        (
            "tests/Calc.MTests/AddTests.cs".into(),
            "using Microsoft.VisualStudio.TestTools.UnitTesting;\n\nnamespace Calc.MTests;\n\n[TestClass]\npublic class AddTests\n{\n    [TestMethod]\n    public void Adds() => Assert.AreEqual(3, Ops.Add(1, 2));\n\n    [TestMethod]\n    [DataRow(1)]\n    [DataRow(2)]\n    public void Zero(int a) => Assert.AreEqual(a, Ops.Add(a, 0));\n}\n".into(),
        ),
    ]
}

/// A solution file naming `projects`, each `(name, path)`.
fn solution(projects: &[(&str, &str)]) -> String {
    let csharp = "FAE04EC0-301F-11D3-BF4B-00C04F79EFBC";
    let mut text = String::from(
        "\nMicrosoft Visual Studio Solution File, Format Version 12.00\n# Visual Studio Version 17\n",
    );
    let guid = |i: usize| format!("11111111-2222-3333-4444-{i:012}");
    for (i, (name, path)) in projects.iter().enumerate() {
        text.push_str(&format!(
            "Project(\"{{{csharp}}}\") = \"{name}\", \"{}\", \"{{{}}}\"\nEndProject\n",
            path.replace('/', "\\"),
            guid(i)
        ));
    }
    text.push_str("Global\n\tGlobalSection(SolutionConfigurationPlatforms) = preSolution\n\t\tDebug|Any CPU = Debug|Any CPU\n\tEndGlobalSection\n\tGlobalSection(ProjectConfigurationPlatforms) = postSolution\n");
    for i in 0..projects.len() {
        let g = guid(i);
        text.push_str(&format!(
            "\t\t{{{g}}}.Debug|Any CPU.ActiveCfg = Debug|Any CPU\n\t\t{{{g}}}.Debug|Any CPU.Build.0 = Debug|Any CPU\n"
        ));
    }
    text.push_str("\tEndGlobalSection\nEndGlobal\n");
    text
}

const X: &str = "tests/Calc.XTests/Calc.XTests.csproj";
const N: &str = "tests/Calc.NTests/Calc.NTests.csproj";
const M: &str = "tests/Calc.MTests/Calc.MTests.csproj";

// covers: C-98, C-99, C-33, C-34
#[test]
fn a_test_project_reruns_when_a_file_it_compiles_changes_and_only_then() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    let first = p.test(&[], &[]);
    assert_eq!(first.code, 0, "{}", first.out());
    assert!(
        first
            .lines()
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        first.lines()
    );
    // Each project's reads are what it and the library compile (the project files are in the rest
    // set, which no key lists).
    let map = store::map(&p.cache);
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} in {map}"))
            .iter()
            .map(|r| r.as_str().unwrap().to_string())
            .collect()
    };
    let x = reads(&p.unit(X));
    for read in [
        "src/Calc/Ops.cs",
        "tests/Calc.XTests/AddTests.cs",
        "tests/Calc.XTests/MulTests.cs",
    ] {
        assert!(x.contains(&read.to_string()), "{read} in {x:?}");
    }
    assert!(!x.iter().any(|r| r.contains("NTests")), "{x:?}");
    for unit in [N, M] {
        assert!(
            reads(&p.unit(unit)).contains(&"src/Calc/Ops.cs".to_string()),
            "{unit}"
        );
    }

    // One test project's file: that project runs, the others are left out whole, unbuilt.
    p.write(
        "tests/Calc.NTests/AddTests.cs",
        "using NUnit.Framework;\n\nnamespace Calc.NTests;\n\npublic class AddTests\n{\n    [Test]\n    public void Adds() => Assert.That(Ops.Add(2, 2), Is.EqualTo(4));\n\n    [TestCase(1)]\n    [TestCase(2)]\n    public void Zero(int a) => Assert.That(Ops.Add(a, 0), Is.EqualTo(a));\n}\n",
    );
    p.commit("n");
    let second = p.test(&[], &[]);
    assert_eq!(second.code, 0, "{}", second.out());
    // NUnit's project ran whole; of the others, at most the one test the audit draws (a theory's
    // two rows).
    let totals = second.totals();
    assert_eq!(totals.get("Calc.NTests.dll"), Some(&3), "{}", second.out());
    let others =
        totals.get("Calc.XTests.dll").unwrap_or(&0) + totals.get("Calc.MTests.dll").unwrap_or(&0);
    assert!(others <= 2, "{totals:?}\n{}", second.out());

    // The library: every project that compiles it runs.
    p.write(
        "src/Calc/Ops.cs",
        "namespace Calc;\n\npublic static class Ops\n{\n    public static int Add(int a, int b) => b + a;\n    public static int Mul(int a, int b) => a * b;\n}\n",
    );
    p.commit("ops");
    let third = p.test(&[], &[]);
    assert_eq!(third.code, 0, "{}", third.out());
    for dll in ["Calc.XTests.dll", "Calc.NTests.dll", "Calc.MTests.dll"] {
        assert!(third.stdout.contains(dll), "{dll}: {}", third.out());
    }
}

// covers: C-100, C-39
#[test]
fn a_test_that_failed_runs_alone_and_the_ones_that_passed_are_left_out_by_name() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    const NAMED: &[&str] = &["--logger", "console;verbosity=normal"];
    let failed = p.test(NAMED, &[("CALC_FAIL", "1")]);
    assert_ne!(failed.code, 0, "{}", failed.out());
    assert!(
        failed
            .ran()
            .contains(&"Calc.XTests.MulTests.Muls".to_string()),
        "{:?}",
        failed.ran()
    );
    // The same code: the xUnit project runs its test that failed, and none that passed; the
    // other projects are left out whole.
    let again = p.test(NAMED, &[]);
    assert_eq!(again.code, 0, "{}", again.out());
    assert!(
        again
            .ran()
            .contains(&"Calc.XTests.MulTests.Muls".to_string()),
        "{}",
        again.out()
    );
    // Eight tests by id (a theory's and a data-driven test's rows are one): of the seven that
    // passed, only the one the audit draws runs.
    let line = again
        .lines()
        .into_iter()
        .find(|l| l.contains(" tests run, in "))
        .unwrap_or_else(|| panic!("{}", again.out()));
    assert_eq!(
        hushrun_testkit::lines::drawn(line),
        hushrun_testkit::lines::drawn(
            "hushrun: unit: 2 of 8 tests run, in 2 of 3 test projects; 6 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    assert!(
        again
            .lines()
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        again.lines()
    );
}

// covers: C-100
#[test]
fn commands_own_filter_and_trx_logger_are_kept() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    let ran = p.test(
        &[
            "--logger",
            "trx",
            "--logger",
            "console;verbosity=normal",
            "--filter",
            "FullyQualifiedName~Calc.XTests",
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.out());
    // The filter held: only the xUnit project's tests ran.
    assert!(
        ran.ran().iter().all(|t| t.starts_with("Calc.XTests.")),
        "{:?}",
        ran.ran()
    );
    // COMMAND's TRX file is where VSTest puts it, and hushrun read it: the run was recorded.
    let results = p.repo.join("tests/Calc.XTests/TestResults");
    let trx = std::fs::read_dir(&results)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "trx"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(trx, 1, "{}: {}", results.display(), ran.out());
    assert!(
        ran.lines()
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        ran.lines()
    );
    let map = store::map(&p.cache);
    assert!(map["units"].get(p.unit(X)).is_some(), "{map}");
}

// covers: C-99, C-32
#[test]
fn an_assembly_older_than_what_it_compiles_records_no_pass() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    let first = p.test(&[], &[]);
    assert_eq!(first.code, 0, "{}", first.out());
    p.write(
        "src/Calc/Ops.cs",
        "namespace Calc;\n\npublic static class Ops\n{\n    public static int Add(int a, int b) => a + b + 0;\n    public static int Mul(int a, int b) => a * b;\n}\n",
    );
    p.commit("ops");
    let stale = p.test(&["--no-build"], &[]);
    assert_eq!(stale.code, 0, "{}", stale.out());
    assert!(
        stale
            .lines()
            .iter()
            .any(|l| l.contains("ran an assembly older than src/Calc/Ops.cs")),
        "{:?}",
        stale.lines()
    );
}

// covers: C-101, C-98
#[test]
fn what_runs_without_hushruns_vstest_target_says_why() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    let built = p.dotnet(&dotnet, &["build"]);
    assert_eq!(built.code, 0, "{}", built.out());
    let dll = format!("tests/Calc.XTests/bin/Debug/{framework}/Calc.XTests.dll");
    let ran = p.hushrun(&["run", "unit", "--", "dotnet", "test", &dll], &[]);
    assert_eq!(ran.code, 0, "{}", ran.out());
    assert!(
        ran.lines()
            .iter()
            .any(|l| l.contains("test files aren't observed")
                && l.contains("runs an assembly with VSTest and no MSBuild")),
        "{:?}",
        ran.lines()
    );
}

// covers: C-101, C-30
#[test]
fn a_project_on_the_testing_platform_runs_alone_unobserved_and_the_run_says_so() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let p = Project::new(&dotnet, &framework);
    p.write(
        "tests/Calc.PTests/Calc.PTests.csproj",
        &format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n    <ImplicitUsings>enable</ImplicitUsings>\n    <OutputType>Exe</OutputType>\n    <IsTestProject>true</IsTestProject>\n    <EnableMSTestRunner>true</EnableMSTestRunner>\n    <TestingPlatformDotnetTestSupport>true</TestingPlatformDotnetTestSupport>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"MSTest\" Version=\"3.6.4\" />\n  </ItemGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"../../src/Calc/Calc.csproj\" />\n  </ItemGroup>\n</Project>\n"
        ),
    );
    p.write(
        "tests/Calc.PTests/AddTests.cs",
        "using Microsoft.VisualStudio.TestTools.UnitTesting;\n\nnamespace Calc.PTests;\n\n[TestClass]\npublic class AddTests\n{\n    [TestMethod]\n    public void Adds() => Assert.AreEqual(3, Ops.Add(1, 2));\n}\n",
    );
    p.write(
        "Calc.sln",
        &solution(&[
            ("Calc", "src/Calc/Calc.csproj"),
            ("Calc.XTests", "tests/Calc.XTests/Calc.XTests.csproj"),
            ("Calc.PTests", "tests/Calc.PTests/Calc.PTests.csproj"),
        ]),
    );
    p.commit("platform");
    let note = "hushrun: unit: tests/Calc.PTests/Calc.PTests.csproj runs on Microsoft.Testing.Platform, whose results hushrun doesn't read yet: its tests aren't observed";
    let first = p.test(&[], &[]);
    assert_eq!(first.code, 0, "{}", first.out());
    assert!(first.lines().contains(&note), "{:?}", first.lines());
    // A change to its own file runs it, and not the xUnit project: its files are its own.
    p.write(
        "tests/Calc.PTests/AddTests.cs",
        "using Microsoft.VisualStudio.TestTools.UnitTesting;\n\nnamespace Calc.PTests;\n\n[TestClass]\npublic class AddTests\n{\n    [TestMethod]\n    public void Adds() => Assert.AreEqual(4, Ops.Add(2, 2));\n}\n",
    );
    p.commit("p");
    let second = p.test(&[], &[]);
    assert_eq!(second.code, 0, "{}", second.out());
    assert!(second.lines().contains(&note), "{:?}", second.lines());
    let x = second.totals().get("Calc.XTests.dll").copied().unwrap_or(0);
    assert!(x <= 2, "the xUnit project ran: {}", second.out());
}

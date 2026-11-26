//! `dotnet test`'s fixtures, which `units_dotnet.rs` (in-process) and `e2e_dotnet.rs` share: a
//! generated solution of an xUnit, an NUnit and an MSTest project over one library, for the .NET
//! SDK `HUSHRUN_TEST_DOTNET` names (the `dotnet` program), whose major version names their target
//! framework, and what VSTest's console logger printed of a run.
//!
//! Without `HUSHRUN_TEST_DOTNET` the tests say so and pass. Their packages, at the exact versions
//! below, come from NuGet's global packages folder `HUSHRUN_TEST_DOTNET_PACKAGES` (else one in the
//! system's temporary directory), filled from nuget.org the first time.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::repo::Repo;

/// Environment variables COMMAND must never inherit besides [`super::repo::CLEARED`]: hushrun's
/// own for its VSTest target and programs, the SDK's `VSTestTargets`, and a workspace wrapper.
pub const CLEARED: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_DOTNET",
    "HUSHRUN_DOTNET_VSTEST_TARGETS",
    "VSTestTargets",
    "RUSTC_WORKSPACE_WRAPPER",
];

/// The `dotnet` to test with and the target framework its SDK builds, or none, said.
pub fn dotnet() -> Option<(PathBuf, String)> {
    let Some(path) = std::env::var_os("HUSHRUN_TEST_DOTNET").map(PathBuf::from) else {
        eprintln!("note: skipping: HUSHRUN_TEST_DOTNET names no dotnet");
        return None;
    };
    let out = hushrun_testkit::tool::command(&path)
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
pub fn packages() -> PathBuf {
    std::env::var_os("HUSHRUN_TEST_DOTNET_PACKAGES")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hushrun-test-nuget"))
}

/// The solution for `framework`, committed, whose COMMAND finds `dotnet` on its `PATH` and gets
/// the SDK's settings for a machine of its own.
pub fn project(dotnet: &Path, framework: &str) -> Repo {
    let r = Repo::new(&calc(framework)).clear(CLEARED);
    let home = r.root.join("home");
    let mut path = std::ffi::OsString::from(dotnet.parent().unwrap());
    path.push(":/usr/bin:/bin");
    r.give("PATH", path)
        .give("DOTNET_CLI_HOME", home)
        .give("NUGET_PACKAGES", packages())
        .give("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .give("DOTNET_NOLOGO", "1")
        .give("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        .give("MSBUILDDISABLENODEREUSE", "1")
}

/// `dotnet <args>` in the repository, outside hushrun, as COMMAND's would run: its exit code and
/// output.
pub fn run_dotnet(r: &Repo, dotnet: &Path, args: &[&str]) -> (i32, String) {
    let home = r.root.join("home");
    let output = hushrun_testkit::tool::command(dotnet)
        .args(args)
        .current_dir(&r.repo)
        .env("HOME", &home)
        .env("DOTNET_CLI_HOME", &home)
        .env("NUGET_PACKAGES", packages())
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("MSBUILDDISABLENODEREUSE", "1")
        .env_remove("VSTestTargets")
        .output()
        .expect("dotnet");
    (
        output.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// A unit's name: `project`, for `framework`, in Debug.
pub fn unit(project: &str, framework: &str) -> String {
    format!("{project} ({framework}, Debug)")
}

/// The tests the console logger says passed or failed (`stdout`), by their names as it prints
/// them.
pub fn ran(stdout: &str) -> Vec<String> {
    // Two test projects run at once: one's `  Passed <name>` can land at the end of the
    // other's line (`Results File: <path>  Passed <name> [6 ms]`), so each line is read from
    // VSTest's two-space indent too.
    let mut out: Vec<String> = stdout
        .lines()
        .flat_map(|l| l.split("  "))
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l
                .strip_prefix("Passed ")
                .or_else(|| l.strip_prefix("Failed "))?;
            let name = rest.rsplit_once(" [").map_or(rest, |(n, _)| n).trim();
            // Under load VSTest's console lines can come out cut: a name it didn't print is
            // no test's.
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect();
    out.sort();
    out
}

/// How many tests VSTest ran of each assembly, by its file name, from its summaries in `stdout`
/// (`Passed!  - Failed: 0, Passed: 3, Skipped: 0, Total: 3, Duration: 9 ms - A.dll (net8.0)`).
///
/// Under load two projects' summaries can land on one line (`… Duration: 84 msPassed!  - …`),
/// so each summary is read from its own `Total:` to the next one, wherever the lines break: a
/// total counts for the assembly its own summary names, and one whose summary names none (cut)
/// counts for none, never for the next summary's.
pub fn totals(stdout: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for summary in stdout.split("Total:").skip(1) {
        let Some(total) = summary
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|t| t.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(dll) = summary.split(" - ").skip(1).find_map(|rest| {
            let name = rest.split_whitespace().next()?;
            name.ends_with(".dll").then(|| name.to_string())
        }) else {
            continue;
        };
        *out.entry(dll).or_insert(0) += total;
    }
    out
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/calc\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A solution of one library and three test projects over it, one per test framework, each at
/// the versions pinned here.
pub fn calc(framework: &str) -> Vec<(String, String)> {
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
pub fn solution(projects: &[(&str, &str)]) -> String {
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

pub const X: &str = "tests/Calc.XTests/Calc.XTests.csproj";
pub const N: &str = "tests/Calc.NTests/Calc.NTests.csproj";
pub const M: &str = "tests/Calc.MTests/Calc.MTests.csproj";

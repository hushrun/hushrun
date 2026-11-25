//! \[ADR 0118\] Test projects with `dotnet test` (C-30 to C-34, C-39, C-98 to C-101), end to end:
//! a unit is a test project for one target framework and configuration, a test one test method by
//! its fully qualified name, what a project read is what MSBuild says it and the projects it
//! references compile, and hushrun's VSTest target leaves out only what it was told, after
//! checking it again.
//!
//! These run the `hushrun` binary (`common/cli.rs`): hushrun's VSTest target starts the binary
//! itself (MSBuild's `Exec` of `HUSHRUN_DOTNET`), so a flow that observes a test project reads all
//! of hushrun (CONTRIBUTING.md, "Which tests CI runs again"). What needs no VSTest target, a
//! `dotnet test` hushrun doesn't observe and what VSTest printed, is in `units_dotnet.rs`,
//! in-process.
//!
//! The fixtures are `common/dotnet.rs`'s, for the .NET SDK `HUSHRUN_TEST_DOTNET` names. Without it
//! they say so and pass.

#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/dotnet.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use hushrun_testkit::git;

use cli::{Cli as _, Output};
use fixtures::{M, N, X, dotnet, project, ran, solution, totals, unit};
use repo::Repo;

/// Its output, COMMAND's and hushrun's.
fn out(ran: &Output) -> String {
    format!("{}{}", ran.stdout, ran.stderr)
}

/// hushrun's own lines.
fn lines(ran: &Output) -> Vec<&str> {
    ran.stderr
        .lines()
        .chain(ran.stdout.lines())
        .filter(|l| l.starts_with("hushrun: "))
        .collect()
}

/// `hushrun run unit -- dotnet test <args>`, with `env` besides.
fn test(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut all = vec!["run", "unit", "--", "dotnet", "test"];
    all.extend_from_slice(args);
    r.run(&all, env)
}

// covers: C-98, C-99, C-33, C-34
#[test]
fn a_test_project_reruns_when_a_file_it_compiles_changes_and_only_then() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    let first = test(&r, &[], &[]);
    assert_eq!(first.code, 0, "{}", out(&first));
    assert!(
        lines(&first)
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        lines(&first)
    );
    // Each project's reads are what it and the library compile (the project files are in the rest
    // set, which no key lists).
    let map = r.map();
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} in {map}"))
            .iter()
            .map(|r| r.as_str().unwrap().to_string())
            .collect()
    };
    let x = reads(&unit(X, &framework));
    for read in [
        "src/Calc/Ops.cs",
        "tests/Calc.XTests/AddTests.cs",
        "tests/Calc.XTests/MulTests.cs",
    ] {
        assert!(x.contains(&read.to_string()), "{read} in {x:?}");
    }
    assert!(!x.iter().any(|r| r.contains("NTests")), "{x:?}");
    for project in [N, M] {
        assert!(
            reads(&unit(project, &framework)).contains(&"src/Calc/Ops.cs".to_string()),
            "{project}"
        );
    }

    // One test project's file: that project runs, the others are left out whole, unbuilt.
    r.write(
        "tests/Calc.NTests/AddTests.cs",
        "using NUnit.Framework;\n\nnamespace Calc.NTests;\n\npublic class AddTests\n{\n    [Test]\n    public void Adds() => Assert.That(Ops.Add(2, 2), Is.EqualTo(4));\n\n    [TestCase(1)]\n    [TestCase(2)]\n    public void Zero(int a) => Assert.That(Ops.Add(a, 0), Is.EqualTo(a));\n}\n",
    );
    git::commit_all(&r.repo, "n");
    let second = test(&r, &[], &[]);
    assert_eq!(second.code, 0, "{}", out(&second));
    // NUnit's project ran whole; of the others, at most the one test the audit draws (a theory's
    // two rows).
    let totals = totals(&second.stdout);
    assert_eq!(totals.get("Calc.NTests.dll"), Some(&3), "{}", out(&second));
    let others =
        totals.get("Calc.XTests.dll").unwrap_or(&0) + totals.get("Calc.MTests.dll").unwrap_or(&0);
    assert!(others <= 2, "{totals:?}\n{}", out(&second));

    // The library: every project that compiles it runs.
    r.write(
        "src/Calc/Ops.cs",
        "namespace Calc;\n\npublic static class Ops\n{\n    public static int Add(int a, int b) => b + a;\n    public static int Mul(int a, int b) => a * b;\n}\n",
    );
    git::commit_all(&r.repo, "ops");
    let third = test(&r, &[], &[]);
    assert_eq!(third.code, 0, "{}", out(&third));
    for dll in ["Calc.XTests.dll", "Calc.NTests.dll", "Calc.MTests.dll"] {
        assert!(third.stdout.contains(dll), "{dll}: {}", out(&third));
    }
}

// covers: C-100, C-39
#[test]
fn a_test_that_failed_runs_alone_and_the_ones_that_passed_are_left_out_by_name() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    const NAMED: &[&str] = &["--logger", "console;verbosity=normal"];
    let failed = test(&r, NAMED, &[("CALC_FAIL", "1")]);
    assert_ne!(failed.code, 0, "{}", out(&failed));
    assert!(
        ran(&failed.stdout).contains(&"Calc.XTests.MulTests.Muls".to_string()),
        "{:?}",
        ran(&failed.stdout)
    );
    // The same code: the xUnit project runs its test that failed, and none that passed; the
    // other projects are left out whole.
    let again = test(&r, NAMED, &[]);
    assert_eq!(again.code, 0, "{}", out(&again));
    assert!(
        ran(&again.stdout).contains(&"Calc.XTests.MulTests.Muls".to_string()),
        "{}",
        out(&again)
    );
    // Eight tests by id (a theory's and a data-driven test's rows are one): of the seven that
    // passed, only the one the audit draws runs.
    let line = lines(&again)
        .into_iter()
        .find(|l| l.contains(" tests run, in "))
        .unwrap_or_else(|| panic!("{}", out(&again)));
    assert_eq!(
        hushrun_testkit::lines::drawn(line),
        hushrun_testkit::lines::drawn(
            "hushrun: unit: 2 of 8 tests run, in 2 of 3 test projects; 6 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    assert!(
        lines(&again)
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        lines(&again)
    );
}

// covers: C-100
#[test]
fn commands_own_filter_and_trx_logger_are_kept() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    let ran = test(
        &r,
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
    assert_eq!(ran.code, 0, "{}", out(&ran));
    // The filter held: only the xUnit project's tests ran.
    assert!(
        fixtures::ran(&ran.stdout)
            .iter()
            .all(|t| t.starts_with("Calc.XTests.")),
        "{:?}",
        fixtures::ran(&ran.stdout)
    );
    // COMMAND's TRX file is where VSTest puts it, and hushrun read it: the run was recorded.
    let results = r.repo.join("tests/Calc.XTests/TestResults");
    let trx = std::fs::read_dir(&results)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "trx"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(trx, 1, "{}: {}", results.display(), out(&ran));
    assert!(
        lines(&ran)
            .iter()
            .any(|l| l.contains("it won't run again on this code")),
        "{:?}",
        lines(&ran)
    );
    let map = r.map();
    assert!(map["units"].get(unit(X, &framework)).is_some(), "{map}");
}

// covers: C-99, C-32
#[test]
fn an_assembly_older_than_what_it_compiles_records_no_pass() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    let first = test(&r, &[], &[]);
    assert_eq!(first.code, 0, "{}", out(&first));
    r.write(
        "src/Calc/Ops.cs",
        "namespace Calc;\n\npublic static class Ops\n{\n    public static int Add(int a, int b) => a + b + 0;\n    public static int Mul(int a, int b) => a * b;\n}\n",
    );
    git::commit_all(&r.repo, "ops");
    let stale = test(&r, &["--no-build"], &[]);
    assert_eq!(stale.code, 0, "{}", out(&stale));
    assert!(
        lines(&stale)
            .iter()
            .any(|l| l.contains("ran an assembly older than src/Calc/Ops.cs")),
        "{:?}",
        lines(&stale)
    );
}

// covers: C-101, C-30
#[test]
fn a_project_on_the_testing_platform_runs_alone_unobserved_and_the_run_says_so() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    r.write(
        "tests/Calc.PTests/Calc.PTests.csproj",
        &format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n    <ImplicitUsings>enable</ImplicitUsings>\n    <OutputType>Exe</OutputType>\n    <IsTestProject>true</IsTestProject>\n    <EnableMSTestRunner>true</EnableMSTestRunner>\n    <TestingPlatformDotnetTestSupport>true</TestingPlatformDotnetTestSupport>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"MSTest\" Version=\"3.6.4\" />\n  </ItemGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"../../src/Calc/Calc.csproj\" />\n  </ItemGroup>\n</Project>\n"
        ),
    );
    r.write(
        "tests/Calc.PTests/AddTests.cs",
        "using Microsoft.VisualStudio.TestTools.UnitTesting;\n\nnamespace Calc.PTests;\n\n[TestClass]\npublic class AddTests\n{\n    [TestMethod]\n    public void Adds() => Assert.AreEqual(3, Ops.Add(1, 2));\n}\n",
    );
    r.write(
        "Calc.sln",
        &solution(&[
            ("Calc", "src/Calc/Calc.csproj"),
            ("Calc.XTests", "tests/Calc.XTests/Calc.XTests.csproj"),
            ("Calc.PTests", "tests/Calc.PTests/Calc.PTests.csproj"),
        ]),
    );
    git::commit_all(&r.repo, "platform");
    let note = "hushrun: unit: tests/Calc.PTests/Calc.PTests.csproj runs on Microsoft.Testing.Platform, whose results hushrun doesn't read yet: its tests aren't observed";
    let first = test(&r, &[], &[]);
    assert_eq!(first.code, 0, "{}", out(&first));
    assert!(lines(&first).contains(&note), "{:?}", lines(&first));
    // A change to its own file runs it, and not the xUnit project: its files are its own.
    r.write(
        "tests/Calc.PTests/AddTests.cs",
        "using Microsoft.VisualStudio.TestTools.UnitTesting;\n\nnamespace Calc.PTests;\n\n[TestClass]\npublic class AddTests\n{\n    [TestMethod]\n    public void Adds() => Assert.AreEqual(4, Ops.Add(2, 2));\n}\n",
    );
    git::commit_all(&r.repo, "p");
    let second = test(&r, &[], &[]);
    assert_eq!(second.code, 0, "{}", out(&second));
    assert!(lines(&second).contains(&note), "{:?}", lines(&second));
    let x = totals(&second.stdout)
        .get("Calc.XTests.dll")
        .copied()
        .unwrap_or(0);
    assert!(x <= 2, "the xUnit project ran: {}", out(&second));
}

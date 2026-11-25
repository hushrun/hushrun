//! \[ADR 0118\] `dotnet test` (C-98, C-101), in-process: a `dotnet test` that runs no MSBuild isn't
//! observed and says why, and what VSTest's console logger prints is read as the end-to-end tests
//! read it.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): a test runs a check as `hushrun run
//! unit -- dotnet test …` would, through the `dotnet test` adapter's own code, and the code every
//! adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`). The .NET SDK
//! runs as COMMAND, for real. A run hushrun observes starts hushrun's VSTest target, which runs the
//! binary itself, so those flows are end to end: `e2e_dotnet.rs`.
//!
//! The fixtures are `common/dotnet.rs`'s, for the .NET SDK `HUSHRUN_TEST_DOTNET` names. Without it
//! they say so and pass.

#![cfg(unix)]

#[path = "common/dotnet.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeMap;

use hushrun::units::dotnet;

use fixtures::{project, run_dotnet, totals};
use in_process::{Ran, Repo};

/// `hushrun run unit -- ARGS`, in-process, through the `dotnet test` adapter alone, for a COMMAND
/// it refuses: it panics when the adapter would load, which starts hushrun's VSTest target
/// (`e2e_dotnet.rs`).
fn run(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the adapter (`dotnet::recognized`), with no trait
    // object: what COMMAND is, and why hushrun can't observe it.
    match dotnet::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        Some(Ok(())) => panic!("{args:?} loads the adapter, whose run starts hushrun itself"),
        None => panic!("{args:?} isn't dotnet test"),
    }
}

#[test]
fn vstest_summaries_are_counted_by_their_own_assembly_even_when_they_share_a_line() {
    let stdout = [
        "Passed!  - Failed:     0, Passed:     1, Skipped:     0, Total:     1, Duration: 12 ms - Calc.XTests.dll (net11.0)",
        // Two summaries joined: the first, cut, names no assembly and counts for none; the
        // second counts for its own.
        "Passed!  - Failed:     0, Passed:     1, Skipped:     0, Total:     1, Duration: 84 msPassed!  - Failed:     0, Passed:     3, Skipped:     0, Total:     3, Duration: 90 ms - Calc.NTests.dll (net11.0)",
        // A cut summary names no assembly: it counts for none, not the next one's.
        "Passed!  - Failed:     0, Passed:     5, Skipped:     0, Total:     5, Dur",
        "  Calc.MTests -> /tmp/x/Calc.MTests.dll",
        "Passed!  - Failed:     0, Passed:     2, Skipped:     0, Total:     2, Duration: 7 ms - Calc.MTests.dll (net11.0)",
    ]
    .join("\n");
    let totals = totals(&stdout);
    assert_eq!(
        totals,
        BTreeMap::from([
            ("Calc.MTests.dll".to_string(), 2),
            ("Calc.NTests.dll".to_string(), 3),
            ("Calc.XTests.dll".to_string(), 1),
        ])
    );
}

// covers: C-101, C-98
#[test]
fn what_runs_without_hushruns_vstest_target_says_why() {
    let Some((dotnet, framework)) = fixtures::dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework);
    let (code, built) = run_dotnet(&r, &dotnet, &["build"]);
    assert_eq!(code, 0, "{built}");
    let dll = format!("tests/Calc.XTests/bin/Debug/{framework}/Calc.XTests.dll");
    let ran = run(&r, &["dotnet", "test", &dll]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.lines
            .iter()
            .any(|l| l.contains("test files aren't observed")
                && l.contains("runs an assembly with VSTest and no MSBuild")),
        "{:?}",
        ran.lines
    );
}

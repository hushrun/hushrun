//! ADR 0030: the names before ADR 0029 (`--ledger`, `HUSHRUN_LEDGER*`) and before ADR 0030
//! (`HUSHRUN_REPOSITORY`) were never released, so hushrun keeps no aliases for them: the old
//! flags are unknown arguments, and the old variables are ignored like any other. Nor does
//! `hushrun ledger`, now `hushrun repository`.

use hushrun_testkit::fake_ledger::FakeLedger;
use hushrun_testkit::world::World;

const MEMBER: u8 = 1; // a member's machine

fn run(cmd: &mut assert_cmd::Command) -> (i32, String) {
    let output = cmd.output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A local plan with no URL from the environment: hushrun.yaml names none either.
fn plan(w: &World) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(MEMBER);
    cmd.env_remove("HUSHRUN_REPOSITORY_URL")
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .args(["plan", "--format", "json"]);
    cmd
}

fn world_without_url() -> World {
    let w = World::new("acme-api.yaml");
    let text = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let text: String = text
        .lines()
        .filter(|l| !l.starts_with("repository_url:"))
        .map(|l| format!("{l}\n"))
        .collect();
    w.write("hushrun.yaml", &text);
    w.commit("no url");
    w
}

#[test]
fn the_old_variables_are_ignored() {
    let w = world_without_url();
    let (code, stderr) = run(plan(&w)
        .env("HUSHRUN_REPOSITORY", w.ledger_url())
        .env("HUSHRUN_LEDGER", w.ledger_url())
        .env("HUSHRUN_REPOSITORY_TOKEN", FakeLedger::ADMIN));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("no repository URL"), "{stderr}");
    assert!(stderr.contains("HUSHRUN_REPOSITORY_URL"), "{stderr}");
    assert!(!stderr.contains("HUSHRUN_LEDGER"), "{stderr}");

    // The new name works.
    let (code, stderr) = run(plan(&w)
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
        .env("HUSHRUN_REPOSITORY_TOKEN", FakeLedger::ADMIN));
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
}

#[test]
fn the_old_flags_are_unknown_arguments() {
    let w = world_without_url();
    for args in [
        &["--ledger", "https://hushrun.example/l/acme/x", "plan"][..],
        &["adopt", "--ledger-base", "https://hushrun.example/l/acme"][..],
    ] {
        let (code, stderr) = run(w.hushrun().args(args));
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stderr.contains("unexpected argument"), "{args:?}: {stderr}");
    }
}

/// `hushrun ledger …` is `hushrun repository …`, with no alias: the old name is an unknown
/// subcommand.
#[test]
fn the_old_subcommand_is_unknown() {
    let w = world_without_url();
    for sub in ["ls", "show", "signers", "gc", "monitor"] {
        let (code, stderr) = run(w.hushrun().args(["ledger", sub]));
        assert_eq!(code, 2, "{sub}: {stderr}");
        assert!(
            stderr.contains("unrecognized subcommand"),
            "{sub}: {stderr}"
        );
    }
    let output = w.hushrun().args(["repository", "--help"]).output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for sub in ["ls", "show", "signers", "gc", "monitor"] {
        assert!(help.contains(sub), "{help}");
    }
}

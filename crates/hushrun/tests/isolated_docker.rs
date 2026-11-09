//! `run --isolated` with a real container runtime (M8, E-12), behind the `it` feature: no
//! network by default, exactly the digested files, only the check's variables, and an export
//! that is gone afterwards even when the container wrote to it as root.
//!
//! ```sh
//! cargo test -p hushrun --features it --test isolated_docker
//! ```
//!
//! It needs Docker (or Podman, through `HUSHRUN_CONTAINER_RUNTIME`) and pulls Alpine once.
#![cfg(all(feature = "it", unix))]

use hushrun_testkit::world::World;
use serde_json::Value;

/// Alpine 3.20, pinned by its index digest: every platform.
const ALPINE: &str =
    "alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc";

/// Held by each test: they share the runtime, and one checks that no isolated run's container is
/// left, which another test's running check would be.
static RUNTIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn world(checks: &str) -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/shop
{}
scopes:
  api:
    include: [\"services/api/**\", \"tools/**\"]
checks:
{checks}
deploy:
  branch: main
  require: [offline]
",
            w.ledger_lines()
        ),
    );
    w.write("tools/build.sh", "#!/bin/sh\necho built\n");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            w.repo.join("tools/build.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    w.commit("checks");
    w
}

fn check(name: &str, script: &str, more: &str) -> String {
    format!(
        "  {name}:\n    image: \"{ALPINE}\"\n    run: {}\n{more}",
        serde_json::to_string(script).unwrap()
    )
}

fn run(w: &World, names: &[&str], env: &[(&str, &str)]) -> (i32, Value, String) {
    let mut cmd = w.hushrun_as(3);
    cmd.env("HUSHRUN_EXPORT_DIR", w.root.join("exports"));
    if let Ok(runtime) = std::env::var("HUSHRUN_CONTAINER_RUNTIME") {
        cmd.env("HUSHRUN_CONTAINER_RUNTIME", runtime);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .args(["run", "--isolated", "--format", "json"])
        .args(names)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// covers: E-12
#[test]
fn isolated_checks_have_no_network_and_see_only_the_digested_files() {
    let _runtime = one_at_a_time();
    let w = world(&format!(
        "{}{}{}",
        // No route out: connecting anywhere fails at once, whatever devices the kernel shows.
        check(
            "offline",
            "test -z \"$(ip route)\" && ! wget -q -T 3 -O /dev/null http://1.1.1.1",
            "",
        ),
        check(
            "files",
            "test -x tools/build.sh && test -e services/api/main.go && test ! -e main.go && test ! -e web && test \"$(pwd)\" = /src",
            "",
        ),
        check(
            "variables",
            "test \"$GO_VERSION\" = 1.23 && test -z \"$UNRELATED\" && test -z \"$HUSHRUN_SIGNING_KEY\" && touch left-by-root",
            "    env: [GO_VERSION]\n",
        ),
    ));
    let (code, out, stderr) = run(
        &w,
        &["offline", "files", "variables"],
        &[("GO_VERSION", "1.23"), ("UNRELATED", "x")],
    );
    assert_eq!(code, 0, "{out}\n{stderr}");
    for check in out["checks"].as_array().unwrap() {
        assert_eq!(check["result"], "pass", "{check}: {stderr}");
        assert_eq!(check["custody"], "isolated");
        assert_eq!(check["environment"]["network"], "none");
        assert_eq!(check["environment"]["image"], ALPINE);
    }
    // The export is gone, the file the container wrote as root included.
    let left: Vec<_> = std::fs::read_dir(w.root.join("exports")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

// covers: E-12, C-5
#[test]
fn a_check_that_declares_an_open_network_has_one() {
    let _runtime = one_at_a_time();
    let w = world(&format!(
        "{}{}",
        check("offline", "true", ""),
        check(
            "online",
            "ip route | grep -q '^default'",
            "    network: open\n",
        ),
    ));
    let (code, out, stderr) = run(&w, &["online"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["checks"][0]["environment"]["network"], "open");
}

// covers: E-12
#[test]
fn a_check_that_exits_125_is_told_from_the_runtimes_failure() {
    let _runtime = one_at_a_time();
    let w = world(&format!(
        "{}{}",
        check("offline", "exit 125", ""),
        check("missing", "true", "    shell: bash\n"),
    ));
    // The check's own 125 is a failure, recorded; Alpine has no bash, and the runtime's 127 for
    // a command it can't start is the check's failure too.
    let (code, out, stderr) = run(&w, &["offline", "missing"], &[]);
    assert_eq!(code, 8, "{out}\n{stderr}");
    assert_eq!(out["checks"][0]["result"], "fail", "{stderr}");
    assert_eq!(out["checks"][0]["exitCode"], 125, "{out}");
    assert_eq!(out["checks"][1]["result"], "fail", "{stderr}");
    // Nothing is left behind.
    let left = std::process::Command::new(
        std::env::var("HUSHRUN_CONTAINER_RUNTIME").unwrap_or_else(|_| "docker".into()),
    )
    .args([
        "ps",
        "--all",
        "--quiet",
        "--filter",
        "label=com.hushrun.check",
    ])
    .output()
    .unwrap();
    assert!(
        left.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&left.stdout)
    );
}

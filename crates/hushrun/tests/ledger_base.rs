//! `HUSHRUN_REPOSITORY_BASE` (T-4): an organization's one variable gives each repository its ledger,
//! `<base>/<repository id>`, with the id from the CI provider. \[ADR 0062\] The pipeline plans
//! without a stage.

use hushrun_testkit::fake_ledger::FakeLedger;
use hushrun_testkit::world::World;
use serde_json::Value;

const MACHINE: u8 = 1; // a member's machine

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn run(cmd: &mut assert_cmd::Command) -> (i32, Vec<u8>, String) {
    let output = cmd.output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A world, a second server under a base, at `<base>/github.com/acme/shop`, and the base's
/// URL. hushrun.yaml names that second server and pins its log key instead of the world's (T-9).
fn under_a_base() -> (World, FakeLedger, String) {
    let w = World::new("org-owned.yaml");
    let derived = FakeLedger::start_at("/l/acme/github.com/acme/shop");
    derived.set_repository("github.com/acme/shop");
    let base = derived.url().replace("/github.com/acme/shop", "");
    point_at(&w, &derived);
    (w, derived, base)
}

/// hushrun.yaml names `server` and pins its log key (T-9), and `server` names the world's members
/// (T-15).
fn point_at(w: &World, server: &FakeLedger) {
    let (origin, key) = server.log_key();
    for n in hushrun_testkit::world::MEMBERS {
        server.register_machine(
            &hushrun_testkit::world::test_key(n).public_key(),
            &hushrun_testkit::world::member(n),
            n == 1,
        );
    }
    let text = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let mut kept = String::new();
    let mut skipping = false;
    for line in text.lines() {
        if !line.is_empty() && !line.starts_with([' ', '#']) {
            skipping = line.starts_with("transparency_log:");
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept.push_str(&format!(
        "transparency_log:\n  origin: {origin}\n  key: \"{key}\"\n"
    ));
    w.write("hushrun.yaml", &kept);
    set_ledger(w, Some(&server.url()));
}

/// hushrun.yaml's `repository_url` line, set or removed, committed.
fn set_ledger(w: &World, ledger: Option<&str>) {
    let text = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let text: String = text
        .lines()
        .filter_map(|line| match (line.starts_with("repository_url:"), ledger) {
            (true, Some(url)) => Some(format!("repository_url: {url}\n")),
            (true, None) => None,
            (false, _) => Some(format!("{line}\n")),
        })
        .collect();
    w.write("hushrun.yaml", &text);
    w.commit("ledger");
}

/// A GitHub Actions job in `repository`, with the organization's base and no HUSHRUN_REPOSITORY_URL.
fn on_github(w: &World, base: &str, repository: &str) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(MACHINE);
    cmd.env_remove("HUSHRUN_REPOSITORY_URL")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_RUN_ID", "100")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_REPOSITORY", repository)
        .env("GITHUB_SERVER_URL", "https://github.com")
        .env("GITHUB_EVENT_NAME", "push")
        .env("GITHUB_REF", "refs/heads/develop")
        .env("GITHUB_SHA", w.head())
        .env("GITHUB_OUTPUT", w.path("output"))
        .env("GITHUB_STEP_SUMMARY", w.path("summary"))
        .env("HUSHRUN_REPOSITORY_BASE", base)
        .args(["plan", "--format", "json"]);
    cmd
}

// covers: T-4
#[test]
fn ci_reads_the_ledger_the_base_gives_its_repository() {
    let (w, derived, base) = under_a_base();
    let (code, stdout, stderr) = run(&mut on_github(&w, &base, "acme/shop"));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(json(&stdout)["scopes"][0]["mode"], "full");
    // The ledger the plan read is the derived one: a plan --start writes there.
    let (code, _, stderr) = run(on_github(&w, &base, "acme/shop").arg("--start"));
    assert_eq!(code, 0, "{stderr}");
    let digests: std::collections::BTreeSet<String> = derived
        .keys()
        .iter()
        .filter_map(|k| k.strip_prefix("v1/events/default/"))
        .filter_map(|k| k.split('/').next().map(str::to_string))
        .collect();
    assert_eq!(digests.len(), 1, "one digest under the derived ledger");
    assert!(
        w.keys("v1/").is_empty(),
        "nothing in the world's own ledger"
    );
}

// covers: T-4
#[test]
fn with_a_base_hushrun_yaml_must_name_the_same_ledger_and_repository() {
    let (w, derived, base) = under_a_base();
    // Another repository: its id comes from the CI provider, not from the tree.
    let (code, _, stderr) = run(&mut on_github(&w, &base, "acme/other"));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("this run is in github.com/acme/other, but hushrun.yaml's repository is github.com/acme/shop"),
        "{stderr}"
    );
    let (code, _, stderr) = run(&mut on_github(&w, &base, "Acme/Shop"));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("differ only in case"), "{stderr}");

    // A ledger elsewhere: a changed base can't redirect a repository silently.
    set_ledger(
        &w,
        Some("https://elsewhere.example/l/acme/github.com/acme/shop"),
    );
    let (code, _, stderr) = run(&mut on_github(&w, &base, "acme/shop"));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("but hushrun.yaml's `repository_url` is https://elsewhere.example/l/acme/github.com/acme/shop: a changed base can't redirect a repository silently"),
        "{stderr}"
    );
    // No ledger at all.
    set_ledger(&w, None);
    let (code, _, stderr) = run(&mut on_github(&w, &base, "acme/shop"));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains(&format!(
            "hushrun.yaml must name the URL it gives github.com/acme/shop: add `repository_url: {}",
            derived.url()
        )),
        "{stderr}"
    );
}

// covers: T-4
#[test]
fn hushrun_ledger_wins_over_the_base_and_generic_ci_needs_it() {
    let (w, _derived, base) = under_a_base();
    point_at(&w, &w.fake);
    let (code, _, stderr) =
        run(on_github(&w, &base, "acme/shop").env("HUSHRUN_REPOSITORY_URL", w.ledger_url()));
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("overrides the URL HUSHRUN_REPOSITORY_BASE gives this repository"),
        "{stderr}"
    );

    // Generic CI doesn't say which repository runs.
    let (code, _, stderr) = run(w
        .hushrun_as(MACHINE)
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .env("CI", "true")
        .env("HUSHRUN_RUN_ID", "build-1")
        .env("HUSHRUN_REPOSITORY_BASE", &base)
        .args(["plan"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("set HUSHRUN_REPOSITORY_URL instead"),
        "{stderr}"
    );
    // T-4: with neither, hushrun.yaml names the repository's URL, and only an ID token goes
    // there; this fake takes the admin token, which isn't sent.
    let (code, _, stderr) = run(w
        .hushrun_as(MACHINE)
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .env("CI", "true")
        .env("HUSHRUN_RUN_ID", "build-1")
        .args(["plan"]));
    assert_eq!(code, 6, "{stderr}");
    assert!(
        stderr.contains("comes from hushrun.yaml, so hushrun sends it the job's ID token"),
        "{stderr}"
    );
}

// covers: T-4, L-10
#[test]
fn locally_the_base_applies_to_hushrun_yaml_s_repository_without_a_token() {
    let (w, derived, base) = under_a_base();
    set_ledger(&w, Some("https://not-used.example/l/x"));
    let (_, _, stderr) = run(w
        .hushrun()
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .env("HUSHRUN_REPOSITORY_BASE", &base)
        .args(["plan"]));
    // The base gives hushrun.yaml's repository its ledger, and that is the one asked.
    assert!(!derived.requests().is_empty(), "{stderr}");
    // But the repository comes from the tree, so no token goes there (T-4).
    assert!(stderr.contains("sends it no token"), "{stderr}");
    assert!(
        derived.requests().iter().all(|r| r.authorization.is_none()),
        "{stderr}"
    );
}

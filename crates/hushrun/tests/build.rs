//! Build blocks from the command line (B-1, B-2, B-19, B-20, B-21): `hushrun build`, the wrapper
//! `hushrun run` sets for COMMAND, and the binary called by the wrapper's name.

use std::path::{Path, PathBuf};

use hushrun_testkit::world::World;
use serde_json::Value;

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

/// Prints the wrapper COMMAND gets, and nothing else.
const PRINT_WRAPPER: &str = r#"printf %s "$RUSTC_WRAPPER""#;

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn of(cmd: &mut assert_cmd::Command) -> Ran {
        let output = cmd.output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// hushrun's lines on stderr that hold `text`.
    fn lines_with(&self, text: &str) -> usize {
        self.stderr.lines().filter(|l| l.contains(text)).count()
    }
}

/// `hushrun` in the world, with no hosted backend in reach.
fn hushrun(w: &World) -> assert_cmd::Command {
    let mut cmd = w.hushrun();
    cmd.env("HUSHRUN_HOST", NO_HOST);
    cmd
}

fn build(w: &World, args: &[&str]) -> Ran {
    Ran::of(hushrun(w).arg("build").args(args))
}

/// `hushrun run NAME --force -- sh -c SCRIPT`: COMMAND runs whatever passed before.
fn run_sh(cmd: &mut assert_cmd::Command, name: &str, script: &str) -> Ran {
    Ran::of(cmd.args(["run", name, "--force", "--", "sh", "-c", script]))
}

/// The settings file: `build.json` beside the credentials the world names.
fn settings(w: &World) -> PathBuf {
    w.root.join("build.json")
}

fn saved_mode(w: &World) -> String {
    let text = std::fs::read_to_string(settings(w)).expect("build.json");
    let value: Value = serde_json::from_str(&text).expect("JSON");
    value["mode"].as_str().expect("mode").to_string()
}

fn status_json(w: &World) -> Value {
    let ran = build(w, &["status", "--format", "json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    serde_json::from_str(&ran.stdout).expect("JSON")
}

// covers: B-1, B-21
#[test]
fn build_sets_the_mode_in_build_json_beside_the_credentials() {
    let w = World::new("acme-api.yaml");
    let store = w.cache.join("build");

    // Off until the person turns it on; nothing is written by looking.
    let ran = build(&w, &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.starts_with("mode      off\n"), "{}", ran.stdout);
    assert!(
        ran.stdout
            .contains(&format!("settings  {}\n", settings(&w).display())),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains(&format!("store     {}\n", store.display())),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains("keeps     a block 15 days after its last use\n"),
        "{}",
        ran.stdout
    );
    assert!(!settings(&w).exists());

    let on = build(&w, &["on"]);
    assert_eq!(on.code, 0, "{}", on.stderr);
    assert_eq!(
        on.stdout,
        format!(
            "Build reuse is on: commands run by `hushrun run` reuse compiled crates kept in {}.\nA block not used for 15 days is deleted.\n",
            store.display()
        )
    );
    assert_eq!(on.stderr, "");
    assert_eq!(saved_mode(&w), "on");
    assert!(build(&w, &["status"]).stdout.starts_with("mode      on\n"));

    // A second `on` changes nothing, and succeeds.
    let again = build(&w, &["on"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        again.stdout.starts_with("Build reuse is already on: "),
        "{}",
        again.stdout
    );
    assert_eq!(saved_mode(&w), "on");

    let observe = build(&w, &["observe"]);
    assert_eq!(observe.code, 0, "{}", observe.stderr);
    assert_eq!(
        observe.stdout,
        format!(
            "Build reuse is in observe: everything still compiles, and each `hushrun run` says what would have been reused.\nCompiled crates are kept in {}, and a block not used for 15 days is deleted.\n",
            store.display()
        )
    );
    assert_eq!(saved_mode(&w), "observe");
    assert_eq!(status_json(&w)["mode"], "observe");

    // What happens to programs on macOS: movable unless the person chose tied.
    let movable = build(&w, &["status"]);
    assert!(
        movable.stdout.contains(
            "programs  movable: a program on macOS gets a debug bundle and may be reused in another folder\n"
        ),
        "{}",
        movable.stdout
    );
    assert_eq!(status_json(&w)["programs"], "movable");
    let mut file: Value =
        serde_json::from_str(&std::fs::read_to_string(settings(&w)).expect("settings"))
            .expect("JSON");
    file["programs"] = "tied".into();
    std::fs::write(settings(&w), file.to_string()).expect("settings");
    let tied = build(&w, &["status"]);
    assert!(
        tied.stdout.contains(
            "programs  tied: a program is reused only in the target folder that built it\n"
        ),
        "{}",
        tied.stdout
    );
    assert_eq!(status_json(&w)["programs"], "tied");

    let off = build(&w, &["off"]);
    assert_eq!(off.code, 0, "{}", off.stderr);
    assert_eq!(
        off.stdout,
        format!(
            "Build reuse is off: nothing is reused or stored.\n{} is left as it is: `hushrun build clean` empties it.\n",
            store.display()
        )
    );
    assert_eq!(saved_mode(&w), "off");

    // -q says nothing; the file still changes.
    let quiet = build(&w, &["on", "-q"]);
    assert_eq!(
        (quiet.code, quiet.stdout.as_str()),
        (0, ""),
        "{}",
        quiet.stderr
    );
    assert_eq!(saved_mode(&w), "on");

    // A variable overrides the mode for one shell, and status says so.
    let overridden = Ran::of(hushrun(&w).env("HUSHRUN_BUILD", "off").args(["build"]));
    assert!(
        overridden.stdout.starts_with(
            "mode      off\n          HUSHRUN_BUILD=off overrides the saved mode, on, in this shell\n"
        ),
        "{}",
        overridden.stdout
    );
    let json: Value = serde_json::from_str(
        &Ran::of(
            hushrun(&w)
                .env("HUSHRUN_BUILD", "off")
                .args(["build", "--format", "json"]),
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(json["mode"], "off");
    assert_eq!(json["saved_mode"], "on");
    assert_eq!(json["overridden_by"], "HUSHRUN_BUILD");
    assert_eq!(json["settings"], settings(&w).to_str().unwrap());
    assert_eq!(json["held"]["blocks"], 0);
}

// covers: B-21
#[test]
fn clean_empties_the_store_and_says_so() {
    let w = World::new("acme-api.yaml");
    let ran = build(&w, &["clean"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        format!("{} held no blocks.\n", w.cache.join("build").display())
    );
    // The variables that name the store must be usable, or another store could be emptied.
    let refused = Ran::of(
        hushrun(&w)
            .env("HUSHRUN_BUILD_DIR", "relative/store")
            .args(["build", "clean"]),
    );
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("Nothing was removed"),
        "{}",
        refused.stderr
    );
}

// covers: B-2
#[cfg(unix)]
#[test]
fn run_sets_the_wrapper_only_when_reuse_is_on_here() {
    let w = World::new("acme-api.yaml");
    let tested = std::fs::canonicalize(assert_cmd::cargo::cargo_bin("hushrun")).unwrap();

    // Off: nothing set.
    let off = run_sh(&mut hushrun(&w), "x", PRINT_WRAPPER);
    assert_eq!((off.code, off.stdout.as_str()), (0, ""), "{}", off.stderr);

    assert_eq!(build(&w, &["on"]).code, 0);
    let on = run_sh(&mut hushrun(&w), "x", PRINT_WRAPPER);
    assert_eq!(on.code, 0, "{}", on.stderr);
    let wrapper = PathBuf::from(&on.stdout);
    assert_eq!(
        wrapper.file_name().and_then(|n| n.to_str()),
        Some("hushrun-rustc"),
        "{}",
        on.stdout
    );
    assert!(wrapper.starts_with(w.cache.join("build")), "{}", on.stdout);
    assert_eq!(std::fs::canonicalize(&wrapper).unwrap(), tested);
    // The session the wrapper reads is gone once the run is over.
    let sessions = w.cache.join("build/sessions");
    assert_eq!(
        std::fs::read_dir(&sessions).map_or(0, Iterator::count),
        0,
        "{}",
        sessions.display()
    );

    // A wrapper the person set stays.
    let set = run_sh(
        hushrun(&w).env("RUSTC_WRAPPER", "sccache"),
        "x",
        PRINT_WRAPPER,
    );
    assert_eq!(
        (set.code, set.stdout.as_str()),
        (0, "sccache"),
        "{}",
        set.stderr
    );

    // Never in CI.
    let ci = run_sh(hushrun(&w).env("CI", "true"), "x", PRINT_WRAPPER);
    assert_eq!((ci.code, ci.stdout.as_str()), (0, ""), "{}", ci.stderr);

    // One command can turn it off.
    let one = run_sh(hushrun(&w).env("HUSHRUN_BUILD", "off"), "x", PRINT_WRAPPER);
    assert_eq!((one.code, one.stdout.as_str()), (0, ""), "{}", one.stderr);
    assert_eq!(one.lines_with("build reuse"), 0, "{}", one.stderr);
}

// covers: B-1, B-19
#[cfg(unix)]
#[test]
fn a_variable_or_settings_that_cant_be_used_cost_one_line_and_never_the_command() {
    let w = World::new("acme-api.yaml");
    let script = r#"printf %s "$RUSTC_WRAPPER"; exit 5"#;

    let nonsense = run_sh(hushrun(&w).env("HUSHRUN_BUILD", "nonsense"), "x", script);
    assert_eq!(
        (nonsense.code, nonsense.stdout.as_str()),
        (5, ""),
        "{}",
        nonsense.stderr
    );
    assert_eq!(
        nonsense.lines_with(
            "hushrun: build reuse is off for this run: HUSHRUN_BUILD=\"nonsense\" is not valid: expected off, observe or on"
        ),
        1,
        "{}",
        nonsense.stderr
    );
    assert_eq!(nonsense.lines_with("build reuse"), 1, "{}", nonsense.stderr);

    // A settings file that isn't JSON.
    std::fs::write(settings(&w), "{ not json").unwrap();
    let broken = run_sh(&mut hushrun(&w), "y", script);
    assert_eq!(
        (broken.code, broken.stdout.as_str()),
        (5, ""),
        "{}",
        broken.stderr
    );
    assert_eq!(
        broken.lines_with(&format!(
            "hushrun: build reuse is off for this run: {} is not valid build settings",
            settings(&w).display()
        )),
        1,
        "{}",
        broken.stderr
    );
    assert_eq!(broken.lines_with("build reuse"), 1, "{}", broken.stderr);

    // -q keeps it quiet, and the code is still COMMAND's.
    let quiet = run_sh(hushrun(&w).arg("-q"), "z", script);
    assert_eq!((quiet.code, quiet.stderr.as_str()), (5, ""));

    // Status shows the file can't be read, as a configuration error.
    let status = build(&w, &["status"]);
    assert_eq!(status.code, 2, "{}", status.stderr);
    assert!(
        status.stdout.starts_with("mode      unknown: "),
        "{}",
        status.stdout
    );
    // And `on` doesn't overwrite what it can't read.
    let on = build(&w, &["on"]);
    assert_eq!(on.code, 2, "{}", on.stdout);
    assert_eq!(std::fs::read_to_string(settings(&w)).unwrap(), "{ not json");
}

/// A fake rustc: prints each argument on its own line and exits 7.
#[cfg(unix)]
fn fake_rustc(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("fake-rustc");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

// covers: B-2
#[cfg(unix)]
#[test]
fn called_by_the_wrappers_name_it_is_the_wrapper_and_nothing_else() {
    let w = World::new("acme-api.yaml");
    let bin = w.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let link = bin.join("hushrun-rustc");
    std::os::unix::fs::symlink(assert_cmd::cargo::cargo_bin("hushrun"), &link).unwrap();
    let rustc = fake_rustc(&w.root);
    // Settings and variables the CLI would read and speak of: the wrapper reads none.
    std::fs::write(settings(&w), "{ not json").unwrap();
    let args = [
        "--crate-name",
        "two words",
        "--format",
        "xml",
        "-q",
        "--help",
        "run",
        "",
    ];
    let mut cmd = assert_cmd::Command::new(&link);
    cmd.current_dir(&w.repo)
        .env("HUSHRUN_CACHE_DIR", &w.cache)
        .env("HUSHRUN_CREDENTIALS", w.root.join("credentials.json"))
        .env("HUSHRUN_HOST", NO_HOST)
        .env("HUSHRUN_FORMAT", "xml")
        .env("HUSHRUN_BUILD", "nonsense")
        .env("HUSHRUN_LOG", "trace")
        .env_remove("HUSHRUN_NO_UPDATE_CHECK")
        .arg(&rustc)
        .args(args);
    let ran = Ran::of(&mut cmd);
    assert_eq!(ran.code, 7, "{}", ran.stderr);
    let expected: String = args.iter().map(|a| format!("{a}\n")).collect();
    assert_eq!(ran.stdout, expected);
    assert_eq!(ran.stderr, "");
    // No hook, and nothing written in the cache.
    assert!(!w.repo.join(".git/hooks/pre-push").exists());
    assert!(!w.cache.join("build").exists());
}

// covers: B-2, B-20
#[cfg(unix)]
#[test]
fn cargo_builds_through_the_wrapper() {
    let w = World::new("acme-api.yaml");
    let tiny = w.root.join("tiny");
    std::fs::create_dir_all(tiny.join("src")).unwrap();
    std::fs::write(
        tiny.join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        tiny.join("src/lib.rs"),
        "pub fn two() -> u32 {\n    1 + 1\n}\n",
    )
    .unwrap();
    assert_eq!(build(&w, &["on"]).code, 0);
    let target = w.root.join("target");
    let mut cmd = hushrun(&w);
    cmd.current_dir(&tiny)
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .args(["run", "b", "--", "cargo", "build", "--offline", "-v"]);
    let ran = Ran::of(&mut cmd);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        target.join("debug/libtiny.rlib").is_file(),
        "{}",
        ran.stderr
    );
    // Cargo's verbose line for the compile starts with the wrapper.
    assert!(
        ran.stderr.lines().any(|l| l.contains("Running `")
            && l.contains("hushrun-rustc")
            && l.contains("--crate-name tiny")),
        "{}",
        ran.stderr
    );
    // The run linked the wrapper in the store.
    let links = std::fs::read_dir(w.cache.join("build/bin"))
        .unwrap()
        .count();
    assert_eq!(links, 1);
}

/// A check that says the wrapper it gets, as `wrapper=[…]` on a line of its own, then exits
/// with `code`.
fn wrapper_check(code: i32) -> String {
    format!("{{run: 'echo \"wrapper=[$RUSTC_WRAPPER]\"; exit {code}'}}")
}

/// A world whose hushrun.yaml has `checks`, each `name: definition`.
fn with_checks(checks: &[(&str, String)]) -> World {
    let w = World::new("acme-api.yaml");
    let listed: String = checks
        .iter()
        .map(|(name, def)| format!("  {name}: {def}\n"))
        .collect();
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/Acme/api\n{}\nchecks:\n{listed}deploy:\n  branch: develop\n  require: [{}]\n",
            w.ledger_lines(),
            checks[0].0
        ),
    );
    w.commit("checks");
    w
}

/// `hushrun run CHECK… --force --format json`, signing as a member's machine: hushrun.yaml's
/// checks, whatever evidence exists.
fn run_checks(cmd: &mut assert_cmd::Command, checks: &[&str]) -> (Ran, Value) {
    let ran = Ran::of(
        cmd.arg("run")
            .args(checks)
            .args(["--force", "--format", "json"]),
    );
    let value = serde_json::from_str(&ran.stdout).unwrap_or(Value::Null);
    (ran, value)
}

fn member(w: &World) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(1);
    cmd.env("HUSHRUN_HOST", NO_HOST);
    cmd
}

/// What each `wrapper_check` that ran said it got, in order.
fn wrappers(ran: &Ran) -> Vec<String> {
    ran.stderr
        .lines()
        .filter_map(|l| l.strip_prefix("wrapper=[")?.strip_suffix(']'))
        .map(str::to_string)
        .collect()
}

/// The session folders left in the world's store.
fn sessions_left(w: &World) -> usize {
    std::fs::read_dir(w.cache.join("build/sessions")).map_or(0, Iterator::count)
}

// covers: B-2
#[cfg(unix)]
#[test]
fn run_check_sets_the_wrapper_only_when_reuse_is_on_here() {
    let w = with_checks(&[("w", wrapper_check(0))]);
    let tested = std::fs::canonicalize(assert_cmd::cargo::cargo_bin("hushrun")).unwrap();

    // Off: nothing set.
    let (off, _) = run_checks(&mut member(&w), &["w"]);
    assert_eq!(off.code, 0, "{}", off.stderr);
    assert_eq!(wrappers(&off), [""], "{}", off.stderr);

    assert_eq!(build(&w, &["on"]).code, 0);
    let (on, value) = run_checks(&mut member(&w), &["w"]);
    assert_eq!(on.code, 0, "{}", on.stderr);
    assert_eq!(value["checks"][0]["result"], "pass", "{}", on.stdout);
    let found = wrappers(&on);
    assert_eq!(found.len(), 1, "{}", on.stderr);
    let wrapper = PathBuf::from(&found[0]);
    assert_eq!(
        wrapper.file_name().and_then(|n| n.to_str()),
        Some("hushrun-rustc"),
        "{}",
        on.stderr
    );
    assert!(wrapper.starts_with(w.cache.join("build")), "{}", on.stderr);
    assert_eq!(std::fs::canonicalize(&wrapper).unwrap(), tested);
    // The session the wrapper reads is gone once the check is over.
    assert_eq!(sessions_left(&w), 0);

    // A wrapper the person set stays.
    let (set, _) = run_checks(member(&w).env("RUSTC_WRAPPER", "sccache"), &["w"]);
    assert_eq!(set.code, 0, "{}", set.stderr);
    assert_eq!(wrappers(&set), ["sccache"], "{}", set.stderr);

    // One command can turn it off.
    let (one, _) = run_checks(member(&w).env("HUSHRUN_BUILD", "off"), &["w"]);
    assert_eq!(wrappers(&one), [""], "{}", one.stderr);
    assert_eq!(one.lines_with("build reuse"), 0, "{}", one.stderr);
}

// covers: B-2, B-19
#[cfg(unix)]
#[test]
fn a_check_that_fails_keeps_its_exit_code_with_reuse_on() {
    let w = with_checks(&[("fails", wrapper_check(5)), ("ok", wrapper_check(0))]);
    assert_eq!(build(&w, &["on"]).code, 0);
    let (ran, value) = run_checks(&mut member(&w), &["fails", "ok"]);
    // hushrun's code for a failed check, and the check's own in the result.
    assert_eq!(ran.code, 8, "{}", ran.stderr);
    assert_eq!(value["checks"][0]["result"], "fail", "{}", ran.stdout);
    assert_eq!(value["checks"][0]["exitCode"], 5, "{}", ran.stdout);
    assert_eq!(value["checks"][1]["result"], "pass", "{}", ran.stdout);
    // Each check got the wrapper, through a session of its own, and none is left.
    let found = wrappers(&ran);
    assert_eq!(found.len(), 2, "{}", ran.stderr);
    assert!(
        found.iter().all(|w| w.ends_with("/hushrun-rustc")),
        "{}",
        ran.stderr
    );
    assert_eq!(sessions_left(&w), 0);
}

// covers: B-19
#[cfg(unix)]
#[test]
fn run_check_with_settings_that_cant_be_used_says_so_once_and_runs_every_check() {
    let w = with_checks(&[("fails", wrapper_check(5)), ("ok", wrapper_check(0))]);
    let (ran, value) = run_checks(
        member(&w).env("HUSHRUN_BUILD", "nonsense"),
        &["fails", "ok"],
    );
    assert_eq!(ran.code, 8, "{}", ran.stderr);
    assert_eq!(value["checks"][0]["exitCode"], 5, "{}", ran.stdout);
    assert_eq!(value["checks"][1]["result"], "pass", "{}", ran.stdout);
    assert_eq!(wrappers(&ran), ["", ""], "{}", ran.stderr);
    assert_eq!(
        ran.lines_with(
            "hushrun run: build reuse is off for this run: HUSHRUN_BUILD=\"nonsense\" is not valid: expected off, observe or on"
        ),
        1,
        "{}",
        ran.stderr
    );
    assert_eq!(ran.lines_with("build reuse"), 1, "{}", ran.stderr);

    // -q keeps it quiet.
    let (quiet, _) = run_checks(
        member(&w).env("HUSHRUN_BUILD", "nonsense").arg("-q"),
        &["fails", "ok"],
    );
    assert_eq!(quiet.code, 8, "{}", quiet.stderr);
    assert_eq!(quiet.lines_with("build reuse"), 0, "{}", quiet.stderr);
}

// covers: B-2
#[cfg(unix)]
#[test]
fn an_isolated_check_never_gets_the_wrapper() {
    use hushrun_testkit::container::FakeRuntime;
    const IMAGE: &str =
        "golang@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let w = with_checks(&[(
        "unit",
        format!(
            "{{image: \"{IMAGE}\", platform: linux/amd64, run: 'echo \"wrapper=[$RUSTC_WRAPPER$HUSHRUN_BUILD_SESSION]\"'}}"
        ),
    )]);
    let rt = FakeRuntime::install(&w.root);
    assert_eq!(build(&w, &["on"]).code, 0);
    let mut cmd = member(&w);
    cmd.env("HUSHRUN_CONTAINER_RUNTIME", &rt.path)
        .env("HUSHRUN_EXPORT_DIR", w.root.join("exports"));
    let (ran, value) = run_checks(&mut cmd, &["unit", "--isolated"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(value["checks"][0]["result"], "pass", "{}", ran.stdout);
    assert_eq!(wrappers(&ran), [""], "{}", ran.stderr);
    // No session was begun and nothing was linked: the container has neither.
    assert!(!w.cache.join("build").exists(), "{}", ran.stderr);
    let runs = rt.runs();
    assert_eq!(runs.len(), 1);
    // The check declares no variables, so none goes in.
    assert!(!runs[0].iter().any(|a| a == "--env"), "{:?}", runs[0]);
}

use super::*;

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn run_file(root: &Path, extra: serde_json::Value) -> RunFile {
    let mut value = serde_json::json!({
        "schema": "hushrun.units/v0",
        "root": root,
        "reuse": [],
    });
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn has_bash() -> bool {
    std::process::Command::new("bash")
        .arg("-c")
        .arg("true")
        .status()
        .is_ok_and(|s| s.success())
}

// covers: C-86
#[test]
fn bats_is_recognized_by_its_name_a_path_or_a_launcher() {
    for yes in [
        "bats test",
        "bats -r test --jobs 4",
        "./test/bats/bin/bats test",
        "node_modules/.bin/bats test",
        "npx bats test",
        "npx --no-install bats test",
        "bunx bats test",
        "pnpm bats test",
        "pnpm exec bats test",
        "yarn exec bats test",
        "bun x bats test",
        "npm exec -- bats test",
    ] {
        assert!(starts_bats(&words(yes)), "{yes}");
    }
    for no in [
        "pytest",
        "bash -c bats",
        "npm test",
        "batsman test",
        "bun bats",
        "npx vitest run",
    ] {
        assert!(!starts_bats(&words(no)), "{no}");
    }
    // `env NAME=value bats` is bats, by the registry's own reading of `env`.
    let chosen = super::super::adapter::choose(&words("env A=1 bats test"), Path::new("."));
    if cfg!(windows) {
        assert!(chosen.is_err());
    } else {
        assert_eq!(chosen.unwrap().word(), WORD);
    }
}

// covers: C-86
#[test]
fn the_hook_loads_through_bash_env_and_keeps_the_one_the_environment_had() {
    let vars = variables(
        Path::new("/c/adapters/bats-1/hushrun_bats.bash"),
        Path::new("/opt/hushrun"),
        Some("/home/me/.bashenv".into()),
    );
    assert_eq!(
        vars,
        vec![
            (
                "BASH_ENV".to_string(),
                OsString::from("/c/adapters/bats-1/hushrun_bats.bash")
            ),
            (
                "HUSHRUN_BATS_ENV_WAS".to_string(),
                OsString::from("/home/me/.bashenv")
            ),
            (
                "HUSHRUN_BATS_EXE".to_string(),
                OsString::from("/opt/hushrun")
            ),
        ]
    );
    let dir = tempfile::tempdir().unwrap();
    let path = install(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK);
    assert_eq!(install(dir.path()).unwrap(), path);
}

// covers: C-87
#[test]
fn a_shell_file_is_a_source_only_when_bash_is_what_runs_it() {
    assert!(bash_runs(b"#!/bin/bash\necho", true));
    assert!(bash_runs(b"#!/usr/bin/env bash\n", true));
    assert!(bash_runs(b"#!/usr/bin/env -S bash -eu\n", true));
    assert!(bash_runs(b"#! /usr/local/bin/bash\n", true));
    assert!(!bash_runs(b"#!/bin/sh\n", true));
    assert!(!bash_runs(b"#!/usr/bin/env zsh\n", false));
    assert!(!bash_runs(b"#!/bin/dash\n", false));
    // No `#!` line: a library `source` reads, unless it can be run as a program.
    assert!(bash_runs(b"greet() { :; }\n", false));
    assert!(!bash_runs(b"greet() { :; }\n", true));
}

// covers: C-87
#[cfg(unix)]
#[test]
fn the_sets_are_test_files_bash_files_and_the_rest() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let files: &[(&str, &str, bool)] = &[
        ("test/a.bats", "@test x { :; }\n", false),
        ("test/helper.bash", "h() { :; }\n", false),
        ("lib/lib.sh", "l() { :; }\n", false),
        ("lib/run.sh", "#!/usr/bin/env bash\n", true),
        ("lib/posix.sh", "#!/bin/sh\n", true),
        ("lib/noshebang.sh", "echo\n", true),
        ("bin/tool", "#!/bin/bash\n", true),
        ("bin/py", "#!/usr/bin/env python3\n", true),
        ("bin/tool.py", "#!/bin/bash\n", true),
        ("README.md", "#!/bin/bash\n", false),
    ];
    for (path, text, exec) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
        let mode = if *exec { 0o755 } else { 0o644 };
        std::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let named = bash_files(root, files.iter().map(|f| f.0));
    assert_eq!(
        named.iter().map(String::as_str).collect::<Vec<_>>(),
        ["bin/tool", "lib/lib.sh", "lib/run.sh", "test/helper.bash"]
    );
    let sets = units::classify(
        files.iter().map(|f| f.0),
        &is_test_file,
        Sources::Named(&named, &[""]),
        &[],
    );
    assert_eq!(sets.of("test/a.bats"), Some(Set::Units));
    assert_eq!(sets.of("bin/tool"), Some(Set::Sources));
    assert_eq!(sets.of("lib/posix.sh"), Some(Set::Rest));
    assert_eq!(sets.of("lib/noshebang.sh"), Some(Set::Rest));
    assert_eq!(sets.of("bin/py"), Some(Set::Rest));
    assert_eq!(sets.of("README.md"), Some(Set::Rest));
}

// covers: C-88
#[test]
fn the_plan_reads_back_in_bash_as_exactly_what_hushrun_json_lists() {
    if !has_bash() {
        eprintln!("note: skipping: no bash");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let odd = "it's \"odd\" $(touch x) * \\ é\ttab";
    let file = run_file(
        dir.path(),
        serde_json::json!({
            "reuse": ["test/whole file.bats"],
            "unit": "test",
            "reads": "test",
            "reuseTests": {"test/a.bats": [odd, "plain"]},
        }),
    );
    let plan = plan_bash(&file);
    let script = format!(
        "{plan}\n\
         t() {{ if \"$@\"; then echo yes; else echo no; fi; }}\n\
         t __hushrun_bats_reused_file 'test/whole file.bats'\n\
         t __hushrun_bats_reused_file 'test/whole'\n\
         t __hushrun_bats_reused_test test/a.bats \"$ODD\"\n\
         t __hushrun_bats_reused_test test/a.bats plain\n\
         t __hushrun_bats_reused_test test/a.bats plai\n\
         t __hushrun_bats_reused_test test/a.bats '*'\n\
         t __hushrun_bats_reused_test test/b.bats plain\n\
         printf '%s\\n' \"${{__hushrun_bats_roots[0]}}\"\n"
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("ODD", odd)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[..7], ["yes", "no", "yes", "yes", "no", "no", "no"]);
    assert_eq!(lines[7], dir.path().to_str().unwrap());
    assert!(!dir.path().join("x").exists(), "nothing in a name runs");
    // With tests not the unit, no test is left out one by one.
    let by_file = run_file(
        dir.path(),
        serde_json::json!({"reuseTests": {"test/a.bats": ["plain"]}}),
    );
    assert!(!plan_bash(&by_file).contains("plain"));
}

/// A run's directory of notes, as the hook writes them.
struct Notes {
    _dir: tempfile::TempDir,
    root: PathBuf,
    run: PathBuf,
}

impl Notes {
    fn new() -> Notes {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("repo");
        let run = root.parent().unwrap().join("units/bats-1");
        std::fs::create_dir_all(&run).unwrap();
        for file in [
            "test/a.bats",
            "test/b.bats",
            "test/c.bats",
            "test/helper.bash",
            "lib/x.sh",
            "lib/y.sh",
            "test/setup_suite.bash",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        Notes {
            _dir: dir,
            root,
            run,
        }
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.run.join(name), text).unwrap();
    }

    fn log(&self, name: &str, files: &[&str]) {
        let lines: String = files
            .iter()
            .map(|f| format!("{}\t{f}\n", self.root.join("test").display()))
            .collect();
        self.write(name, &lines);
    }

    fn file(&self, pid: u32, unit: &str, tests: &[&str], reused: bool) {
        self.write(
            &format!("file-{pid}.meta"),
            &format!("unit {unit}\ngiven x\nreused {}\n", u8::from(reused)),
        );
        self.write(
            &format!("file-{pid}.tests"),
            &tests.iter().map(|t| format!("{t} \n")).collect::<String>(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn test(&self, n: u32, try_: u32, unit: &str, name: &str, id: &str, reused: bool, end: &str) {
        let stem = format!("test-{n}-{try_}");
        self.write(
            &format!("{stem}.meta"),
            &format!("unit {unit}\ngiven x\nname {name} \nnumber {n}\ntry {try_}\nstart 1000\n"),
        );
        self.write(&format!("{stem}.id"), id);
        self.write(&format!("{stem}.reused"), if reused { "1" } else { "0" });
        if !end.is_empty() {
            self.write(&format!("{stem}.end"), &format!("{end}\nend 1250\n"));
        }
    }

    fn facts(&self, extra: serde_json::Value) -> FactsFile {
        facts(&run_file(&self.root, extra), &self.run, "bats-1")
    }
}

fn unit<'a>(facts: &'a FactsFile, name: &str) -> &'a UnitFacts {
    facts
        .units
        .iter()
        .find(|u| u.unit == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn test<'a>(unit: &'a UnitFacts, id: &str) -> &'a TestFacts {
    unit.tests
        .as_ref()
        .unwrap()
        .iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no {id}"))
}

// covers: C-88
#[test]
fn facts_say_what_each_test_ran_and_how_it_ended() {
    let n = Notes::new();
    n.log(
        "suite.log",
        &["setup_suite.bash", "/usr/lib/bats-core/tracing.bash"],
    );
    n.file(
        10,
        "test/a.bats",
        &["test_one", "test_two", "test_three"],
        false,
    );
    n.log("file-10.log", &["helper.bash", "../test/./a.bats"]);
    n.test(
        1,
        1,
        "test/a.bats",
        "test_one",
        "one",
        false,
        "state passed\nskipped 0",
    );
    n.log(
        "test-1-1.log",
        &["../lib/x.sh", "/tmp/elsewhere.sh", "../lib/missing.sh"],
    );
    n.test(
        2,
        1,
        "test/a.bats",
        "test_two",
        "two",
        false,
        "state failed\nskipped 0",
    );
    n.log("test-2-1.log", &["../lib/y.sh"]);
    n.test(
        3,
        1,
        "test/a.bats",
        "test_three",
        "three",
        false,
        "state passed\nskipped 1",
    );
    let facts = n.facts(serde_json::json!({"unit": "test", "reads": "test"}));
    assert_eq!(facts.schema, facts::FACTS_SCHEMA_V1);
    assert!(facts.failures_by_unit);
    assert_eq!(facts.start.as_deref(), Some("bats-1"));
    let a = unit(&facts, "test/a.bats");
    assert_eq!(
        (a.selected, a.finished, a.failed, a.skipped),
        (3, 3, true, 1)
    );
    assert!(!a.deselected);
    assert_eq!(
        a.reads,
        [
            "lib/x.sh",
            "lib/y.sh",
            "test/a.bats",
            "test/helper.bash",
            "test/setup_suite.bash"
        ]
    );
    let ran = |t: &TestFacts| facts.resolve(t.ran.as_ref().unwrap()).unwrap();
    let one = test(a, "one");
    assert!(one.finished && !one.failed && !one.skipped);
    assert_eq!(one.duration_ms, 250);
    assert_eq!(ran(one), ["lib/x.sh".to_string()].into());
    assert!(test(a, "two").failed);
    assert!(test(a, "three").skipped);
    let loaded = a.loaded.as_ref().unwrap();
    assert_eq!(
        facts.resolve(&loaded.ran).unwrap(),
        ["test/a.bats", "test/helper.bash", "test/setup_suite.bash"]
            .map(str::to_string)
            .into()
    );
    // With test files the unit, no test is told.
    let by_file = n.facts(serde_json::json!({}));
    assert_eq!(by_file.schema, facts::FACTS_SCHEMA);
    assert!(unit(&by_file, "test/a.bats").tests.is_none());
}

// covers: C-88
#[test]
fn what_hushrun_left_out_is_deselected_and_what_didnt_end_didnt_finish() {
    let n = Notes::new();
    // A file reused whole: every test skipped as reused.
    n.file(10, "test/a.bats", &["test_one", "test_two"], true);
    n.test(
        1,
        1,
        "test/a.bats",
        "test_one",
        "one",
        true,
        "state passed\nskipped 1",
    );
    n.test(
        2,
        1,
        "test/a.bats",
        "test_two",
        "two",
        true,
        "state passed\nskipped 1",
    );
    // A file listed whole, one test of which ran anyway: it ran.
    n.file(11, "test/b.bats", &["test_one", "test_two"], true);
    n.test(
        3,
        1,
        "test/b.bats",
        "test_one",
        "one",
        true,
        "state passed\nskipped 1",
    );
    n.test(
        4,
        1,
        "test/b.bats",
        "test_two",
        "two",
        false,
        "state failed\nskipped 0",
    );
    // A test left out by itself, one that never ended, one that never started, and one bats
    // ran twice.
    n.file(
        12,
        "test/c.bats",
        &["test_r", "test_cut", "test_never", "test_flaky"],
        false,
    );
    n.test(
        5,
        1,
        "test/c.bats",
        "test_r",
        "r",
        true,
        "state passed\nskipped 1",
    );
    n.test(6, 1, "test/c.bats", "test_cut", "cut", false, "");
    n.test(8, 1, "test/c.bats", "test_flaky", "flaky", false, "");
    n.test(
        8,
        2,
        "test/c.bats",
        "test_flaky",
        "flaky",
        false,
        "state passed\nskipped 0",
    );
    // A test file outside the repository.
    n.file(13, "", &["test_x"], false);
    let facts = n.facts(serde_json::json!({"unit": "test", "reads": "test"}));
    let a = unit(&facts, "test/a.bats");
    assert!(a.deselected);
    assert_eq!((a.selected, a.finished), (0, 0));
    let b = unit(&facts, "test/b.bats");
    assert!(!b.deselected && b.failed);
    assert!(test(b, "one").deselected);
    assert_eq!((b.selected, b.finished), (1, 1));
    let c = unit(&facts, "test/c.bats");
    assert_eq!((c.selected, c.finished), (3, 1));
    assert!(test(c, "r").deselected);
    let cut = test(c, "cut");
    assert!(!cut.finished && cut.not_kept.is_some());
    let flaky = test(c, "flaky");
    assert!(flaky.finished);
    assert_eq!(flaky.not_followed[0].why, "retried");
    assert_eq!(facts.units.len(), 3);
}

// covers: C-88
#[test]
fn a_failure_no_test_owns_keeps_no_pass_and_an_old_bats_is_an_error() {
    let n = Notes::new();
    n.file(10, "test/a.bats", &["test_one", "test_one_again"], false);
    n.write("file-10.failed", "teardown_file");
    n.test(
        1,
        1,
        "test/a.bats",
        "test_one",
        "same",
        false,
        "state passed\nskipped 0",
    );
    n.test(
        2,
        1,
        "test/a.bats",
        "test_one_again",
        "same",
        false,
        "state passed\nskipped 0",
    );
    n.write("suite.failed", "setup_suite or teardown_suite failed");
    n.write("version", "1.9.0");
    let facts = n.facts(serde_json::json!({"unit": "test"}));
    assert!(!facts.failures_by_unit);
    assert!(facts.not_kept.as_deref().unwrap().contains("setup_suite"));
    assert!(facts.errors[0].contains("1.9"));
    let a = unit(&facts, "test/a.bats");
    assert!(a.failed);
    assert_eq!(a.not_followed[0].detail, "its teardown_file failed");
    // Two tests one id: neither is ever left out.
    assert!(a.tests.as_ref().unwrap().iter().all(|t| t.never_left_out));
    n.write("version", "1.13.0");
    assert!(n.facts(serde_json::json!({})).errors.is_empty());
}

// covers: C-88
#[test]
fn the_subcommand_writes_the_facts_whole_and_says_the_start_ended() {
    let n = Notes::new();
    let units = n.run.parent().unwrap();
    std::fs::write(
        units.join(facts::RUN_FILE),
        serde_json::json!({"schema": "hushrun.units/v0", "root": n.root, "reuse": ["test/a.bats"]})
            .to_string(),
    )
    .unwrap();
    std::fs::write(units.join("bats-1.started"), "{}").unwrap();
    let args = |what: &str| -> Vec<OsString> {
        vec![
            "hushrun".into(),
            SUBCOMMAND.into(),
            what.into(),
            units.into(),
            n.run.clone().into(),
        ]
    };
    assert_eq!(from_args(&args("plan")), Some(ExitCode::SUCCESS));
    assert!(
        std::fs::read_to_string(n.run.join("plan.bash"))
            .unwrap()
            .contains("test/a.bats")
    );
    assert_eq!(from_args(&args("facts")), Some(ExitCode::SUCCESS));
    assert!(!units.join("bats-1.started").exists());
    let read = facts::read_all(units).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].adapter, WORD);
    assert_eq!(from_args(&["hushrun".into(), "run".into()]), None);
}

// covers: C-87, C-89
#[test]
fn the_hook_notes_what_bash_ran_and_not_what_bash_never_saw() {
    if !has_bash() {
        eprintln!("note: skipping: no bash");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let hook = root.join("hook.bash");
    std::fs::write(&hook, HOOK).unwrap();
    let write = |name: &str, text: &str| std::fs::write(root.join(name), text).unwrap();
    write("lib.sh", "greet() { echo \"hi $1\"; }\n");
    write("defs.sh", "never_called() { :; }\n");
    write("child.sh", "echo child\n");
    write("hidden.sh", "echo hidden\n");
    write("posix.sh", "echo posix\n");
    write(
        "main.sh",
        "source ./lib.sh\nlib=./defs.sh\nsource \"$lib\"\nfalse\necho \"status=$? last=$_\"\n\
         greet you\nbash ./child.sh\nenv -i bash ./hidden.sh\nsh ./posix.sh\n",
    );
    let log = root.join("notes.log");
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    let out = std::process::Command::new("bash")
        .arg("./main.sh")
        .current_dir(&root)
        .env("BASH_ENV", &hook)
        .env("HUSHRUN_BATS_EXE", "/nonexistent/hushrun")
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_BATS_LOG", &log)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `$?` and `$_` are what they would be without the hook.
    assert!(stdout.contains("status=1 last=false"), "{stdout}");
    assert!(
        stdout.contains("hi you") && stdout.contains("child"),
        "{stdout}"
    );
    let notes = std::fs::read_to_string(&log).unwrap();
    let noted: BTreeSet<&str> = notes
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(_, f)| f)
        .collect();
    for seen in ["./main.sh", "./lib.sh", "./defs.sh", "./child.sh"] {
        assert!(noted.contains(seen), "{seen} in {notes}");
    }
    // \[C-89\] What no hooked bash ran isn't noted: a process with no environment, and `sh`.
    for unseen in ["./hidden.sh", "./posix.sh"] {
        assert!(!noted.contains(unseen), "{unseen} in {notes}");
    }
    // Without hushrun's variables the hook does nothing.
    let bare = std::process::Command::new("bash")
        .arg("-c")
        .arg("trap -p DEBUG; echo done")
        .env("BASH_ENV", &hook)
        .env_remove("HUSHRUN_UNITS")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "done\n");
}

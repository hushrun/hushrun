//! `hushrun guide` (C-28) and `hushrun skill update` (C-29), and what `hushrun ready` says of a
//! repository's skill (C-22): an agent learns which rules this hushrun has, and whether the skill
//! it read is older, or newer, than this hushrun. Each test has a home and a cache of its own.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::schema;
use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const COPIES: [&str; 2] = [
    ".claude/skills/hushrun/SKILL.md",
    ".agents/skills/hushrun/SKILL.md",
];

struct Place {
    _dir: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Place {
    /// A home, and a git repository with one commit.
    fn new() -> Place {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "T"],
        ] {
            git(&repo, args);
        }
        std::fs::write(repo.join("README.md"), "# shop\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "start"]);
        Place {
            _dir: dir,
            home,
            repo,
        }
    }

    /// hushrun in `dir`, with nothing inherited but `PATH`, and no update check.
    fn hushrun(&self, dir: &Path, args: &[&str]) -> Ran {
        let mut cmd = self.command(dir);
        cmd.env("HUSHRUN_NO_UPDATE_CHECK", "1").args(args);
        ran(&mut cmd)
    }

    /// hushrun in `dir`, with nothing inherited but `PATH`.
    fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("HUSHRUN_CACHE_DIR", self.home.join("cache"))
            .current_dir(dir);
        cmd
    }

    fn in_repo(&self, args: &[&str]) -> Ran {
        self.hushrun(&self.repo, args)
    }

    fn guide(&self) -> Value {
        let ran = self.in_repo(&["guide", "--format", "json"]);
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        let value: Value = serde_json::from_str(&ran.stdout).unwrap();
        schema::assert_valid(&schema::load("guide.v1.schema.json"), &value);
        value
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.repo.join(path)).unwrap()
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ran(cmd: &mut Command) -> Ran {
    let output = cmd.output().unwrap();
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Each copy's state, by its path, as `hushrun guide` reports it.
fn states(guide: &Value) -> Vec<(String, String)> {
    guide["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|copy| {
            (
                copy["path"].as_str().unwrap().to_string(),
                copy["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The skill text of a hushrun from before revisions.
fn before_revisions() -> String {
    std::fs::read_to_string(hushrun_testkit::testdata(
        "skills/before-revisions/2026-11-08-fb01dd77.md",
    ))
    .unwrap()
}

// covers: C-28
#[test]
fn guide_prints_the_rules_of_this_hushrun() {
    let place = Place::new();
    let text = place.in_repo(&["guide"]);
    assert_eq!(text.code, 0, "{}", text.stderr);
    assert!(
        text.stdout.starts_with("# hushrun guide\n"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains("## Reading hushrun's lines"),
        "{}",
        text.stdout
    );
    // Then what is true here: this hushrun, and every copy of the skill it looked at.
    assert!(
        text.stdout
            .contains(&format!("This is hushrun {VERSION}, with skill revision ")),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout
            .contains("- .claude/skills/hushrun/SKILL.md: absent"),
        "{}",
        text.stdout
    );

    let guide = place.guide();
    assert_eq!(guide["schema"], "hushrun.guide/v1");
    assert_eq!(guide["version"], VERSION);
    assert!(guide["skillRevision"].as_u64().unwrap() >= 1);
    assert!(
        guide["guide"]
            .as_str()
            .unwrap()
            .starts_with("# hushrun guide\n")
    );
    assert_eq!(guide["update"], Value::Null);
    assert_eq!(
        states(&guide),
        [
            (COPIES[0].to_string(), "absent".to_string()),
            (COPIES[1].to_string(), "absent".to_string()),
            ("~/.claude/skills/hushrun/SKILL.md".into(), "absent".into()),
            ("~/.agents/skills/hushrun/SKILL.md".into(), "absent".into()),
        ]
    );
    // Outside a repository, only this person's copies.
    let outside = place.hushrun(&place.home, &["guide", "--format", "json"]);
    assert_eq!(outside.code, 0, "{}", outside.stderr);
    let outside: Value = serde_json::from_str(&outside.stdout).unwrap();
    assert_eq!(
        outside["skills"].as_array().unwrap().len(),
        2,
        "{outside:#}"
    );
}

/// The commands `hushrun guide` leaves out on purpose, each with why. Every other command
/// `hushrun --help` lists is named in the guide, as `hushrun <command>`: a command added to the CLI
/// goes into what agents read, or here, with its reason.
const LEFT_OUT_OF_THE_GUIDE: &[(&str, &str)] = &[
    (
        "digest",
        "deploys: CI prints a scope's digest for its pipeline",
    ),
    ("plan", "deploys: CI decides what a deploy does"),
    (
        "guard",
        "deploys: CI refuses to deploy a revision that isn't its branch's head",
    ),
    (
        "status",
        "deploys: the evidence hushrun.yaml's required checks have, for CI's plan",
    ),
    ("attest", "deploys: CI records what other tools produced"),
    ("mark", "deploys: CI records a deploy's lifecycle"),
    (
        "verify",
        "deploys: checks the signatures of recorded events",
    ),
    ("keys", "deploys: signing keys are a person's to make"),
    (
        "policy",
        "deploys: whose evidence counts for hushrun.yaml's checks",
    ),
    (
        "repository",
        "the backend's event log, which a person administers",
    ),
    ("sigstore", "deploys: the trust root of keyless signatures"),
    ("artifact", "deploys: images CI promotes by digest"),
    (
        "adopt",
        "deploys: writes deploy workflows; `init` is what sets up `hushrun run`",
    ),
];

/// The commands `hushrun --help` lists, but `help`.
fn commands(help: &str) -> Vec<String> {
    let listed = help
        .split("\nCommands:\n")
        .nth(1)
        .expect("`hushrun --help` lists its commands");
    listed
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|line| !line.starts_with(' '))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_string)
        .collect()
}

/// Whether `text` names `hushrun <command>`, as a whole word.
fn names(text: &str, command: &str) -> bool {
    let named = format!("hushrun {command}");
    text.match_indices(&named).any(|(at, _)| {
        !text[at + named.len()..]
            .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

// covers: C-28
#[test]
fn guide_names_every_command_but_those_left_out_with_a_reason() {
    // An agent that followed the skill and the guide told a person that hushrun saves no build
    // time or disk: `hushrun build` was in `--help` and in neither. This keeps the two together.
    let place = Place::new();
    let help = place.in_repo(&["--help"]);
    assert_eq!(help.code, 0, "{}", help.stderr);
    let commands = commands(&help.stdout);
    assert!(
        ["run", "ready", "build", "guide"]
            .iter()
            .all(|c| commands.iter().any(|listed| listed == c)),
        "{commands:?}"
    );
    let guide = place.guide();
    let text = guide["guide"].as_str().unwrap();
    for (left_out, why) in LEFT_OUT_OF_THE_GUIDE {
        assert!(
            commands.iter().any(|c| c == left_out),
            "`hushrun {left_out}` is no longer a command: take it out of LEFT_OUT_OF_THE_GUIDE"
        );
        assert!(
            !names(text, left_out),
            "the guide names `hushrun {left_out}`: take it out of LEFT_OUT_OF_THE_GUIDE ({why})"
        );
    }
    let missing: Vec<&String> = commands
        .iter()
        .filter(|c| {
            !LEFT_OUT_OF_THE_GUIDE
                .iter()
                .any(|(left_out, _)| left_out == c)
        })
        .filter(|c| !names(text, c))
        .collect();
    assert!(
        missing.is_empty(),
        "recipes/agents/GUIDE.md names no `hushrun <command>` for {missing:?}: say what each does there, or add it to LEFT_OUT_OF_THE_GUIDE with a reason"
    );
}

// covers: C-28, B-21
#[test]
fn guide_says_build_reuse_s_mode_and_store_here() {
    // The agent that said hushrun saves no build time couldn't see that reuse was on here.
    let place = Place::new();
    let store = place.home.join("cache").join("build");
    let here = |text: &str| -> Vec<String> {
        text.lines()
            .skip_while(|line| *line != "## Here")
            .filter(|line| line.starts_with("Build reuse") || line.starts_with("Its store"))
            .map(str::to_string)
            .collect()
    };
    let text = place.in_repo(&["guide"]);
    assert_eq!(text.code, 0, "{}", text.stderr);
    let lines = here(&text.stdout);
    assert_eq!(lines.len(), 2, "{}", text.stdout);
    assert!(
        lines[0].starts_with("Build reuse here is off. The person's build settings: "),
        "{lines:?}"
    );
    assert!(lines[0].ends_with("build.json."), "{lines:?}");
    assert_eq!(lines[1], format!("Its store: {}, empty.", store.display()));
    let guide = place.guide();
    assert_eq!(guide["build"]["mode"], "off", "{guide:#}");
    assert_eq!(guide["build"]["savedMode"], Value::Null, "{guide:#}");
    assert_eq!(
        guide["build"]["store"],
        store.to_str().unwrap(),
        "{guide:#}"
    );
    assert_eq!(guide["build"]["blocks"], 0, "{guide:#}");
    assert_eq!(guide["build"]["inCi"], false, "{guide:#}");

    // The person turns it on; then a shell's HUSHRUN_BUILD sets another mode over it.
    assert_eq!(place.in_repo(&["build", "on"]).code, 0);
    let lines = here(&place.in_repo(&["guide"]).stdout);
    assert!(
        lines[0].starts_with("Build reuse here is on. "),
        "{lines:?}"
    );
    assert_eq!(place.guide()["build"]["mode"], "on");
    let observed = ran(place
        .command(&place.repo)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_BUILD", "observe")
        .arg("guide"));
    let lines = here(&observed.stdout);
    assert!(
        lines[0].starts_with("Build reuse here is in observe: HUSHRUN_BUILD sets it in this shell"),
        "{lines:?}"
    );
    assert!(lines[0].ends_with("which say on."), "{lines:?}");

    // No cache directory: nothing is kept.
    let none = ran(place
        .command(&place.repo)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_CACHE_DIR", "")
        .args(["guide", "--format", "json"]));
    let none: Value = serde_json::from_str(&none.stdout).unwrap();
    schema::assert_valid(&schema::load("guide.v1.schema.json"), &none);
    assert_eq!(none["build"]["store"], Value::Null, "{none:#}");
    assert_eq!(none["build"]["blocks"], Value::Null, "{none:#}");
}

// covers: C-28
#[test]
fn guide_tells_of_a_newer_release_it_already_learned_of() {
    let place = Place::new();
    let cache = place.home.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("update-check.json"),
        r#"{"format":1,"asked":1,"answered":1,"latest":"999.0.0","noticed":0}"#,
    )
    .unwrap();
    // It reads what the day's check learned, and asks nobody: no GitHub is set up here.
    let json = ran(place
        .command(&place.repo)
        .args(["guide", "--format", "json"]));
    assert_eq!(json.code, 0, "{}", json.stderr);
    let guide: Value = serde_json::from_str(&json.stdout).unwrap();
    schema::assert_valid(&schema::load("guide.v1.schema.json"), &guide);
    assert_eq!(guide["update"]["latest"], "999.0.0", "{guide:#}");
    assert_eq!(guide["update"]["command"], "hushrun update", "{guide:#}");
    // With the check turned off, it says nothing of releases.
    assert_eq!(place.guide()["update"], Value::Null);
    let text = ran(place.command(&place.repo).arg("guide"));
    assert!(
        text.stdout.contains(&format!(
            "hushrun 999.0.0 is available, and this is {VERSION}: when you report back, tell the person, who updates it with `hushrun update`"
        )),
        "{}",
        text.stdout
    );
}

// covers: C-29
#[test]
fn skill_update_writes_the_repositorys_copies() {
    let place = Place::new();
    let ran = place.in_repo(&["skill", "update"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        "Wrote .claude/skills/hushrun/SKILL.md.\nWrote .agents/skills/hushrun/SKILL.md.\n"
    );
    assert_eq!(place.read(COPIES[0]), place.read(COPIES[1]));
    assert!(place.read(COPIES[0]).starts_with("---\nname: hushrun\n"));
    let guide = place.guide();
    let revision = guide["skillRevision"].as_u64().unwrap();
    assert!(
        place
            .read(COPIES[0])
            .contains(&format!("hushrun-skill: \"{revision}\"")),
        "{}",
        place.read(COPIES[0])
    );
    assert_eq!(states(&guide)[0].1, "current");
    assert_eq!(states(&guide)[1].1, "current");
    // Again, nothing changes.
    let again = place.in_repo(&["skill", "update"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(
        again.stdout,
        ".claude/skills/hushrun/SKILL.md is current.\n.agents/skills/hushrun/SKILL.md is current.\n"
    );
    // From a subdirectory, the repository's root.
    std::fs::create_dir_all(place.repo.join("src")).unwrap();
    let below = place.hushrun(&place.repo.join("src"), &["skill", "update"]);
    assert_eq!(below.code, 0, "{}", below.stderr);
    assert!(!place.repo.join("src/.claude").exists());
}

// covers: C-24, C-29 (revision 1, as 0.11.0 to 0.11.1 wrote it, once revision 2 is out)
#[test]
fn a_copy_of_revision_1_is_older_and_skill_update_rewrites_it() {
    let place = Place::new();
    let one = std::fs::read_to_string(hushrun_testkit::testdata("skills/revisions/1.md")).unwrap();
    place.write(COPIES[0], &one);
    place.write(COPIES[1], &one);
    let guide = place.guide();
    assert!(guide["skillRevision"].as_u64().unwrap() >= 2, "{guide:#}");
    assert_eq!(states(&guide)[0].1, "older");
    assert_eq!(guide["skills"][0]["revision"], 1);
    let ran = place.in_repo(&["skill", "update"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        "Wrote .claude/skills/hushrun/SKILL.md.\nWrote .agents/skills/hushrun/SKILL.md.\n"
    );
    assert_ne!(place.read(COPIES[0]), one);
    let guide = place.guide();
    assert_eq!(states(&guide)[0].1, "current");
    assert_eq!(states(&guide)[1].1, "current");
}

// covers: C-24, C-29 (revision 2, as 0.12.0 to 0.17.1 wrote it, once revision 3 is out)
#[test]
fn a_copy_of_revision_2_is_older_and_skill_update_rewrites_it() {
    let place = Place::new();
    let two = std::fs::read_to_string(hushrun_testkit::testdata("skills/revisions/2.md")).unwrap();
    place.write(COPIES[0], &two);
    place.write(COPIES[1], &two);
    let guide = place.guide();
    assert!(guide["skillRevision"].as_u64().unwrap() >= 3, "{guide:#}");
    assert_eq!(states(&guide)[0].1, "older");
    assert_eq!(guide["skills"][0]["revision"], 2);
    let text = place.in_repo(&["guide"]);
    assert!(
        text.stdout.contains(
            "- .claude/skills/hushrun/SKILL.md: older, revision 2: `hushrun skill update` updates it"
        ),
        "{}",
        text.stdout
    );
    let ran = place.in_repo(&["skill", "update"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(place.read(COPIES[0]).contains("`hushrun build`"));
    let guide = place.guide();
    assert_eq!(states(&guide)[0].1, "current");
    assert_eq!(states(&guide)[1].1, "current");
}

// covers: C-29
#[test]
fn skill_update_leaves_an_edited_copy_and_a_newer_one() {
    let place = Place::new();
    place.in_repo(&["skill", "update"]);
    let current = place.read(COPIES[0]);
    let revision = place.guide()["skillRevision"].as_u64().unwrap();

    // Edited by hand: someone's choice, kept until --force.
    let edited = format!("{current}\nAlso run make lint.\n");
    place.write(COPIES[0], &edited);
    // From a teammate's newer hushrun: this one is the older.
    let newer = current.replacen(
        &format!("hushrun-skill: \"{revision}\""),
        "hushrun-skill: \"999\"",
        1,
    );
    place.write(COPIES[1], &newer);
    let guide = place.guide();
    assert_eq!(states(&guide)[0].1, "edited");
    assert_eq!(states(&guide)[1].1, "newer");
    assert_eq!(guide["skills"][1]["revision"], 999);

    let ran = place.in_repo(&["skill", "update"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(place.read(COPIES[0]), edited);
    assert_eq!(place.read(COPIES[1]), newer);
    assert!(
        ran.stderr.contains(
            "warning: left .claude/skills/hushrun/SKILL.md: it was edited by hand; --force replaces it"
        ),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains(&format!(
            "warning: left .agents/skills/hushrun/SKILL.md: a newer hushrun wrote it (skill revision 999, this hushrun's is {revision}): update hushrun with `hushrun update`"
        )),
        "{}",
        ran.stderr
    );

    let forced = place.in_repo(&["skill", "update", "--force"]);
    assert_eq!(forced.code, 0, "{}", forced.stderr);
    assert_eq!(place.read(COPIES[0]), current);
    assert_eq!(place.read(COPIES[1]), current);
}

// covers: C-29
#[test]
fn skill_update_global_writes_your_own_copies() {
    let place = Place::new();
    let ran = place.hushrun(&place.home, &["skill", "update", "--global"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        "Wrote ~/.claude/skills/hushrun/SKILL.md.\nWrote ~/.agents/skills/hushrun/SKILL.md.\n"
    );
    let claude =
        std::fs::read_to_string(place.home.join(".claude/skills/hushrun/SKILL.md")).unwrap();
    let agents =
        std::fs::read_to_string(place.home.join(".agents/skills/hushrun/SKILL.md")).unwrap();
    assert_eq!(claude, agents);
    // A copy a skills manager links to another keeps its link: the file it leads to is written.
    std::fs::remove_dir_all(place.home.join(".claude/skills/hushrun")).unwrap();
    std::os::unix::fs::symlink(
        place.home.join(".agents/skills/hushrun"),
        place.home.join(".claude/skills/hushrun"),
    )
    .unwrap();
    std::fs::write(
        place.home.join(".agents/skills/hushrun/SKILL.md"),
        before_revisions(),
    )
    .unwrap();
    let ran = place.hushrun(&place.home, &["skill", "update", "--global"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        std::fs::symlink_metadata(place.home.join(".claude/skills/hushrun"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(place.home.join(".agents/skills/hushrun/SKILL.md")).unwrap(),
        claude
    );
}

// covers: C-29
#[test]
fn skill_update_outside_a_repository_says_where_it_writes() {
    let place = Place::new();
    let ran = place.hushrun(&place.home, &["skill", "update"]);
    assert_eq!(ran.code, 2, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "this isn't a git repository: run it in one, or use --global for your own copies"
        ),
        "{}",
        ran.stderr
    );
}

// covers: C-22, C-24
#[test]
fn ready_says_when_the_repositorys_skill_is_older_or_newer() {
    let place = Place::new();
    // What hushrun 0.10 wrote: before revisions, so older than any revision.
    place.write(COPIES[0], &before_revisions());
    place.write(COPIES[1], &before_revisions());
    let guide = place.guide();
    assert_eq!(states(&guide)[0].1, "older");
    assert_eq!(guide["skills"][0]["revision"], 0);
    let ready = place.in_repo(&["ready", "--format", "json"]);
    assert_eq!(ready.code, 0, "{}", ready.stderr);
    let report: Value = serde_json::from_str(&ready.stdout).unwrap();
    schema::assert_valid(&schema::load("ready.v2.schema.json"), &report);
    assert_eq!(
        report["notes"],
        serde_json::json!([
            "This repository's hushrun skill is older than this hushrun's: `hushrun skill update` updates .claude/skills/hushrun/SKILL.md and .agents/skills/hushrun/SKILL.md, then commit them."
        ]),
        "{report:#}"
    );
    let text = place.in_repo(&["ready"]);
    assert!(
        text.stdout
            .contains("This repository's hushrun skill is older than this hushrun's"),
        "{}",
        text.stdout
    );

    // Current: nothing to say.
    place.in_repo(&["skill", "update"]);
    let ready = place.in_repo(&["ready", "--format", "json"]);
    let report: Value = serde_json::from_str(&ready.stdout).unwrap();
    assert_eq!(report["notes"], serde_json::json!([]), "{report:#}");

    // Newer: this hushrun is the one to update.
    let newer = place.read(COPIES[0]).replacen(
        &format!("hushrun-skill: \"{}\"", guide["skillRevision"]),
        "hushrun-skill: \"999\"",
        1,
    );
    place.write(COPIES[0], &newer);
    let ready = place.in_repo(&["ready", "--format", "json"]);
    let report: Value = serde_json::from_str(&ready.stdout).unwrap();
    assert_eq!(
        report["notes"],
        serde_json::json!([format!(
            "A newer hushrun than this one, {VERSION}, wrote .claude/skills/hushrun/SKILL.md: update hushrun with `hushrun update`."
        )]),
        "{report:#}"
    );
}

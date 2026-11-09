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

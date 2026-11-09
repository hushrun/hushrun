//! hushrun's skill for coding agents, and what is true of each copy of it (C-24, C-29,
//! \[ADR 0065\]).
//!
//! The skill holds what every hushrun does the same way, and `hushrun guide` prints the rest
//! from the binary, so a copy an agent reads stays right whichever hushrun is installed. Each
//! copy says its revision in its frontmatter, `metadata.hushrun-skill`, a number that goes up
//! only when the skill's text changes, so that releases don't rewrite every repository's copy.
//! [`state_of`] is the one place that decides what a copy is: `ready`, `doctor`, `guide`, `init`
//! and `skill update` all ask it.

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

/// The skill, as `hushrun init` and `hushrun skill update` write it.
pub const SKILL: &str = include_str!("../../../recipes/agents/SKILL.md");
/// What `hushrun guide` prints, before what is true here.
pub const GUIDE: &str = include_str!("../../../recipes/agents/GUIDE.md");
/// This hushrun's revision of the skill: [`SKILL`] says it too.
pub const REVISION: u32 = 1;

/// Each revision's text, by its SHA-256, with CRLF read as LF, as git may check it out on
/// Windows. Append only: a copy of an older revision is `older` only while its hash is here. A
/// test fails when [`SKILL`] changes and this doesn't.
const REVISIONS: &[(u32, &str)] = &[(
    1,
    "53a2c8f9cb5fd8427907b33985c044a05d6b40c7eeaf30c5b5f5dc0d08e7043f",
)];

/// The texts hushrun wrote before revisions, 0.3.0 to 0.10.1: revision 0. Their texts are in
/// `testdata/skills/before-revisions`, which a test hashes.
const BEFORE: &[&str] = &[
    "010e78ba59d4c3f0179c8a1cf2819cd2f5094dbe935392aa9589591f5dd62cd0",
    "3b0c49773ca428e1540d6e5c6c4d1279e08e10219968c64fed6df7b023c5924d",
    "9cac84fff9ddfcae3aa188a861fd5c910d4686d3dbe4718d7853b1579e08d9c3",
    "0c7c31ed8b5c0991e1c202abf1363e1d80a5a465b57daceb827c9f271a9db5be",
    "53d12b01a062203e05322f7e939630217a09207993dbc28635a4c91a71513f44",
    "19021ba4d139f33f4faf66b1fe4e83e85e175ab90bff500dd5a65f62cfe315ec",
    "8290aefa1c8e4b95158e3b5085fab324188fc0199f62859ed5a037aeb0ca5e2f",
    "14e96ad4227cee1f9bdc2be90930425174245c85eddbcf87d76d42ee62fa0b33",
    "381c6029551c8d99c45d76938e7aae8783207a5fcc95cfdbd3e5d42e90be7ad3",
];

/// Where a repository's copies go: Claude Code reads the first, Codex the second, and most
/// other agents either.
pub const IN_REPOSITORY: [&str; 2] = [
    ".claude/skills/hushrun/SKILL.md",
    ".agents/skills/hushrun/SKILL.md",
];
/// Where a person's own copies go, under their home directory.
pub const PERSONAL: [&str; 2] = IN_REPOSITORY;

/// What a copy of the skill is, next to this hushrun's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// This hushrun's revision, as it wrote it.
    Current,
    /// An older revision, as an older hushrun wrote it: this hushrun updates it.
    Older(u32),
    /// A newer revision: a newer hushrun wrote it, and this one is the one to update.
    Newer(u32),
    /// A text no hushrun wrote: someone edited it, and it is theirs.
    Edited,
    /// No copy.
    Absent,
}

impl State {
    /// The word for it, as `hushrun guide --format json` says it.
    pub fn word(self) -> &'static str {
        match self {
            State::Current => "current",
            State::Older(_) => "older",
            State::Newer(_) => "newer",
            State::Edited => "edited",
            State::Absent => "absent",
        }
    }

    /// The revision the copy says, when it is one hushrun knows.
    pub fn revision(self) -> Option<u32> {
        match self {
            State::Current => Some(REVISION),
            State::Older(n) | State::Newer(n) => Some(n),
            State::Edited | State::Absent => None,
        }
    }
}

/// What `text`, a copy's contents, is; `None` for no copy.
pub fn state_of(text: Option<&str>) -> State {
    let Some(text) = text else {
        return State::Absent;
    };
    let hash = hash(text);
    match stamp(text) {
        Some(Ok(n)) if n > REVISION => State::Newer(n),
        Some(Ok(n)) if REVISIONS.contains(&(n, hash.as_str())) => match n == REVISION {
            true => State::Current,
            false => State::Older(n),
        },
        Some(_) => State::Edited,
        None if BEFORE.contains(&hash.as_str()) => State::Older(0),
        None => State::Edited,
    }
}

/// What the file at `path` is. One hushrun can't read, such as a directory, is someone's.
pub fn read(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Ok(text) => state_of(Some(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => State::Absent,
        Err(_) => State::Edited,
    }
}

/// The revision a copy's frontmatter says, `metadata.hushrun-skill`: none without one, and an
/// error for one that isn't a number.
fn stamp(text: &str) -> Option<Result<u32, ()>> {
    let text = text.replace("\r\n", "\n");
    let front = text.strip_prefix("---\n")?.split("\n---").next()?;
    front.lines().find_map(|line| {
        let value = line.trim().strip_prefix("hushrun-skill:")?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        Some(value.parse::<u32>().map_err(|_| ()))
    })
}

/// SHA-256 of `text`, in hex, CRLF read as LF.
fn hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.replace("\r\n", "\n").as_bytes()))
}

/// A copy of the skill hushrun looks at, and what it is.
#[derive(Debug, Clone)]
pub struct Copy {
    /// How it is shown: relative to the repository, or `~/…`.
    pub shown: String,
    /// Whether it is a person's own, under their home, rather than the repository's.
    pub personal: bool,
    pub state: State,
}

/// The repository's copies at `root`, then the person's under `home`.
pub fn copies(root: Option<&Path>, home: Option<&Path>) -> Vec<Copy> {
    let mut copies = Vec::new();
    if let Some(root) = root {
        copies.extend(IN_REPOSITORY.iter().map(|relative| Copy {
            shown: (*relative).to_string(),
            state: read(&root.join(relative)),
            personal: false,
        }));
    }
    if let Some(home) = home {
        copies.extend(PERSONAL.iter().map(|relative| Copy {
            shown: format!("~/{relative}"),
            state: read(&home.join(relative)),
            personal: true,
        }));
    }
    copies
}

/// The person's home directory, where their own copies go.
pub fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// What [`write`] did with a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Wrote,
    /// It was this hushrun's already.
    Current,
    /// Left as it was: someone edited it, or a newer hushrun wrote it.
    Left(State),
}

/// Writes this hushrun's skill at `path`, unless it is there already, or, without `force`,
/// someone edited it or a newer hushrun wrote it. A link, as a skills manager makes, keeps
/// leading where it did: the file it leads to is written, whole, through a temporary file.
pub fn write(path: &Path, force: bool) -> anyhow::Result<Written> {
    let state = read(path);
    match state {
        State::Current => return Ok(Written::Current),
        State::Edited | State::Newer(_) if !force => return Ok(Written::Left(state)),
        _ => {}
    }
    let target = match std::fs::canonicalize(path) {
        Ok(target) => target,
        Err(_) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            path.to_path_buf()
        }
    };
    crate::cmd::setup::write(&target, SKILL)?;
    Ok(Written::Wrote)
}

/// What `hushrun ready` notes of the repository's copies at `root` (C-22): older ones, which
/// `skill update` updates, and newer ones, which say to update hushrun. `hushrun` is how this
/// hushrun is started, and `update` how it is updated.
pub fn notes(root: &Path, hushrun: &str, update: &str) -> Vec<String> {
    let copies = copies(Some(root), None);
    let older: Vec<&str> = copies
        .iter()
        .filter(|c| matches!(c.state, State::Older(_)))
        .map(|c| c.shown.as_str())
        .collect();
    let newer: Vec<&str> = copies
        .iter()
        .filter(|c| matches!(c.state, State::Newer(_)))
        .map(|c| c.shown.as_str())
        .collect();
    let mut notes = Vec::new();
    if !older.is_empty() {
        let them = if older.len() == 1 { "it" } else { "them" };
        notes.push(format!(
            "This repository's hushrun skill is older than this hushrun's: `{hushrun} skill update` updates {}, then commit {them}.",
            listed(&older)
        ));
    }
    if !newer.is_empty() {
        notes.push(format!(
            "A newer hushrun than this one, {}, wrote {}: update hushrun with `{update}`.",
            crate::releases::current(),
            listed(&newer)
        ));
    }
    notes
}

/// `a`, `a and b`, `a, b and c`.
pub fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-24
    #[test]
    fn a_change_to_the_skill_raises_its_revision() {
        // [ADR 0065] The revision is what tells a copy older from current: a new text under the
        // same number would make every older copy read as edited, and stay as it is.
        assert_eq!(
            stamp(SKILL),
            Some(Ok(REVISION)),
            "SKILL.md's hushrun-skill must say {REVISION}"
        );
        let (last, last_hash) = *REVISIONS.last().unwrap();
        assert_eq!(
            last, REVISION,
            "REVISIONS ends with this hushrun's revision"
        );
        assert_eq!(
            hash(SKILL),
            last_hash,
            "recipes/agents/SKILL.md changed: raise REVISION and its hushrun-skill to {}, and add its hash to REVISIONS",
            REVISION + 1
        );
        let numbers: Vec<u32> = REVISIONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, (1..=REVISION).collect::<Vec<_>>());
        assert_eq!(state_of(Some(SKILL)), State::Current);
        assert_eq!(state_of(Some(&SKILL.replace('\n', "\r\n"))), State::Current);
    }

    // covers: C-24
    #[test]
    fn every_skill_hushrun_wrote_before_revisions_is_older() {
        let dir = hushrun_testkit::testdata("skills/before-revisions");
        let mut hashes: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| {
                let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
                assert_eq!(state_of(Some(&text)), State::Older(0));
                hash(&text)
            })
            .collect();
        hashes.sort();
        let mut known: Vec<String> = BEFORE.iter().map(|h| (*h).to_string()).collect();
        known.sort();
        assert_eq!(hashes, known);
    }

    // covers: C-24
    #[test]
    fn a_copy_is_one_of_five() {
        assert_eq!(state_of(None), State::Absent);
        let stamped = |n: &str| {
            SKILL.replacen(
                &format!("hushrun-skill: \"{REVISION}\""),
                &format!("hushrun-skill: \"{n}\""),
                1,
            )
        };
        assert_eq!(state_of(Some(&stamped("7"))), State::Newer(7));
        assert_eq!(state_of(Some(&stamped("seven"))), State::Edited);
        // This revision's number with another text: someone's edit.
        assert_eq!(
            state_of(Some(&format!("{SKILL}\nAlso run make lint.\n"))),
            State::Edited
        );
        // An older revision's number with a text it never had: edited too.
        assert_eq!(state_of(Some(&stamped("0"))), State::Edited);
        assert_eq!(state_of(Some("# my notes\n")), State::Edited);
        assert_eq!(State::Older(0).revision(), Some(0));
        assert_eq!(State::Edited.revision(), None);
    }

    // covers: C-29
    #[test]
    fn writing_keeps_what_isnt_this_hushrun_s_to_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/SKILL.md");
        assert_eq!(write(&path, false).unwrap(), Written::Wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);
        assert_eq!(write(&path, false).unwrap(), Written::Current);
        std::fs::write(&path, "# mine\n").unwrap();
        assert_eq!(write(&path, false).unwrap(), Written::Left(State::Edited));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
        assert_eq!(write(&path, true).unwrap(), Written::Wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);
    }
}

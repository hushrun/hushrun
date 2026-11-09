//! \[ADR 0051\] Which package.json scripts a CI line starts: the root's, and its workspace
//! packages', through a package manager, a workspace's options, Turborepo, Nx or Lerna, and the
//! scripts those start in turn (C-15).

use std::collections::{BTreeMap, BTreeSet};

use super::workspaces::Workspace;
use super::{manager, scripts};

/// A script of a package: its directory, `""` for the root's, and its name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ran {
    pub dir: String,
    pub script: String,
}

impl Ran {
    pub fn new(dir: &str, script: &str) -> Ran {
        Ran {
            dir: dir.to_string(),
            script: script.to_string(),
        }
    }

    /// The package.json it is in, from the root.
    pub fn package(&self) -> String {
        match self.dir.as_str() {
            "" => "package.json".to_string(),
            dir => format!("{dir}/package.json"),
        }
    }
}

/// The scripts of the root's package.json and of its workspace's packages.
#[derive(Debug, Default)]
pub struct Scripts<'a> {
    /// Each package's scripts and their commands, by its directory: `""` for the root.
    pub commands: BTreeMap<String, BTreeMap<String, String>>,
    pub workspace: Option<&'a Workspace>,
}

impl Scripts<'_> {
    /// The scripts `commands`, a shell line run in `dir`, starts, and those they start in turn:
    /// only scripts there are.
    pub fn started(&self, commands: &str, dir: &str) -> BTreeSet<Ran> {
        self.started_under(commands, dir).into_keys().collect()
    }

    /// [`Scripts::started`], each with whether some way it starts runs it under Turborepo, which
    /// gives its tasks, and all they start, only the variables turbo.json passes on.
    pub fn started_under(&self, commands: &str, dir: &str) -> BTreeMap<Ran, bool> {
        let mut found: BTreeMap<Ran, bool> = BTreeMap::new();
        let mut next = self.direct(commands, dir, false);
        while let Some((ran, turbo)) = next.pop() {
            let Some(command) = self.command(&ran) else {
                continue;
            };
            // Once under turbo, what it starts is too: a script seen outside it is read again.
            match found.get(&ran) {
                Some(&seen) if seen || !turbo => continue,
                _ => {}
            }
            found.insert(ran.clone(), turbo);
            next.extend(self.direct(command, &ran.dir, turbo));
        }
        found
    }

    fn command(&self, ran: &Ran) -> Option<&String> {
        self.commands.get(&ran.dir)?.get(&ran.script)
    }

    /// What `commands` starts itself, each with whether it runs under turbo: `turbo` says the
    /// line does.
    fn direct(&self, commands: &str, dir: &str, turbo: bool) -> Vec<(Ran, bool)> {
        let mut found = Vec::new();
        for (here, words) in scripts::simple_commands(commands, dir) {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            if let Some(runs) = self.workspace.and_then(|ws| ws.fans_out(&words)) {
                let under = turbo || runs.turbo;
                for script in &runs.scripts {
                    found.extend(
                        runs.packages
                            .iter()
                            .map(|package| (Ran::new(package, script), under)),
                    );
                    if runs.root {
                        found.push((Ran::new("", script), under));
                    }
                }
                continue;
            }
            if let (Some(script), Some(package)) =
                (manager::script_of(&words), self.package_at(&here))
            {
                found.push((Ran::new(&package, &script), turbo));
            }
        }
        found
    }

    /// The package a command run in `here` is in: the deepest of the workspace that holds it,
    /// else the root's, at the root.
    fn package_at(&self, here: &str) -> Option<String> {
        match self.workspace.and_then(|ws| ws.member_of(here)) {
            Some(member) => Some(member.dir.clone()),
            None => here.is_empty().then(String::new),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::setup::workspaces::Member;

    fn workspace() -> Workspace {
        let member = |dir: &str, name: &str| Member {
            dir: dir.to_string(),
            name: Some(name.to_string()),
            text: String::new(),
            needs: BTreeSet::new(),
        };
        Workspace {
            file: "pnpm-workspace.yaml".to_string(),
            members: vec![member("packages/a", "a"), member("packages/b", "b")],
            ..Workspace::default()
        }
    }

    fn scripts(ws: &Workspace) -> Scripts<'_> {
        let of = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(name, command)| (name.to_string(), command.to_string()))
                .collect()
        };
        Scripts {
            commands: BTreeMap::from([
                (
                    String::new(),
                    of(&[
                        ("test", "pnpm -r test"),
                        ("ci", "pnpm lint && pnpm test"),
                        ("lint", "eslint ."),
                    ]),
                ),
                (
                    "packages/a".to_string(),
                    of(&[("test", "pnpm build && vitest run"), ("build", "tsc")]),
                ),
                ("packages/b".to_string(), of(&[("test", "vitest run")])),
            ]),
            workspace: Some(ws),
        }
    }

    fn names(found: &BTreeSet<Ran>) -> Vec<String> {
        found
            .iter()
            .map(|ran| format!("{}:{}", ran.dir, ran.script))
            .collect()
    }

    // covers: C-15
    #[test]
    fn a_root_script_that_fans_out_starts_each_packages_script() {
        let ws = workspace();
        let scripts = scripts(&ws);
        assert_eq!(
            names(&scripts.started("pnpm install\npnpm test", "")),
            [
                ":test",
                "packages/a:build",
                "packages/a:test",
                "packages/b:test"
            ]
        );
        assert_eq!(
            names(&scripts.started("pnpm run ci", "")),
            [
                ":ci",
                ":lint",
                ":test",
                "packages/a:build",
                "packages/a:test",
                "packages/b:test"
            ]
        );
        assert_eq!(
            names(&scripts.started("pnpm --filter b test", "")),
            ["packages/b:test"]
        );
        assert_eq!(
            names(&scripts.started("cd packages/a && npm test", "")),
            ["packages/a:build", "packages/a:test"]
        );
        assert_eq!(
            names(&scripts.started("npm test", "packages/b")),
            ["packages/b:test"]
        );
        assert_eq!(
            names(&scripts.started("hushrun run t -- pnpm --filter a test", "")),
            ["packages/a:build", "packages/a:test"]
        );
        assert!(scripts.started("cd \"$DIR\" && npm test", "").is_empty());
        assert!(scripts.started("npm test", "docs").is_empty());
        assert!(
            scripts
                .started("pnpm --filter b nothing-here", "")
                .is_empty()
        );
    }

    // covers: C-15
    #[test]
    fn what_turbo_starts_runs_under_it() {
        let ws = workspace();
        let mut scripts = scripts(&ws);
        let root = scripts.commands.get_mut("").unwrap();
        root.insert("turbo".into(), "turbo run test".into());
        root.insert("loose".into(), "turbo run test --env-mode=loose".into());
        let under = |line: &str| -> Vec<String> {
            scripts
                .started_under(line, "")
                .into_iter()
                .map(|(ran, turbo)| {
                    let mark = if turbo { " (turbo)" } else { "" };
                    format!("{}:{}{mark}", ran.dir, ran.script)
                })
                .collect()
        };
        assert_eq!(
            under("pnpm run turbo"),
            [
                ":turbo",
                "packages/a:build (turbo)",
                "packages/a:test (turbo)",
                "packages/b:test (turbo)"
            ]
        );
        assert_eq!(
            under("pnpm run loose"),
            [
                ":loose",
                "packages/a:build",
                "packages/a:test",
                "packages/b:test"
            ]
        );
        // Started both ways, it runs under turbo in one.
        assert_eq!(
            under("pnpm --filter b test && npx turbo run test --filter=b"),
            ["packages/b:test (turbo)"]
        );
        assert_eq!(
            under("npx turbo run test --filter=b && pnpm --filter b test"),
            ["packages/b:test (turbo)"]
        );
        assert_eq!(
            under("pnpm test"),
            [
                ":test",
                "packages/a:build",
                "packages/a:test",
                "packages/b:test"
            ]
        );
    }
}

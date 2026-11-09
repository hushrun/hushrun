//! The package manager a repository uses (C-15): how it installs hushrun, how it runs a script,
//! and which commands run one.

use std::path::{Path, PathBuf};

/// A JavaScript package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Manager {
    pub fn name(self) -> &'static str {
        match self {
            Manager::Npm => "npm",
            Manager::Pnpm => "pnpm",
            Manager::Yarn => "yarn",
            Manager::Bun => "bun",
        }
    }

    fn named(name: &str) -> Option<Manager> {
        Some(match name {
            "npm" => Manager::Npm,
            "pnpm" => Manager::Pnpm,
            "yarn" => Manager::Yarn,
            "bun" => Manager::Bun,
            _ => return None,
        })
    }
}

/// The package manager, and what said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub manager: Manager,
    /// `packageManager`, a lockfile's name, or nothing when npm is the default.
    pub because: Option<String>,
    /// The repository root is a workspace's: pnpm and Yarn 1 add to it only when told so.
    workspace_root: bool,
    /// Yarn 1, whose flag for that differs.
    yarn_classic: bool,
}

/// The lockfiles, in the order they decide.
const LOCKFILES: &[(&str, Manager)] = &[
    ("pnpm-lock.yaml", Manager::Pnpm),
    ("yarn.lock", Manager::Yarn),
    ("bun.lock", Manager::Bun),
    ("bun.lockb", Manager::Bun),
    ("package-lock.json", Manager::Npm),
];

/// The package manager of the package.json at `root`: `packageManager`, such as `pnpm@9.1.0`,
/// else its lockfile, else npm. `workspaces` is whether package.json lists workspaces.
pub fn detect(root: &Path, package_manager: Option<&str>, workspaces: bool) -> Detected {
    let declared = package_manager.and_then(|text| {
        let name = text.split('@').next().unwrap_or_default().trim();
        Manager::named(name).map(|manager| (manager, text.trim()))
    });
    let (manager, because) = match declared {
        Some((manager, text)) => (manager, Some(format!("packageManager {text}"))),
        None => match LOCKFILES.iter().find(|(file, _)| root.join(file).is_file()) {
            Some((file, manager)) => (*manager, Some((*file).to_string())),
            None => (Manager::Npm, None),
        },
    };
    let yarn_classic = manager == Manager::Yarn
        && match declared {
            Some((_, text)) => text.starts_with("yarn@1."),
            None => std::fs::read_to_string(root.join("yarn.lock"))
                .is_ok_and(|lock| lock.contains("# yarn lockfile v1")),
        };
    let workspace_root = match manager {
        Manager::Pnpm => root.join("pnpm-workspace.yaml").is_file(),
        Manager::Yarn => yarn_classic && workspaces,
        Manager::Npm | Manager::Bun => false,
    };
    Detected {
        manager,
        because,
        workspace_root,
        yarn_classic,
    }
}

impl Detected {
    /// \[ADR 0051\] Whether a workspace package's scripts find only its own dependencies'
    /// programs, and not the root's: Yarn from 2 on.
    pub fn own_programs(&self) -> bool {
        self.manager == Manager::Yarn && !self.yarn_classic
    }

    /// The script this package manager runs after installing the repository's dependencies: Yarn
    /// from 2 on doesn't run `prepare`.
    pub fn after_install(&self) -> &'static str {
        if self.manager == Manager::Yarn && !self.yarn_classic {
            "postinstall"
        } else {
            "prepare"
        }
    }

    /// The command that adds `spec` to devDependencies and installs it.
    pub fn add(&self, spec: &str) -> Vec<String> {
        let mut args: Vec<&str> = match self.manager {
            Manager::Npm => vec!["npm", "install", "--save-dev"],
            Manager::Pnpm => vec!["pnpm", "add", "--save-dev"],
            Manager::Yarn => vec!["yarn", "add", "--dev"],
            Manager::Bun => vec!["bun", "add", "--dev"],
        };
        if self.workspace_root {
            args.push(if self.yarn_classic { "-W" } else { "-w" });
        }
        args.push(spec);
        args.into_iter().map(str::to_string).collect()
    }

    /// The command that runs the script `name`.
    pub fn run(&self, name: &str) -> Vec<String> {
        vec![self.manager.name().into(), "run".into(), name.into()]
    }
}

/// The script `words`, a command, runs through a package manager in its own package: `test`
/// for `npm test`, `npm t`, `pnpm test` or `yarn test`, `lint` for `npm run lint`,
/// `npm run-script lint`, `pnpm lint`, `pnpm run lint`, `yarn lint`, `yarn run lint` or
/// `bun run lint`. `bun test` is bun's own test runner. Options that pick another package or
/// directory, or that init doesn't know, give none.
pub fn script_of(words: &[&str]) -> Option<String> {
    let (program, rest) = words.split_first()?;
    let manager = Manager::named(program)?;
    let mut rest = rest.iter().copied();
    let mut next = || rest.by_ref().find(|word| !harmless(word));
    let first = next()?;
    if first.starts_with('-') {
        return None;
    }
    let script = match (manager, first) {
        (Manager::Npm, "test" | "t" | "tst") => "test",
        (Manager::Npm, "run" | "run-script" | "rum" | "urn") => next()?,
        (Manager::Npm, _) => return None,
        (Manager::Pnpm, "t" | "tst") => "test",
        (Manager::Pnpm | Manager::Yarn | Manager::Bun, "run") => next()?,
        (Manager::Bun, _) => return None,
        (Manager::Pnpm | Manager::Yarn, script) => script,
    };
    (!script.starts_with('-')).then(|| script.to_string())
}

/// Options that change only what a package manager prints.
fn harmless(word: &str) -> bool {
    matches!(
        word,
        "-s" | "--silent" | "-q" | "--quiet" | "--if-present" | "--color" | "--no-color"
    ) || word.starts_with("--loglevel=")
        || word.starts_with("--reporter=")
}

/// What starts `name`. On Windows, `Command` tries a bare name with `.exe` only, and npm,
/// pnpm and yarn are `.cmd` files: a name is looked up as a shell would, in each PATH
/// directory with each of PATHEXT's extensions that `Command` can start.
#[cfg(windows)]
pub fn program(name: &str) -> PathBuf {
    const STARTABLE: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let exts: Vec<String> = pathext
        .split(';')
        .map(str::to_ascii_lowercase)
        .filter(|ext| STARTABLE.contains(&ext.as_str()))
        .collect();
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path).filter(|dir| !dir.as_os_str().is_empty()) {
        for ext in &exts {
            let file = dir.join(format!("{name}{ext}"));
            if std::fs::symlink_metadata(&file).is_ok_and(|meta| !meta.is_dir()) {
                return file;
            }
        }
    }
    name.into()
}

/// What starts `name`: `Command` looks a bare name up in PATH as a shell would.
#[cfg(not(windows))]
pub fn program(name: &str) -> PathBuf {
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(
        files: &[(&str, &str)],
        package_manager: Option<&str>,
        workspaces: bool,
    ) -> Detected {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        detect(dir.path(), package_manager, workspaces)
    }

    // covers: C-15
    #[test]
    fn package_json_then_lockfiles_name_the_package_manager() {
        let none = detected(&[], None, false);
        assert_eq!((none.manager, none.because), (Manager::Npm, None));
        for (file, manager) in LOCKFILES {
            let found = detected(&[(file, "")], None, false);
            assert_eq!(found.manager, *manager, "{file}");
            assert_eq!(found.because.as_deref(), Some(*file));
        }
        // pnpm's lockfile wins over npm's, and packageManager over both.
        let both = detected(
            &[("package-lock.json", ""), ("pnpm-lock.yaml", "")],
            None,
            false,
        );
        assert_eq!(both.manager, Manager::Pnpm);
        let declared = detected(
            &[("package-lock.json", "")],
            Some("yarn@4.1.0+sha512.abc"),
            false,
        );
        assert_eq!(declared.manager, Manager::Yarn);
        assert_eq!(
            declared.because.as_deref(),
            Some("packageManager yarn@4.1.0+sha512.abc")
        );
        // An unknown name falls back to the lockfiles.
        assert_eq!(
            detected(&[("bun.lockb", "")], Some("deno@2"), false).manager,
            Manager::Bun
        );
    }

    // covers: C-15
    #[test]
    fn each_package_manager_installs_hushrun_its_own_way() {
        /// Files, packageManager, workspaces, and the command.
        type Case<'a> = (&'a [(&'a str, &'a str)], Option<&'a str>, bool, &'a str);
        let spec = "hushrun@^0.1.0";
        let cases: [Case<'_>; 7] = [
            (&[], None, false, "npm install --save-dev hushrun@^0.1.0"),
            (
                &[("pnpm-lock.yaml", "")],
                None,
                false,
                "pnpm add --save-dev hushrun@^0.1.0",
            ),
            (
                &[
                    ("pnpm-lock.yaml", ""),
                    ("pnpm-workspace.yaml", "packages: [web]\n"),
                ],
                None,
                false,
                "pnpm add --save-dev -w hushrun@^0.1.0",
            ),
            (
                &[("yarn.lock", "__metadata:\n")],
                None,
                true,
                "yarn add --dev hushrun@^0.1.0",
            ),
            (
                &[("yarn.lock", "# yarn lockfile v1\n")],
                None,
                true,
                "yarn add --dev -W hushrun@^0.1.0",
            ),
            (
                &[],
                Some("yarn@1.22.19"),
                true,
                "yarn add --dev -W hushrun@^0.1.0",
            ),
            (
                &[("bun.lock", "")],
                None,
                true,
                "bun add --dev hushrun@^0.1.0",
            ),
        ];
        for (files, declared, workspaces, expected) in cases {
            assert_eq!(
                detected(files, declared, workspaces).add(spec).join(" "),
                expected
            );
        }
        assert_eq!(
            detected(&[("pnpm-lock.yaml", "")], None, false).run("lint"),
            ["pnpm", "run", "lint"]
        );
    }

    #[test]
    fn script_runs_through_each_package_manager() {
        let script = |line: &str| script_of(&line.split_whitespace().collect::<Vec<_>>());
        for (line, expected) in [
            ("npm test", Some("test")),
            ("npm t", Some("test")),
            ("npm run lint", Some("lint")),
            ("npm run-script lint", Some("lint")),
            ("npm --silent run lint", Some("lint")),
            ("npm ci", None),
            ("npm run", None),
            ("pnpm lint", Some("lint")),
            ("pnpm run lint", Some("lint")),
            ("pnpm t", Some("test")),
            ("yarn lint", Some("lint")),
            ("yarn run lint", Some("lint")),
            ("bun run lint", Some("lint")),
            ("bun test", None),
            ("bun lint", None),
            ("npm -w web test", None),
            ("yarn --cwd web lint", None),
            ("npx lint", None),
        ] {
            assert_eq!(script(line).as_deref(), expected, "{line}");
        }
    }
}

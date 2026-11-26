//! The `bundler` part: the `.gem` files Bundler downloads, seeded (K-10's C) into the gem cache
//! Bundler already uses, `<Bundler.bundle_path>/cache`, so that `bundle install` finds each one
//! there and installs it to the same paths it would after a download. hushrun sets no variable:
//! not `GEM_HOME`, `GEM_PATH`, `BUNDLE_PATH`, `RUBYOPT` or `RUBYLIB`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hushrun_core::blocks::Hex32;

use crate::part::{Env, Fetch, Part, Reader, Seed, SeedFile, file_name};
use crate::store::VerifiedBy;

/// The part's folder under which the `.gem` files are.
pub const CACHE: &str = "cache";

/// The `bundler` part.
pub struct Bundler;

impl Part for Bundler {
    fn name(&self) -> &'static str {
        "bundler"
    }

    fn tool(&self) -> &'static str {
        "Bundler"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                let name = file_name(path);
                matches!(name, "Gemfile" | "Gemfile.lock" | "gems.rb" | "gems.locked")
                    || name.ends_with(".gemspec")
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        // A gem home (`GEM_HOME`, Bundler's path itself) holds `cache/`; a `BUNDLE_PATH` such as
        // `vendor/bundle` holds `ruby/<version>/cache/`.
        let direct = cache.join(CACHE);
        if direct.is_dir() {
            return Some((direct, CACHE));
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir(cache.join("ruby"))
            .ok()?
            .flatten()
            .map(|e| e.path().join(CACHE))
            .filter(|p| p.is_dir())
            .collect();
        found.sort();
        found.pop().map(|p| (p, CACHE))
    }

    fn keep(&self, path: &str) -> bool {
        // Only the downloads: `cache/bundler/git` (git sources' clones) and anything else is out.
        !path.contains('/') && !path.starts_with('.') && path.ends_with(".gem")
    }

    fn package(&self, path: &str) -> Option<String> {
        // <name>-<version>[-<platform>].gem; a version starts with a digit, a name's words rarely do.
        let stem = path.strip_suffix(".gem")?;
        let at = stem
            .match_indices('-')
            .find(|(i, _)| stem[i + 1..].starts_with(|c: char| c.is_ascii_digit()))?
            .0;
        Some(format!("{} ({})", &stem[..at], &stem[at + 1..]))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn fetch(&self, checkout: &Path, manifests: &[String], cache: &Path) -> Vec<Fetch> {
        // `bundle cache --no-install` downloads each gem the lockfile names and installs none, so
        // no gem's install script runs. Bundler's path and its app cache are the bake's own (the
        // app cache is `vendor/cache` in the checkout otherwise), and the lockfile is frozen.
        manifests
            .iter()
            .filter_map(|lock| {
                let gemfile = match file_name(lock) {
                    "Gemfile.lock" => "Gemfile",
                    "gems.locked" => "gems.rb",
                    _ => return None,
                };
                let dir = checkout.join(lock).parent()?.to_path_buf();
                Some((dir.join(gemfile), dir))
            })
            .enumerate()
            .map(|(i, (gemfile, dir))| Fetch {
                dir,
                argv: ["bundle", "cache", "--no-install"]
                    .map(String::from)
                    .to_vec(),
                env: vec![
                    ("BUNDLE_GEMFILE".into(), gemfile.into_os_string()),
                    ("BUNDLE_PATH".into(), cache.as_os_str().to_owned()),
                    (
                        "BUNDLE_CACHE_PATH".into(),
                        cache.join(format!(".app-{i}")).into_os_string(),
                    ),
                    ("BUNDLE_GLOBAL_GEM_CACHE".into(), "false".into()),
                    ("BUNDLE_FROZEN".into(), "true".into()),
                ],
            })
            .collect()
    }

    fn verified_by_for(&self, manifests: &[(String, String)]) -> VerifiedBy {
        let mut locks = lockfiles(manifests).peekable();
        if locks.peek().is_none() {
            return VerifiedBy::Signature;
        }
        if locks.all(|(_, text)| has_checksums(text)) {
            VerifiedBy::Lockfile
        } else {
            VerifiedBy::Signature
        }
    }

    fn locked(&self, manifests: &[(String, String)]) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (_, text) in lockfiles(manifests) {
            for gem in gems(text) {
                let Some(locked) = gem.locked else { continue };
                // Two lockfiles that hash one file differently: neither is its hash.
                out.entry(gem.file)
                    .and_modify(|had| {
                        if had.as_deref() != Some(locked.as_str()) {
                            *had = None;
                        }
                    })
                    .or_insert(Some(locked));
            }
        }
        out.into_iter()
            .filter_map(|(file, locked)| Some((file, locked?)))
            .collect()
    }

    fn reader(&self, _dir: &Path, checkout: &Path, env: Env<'_>) -> Reader {
        let start = std::env::current_dir().unwrap_or_else(|_| checkout.to_path_buf());
        let Some((gemfile_dir, lockfile)) = lockfile_for(&start, checkout, env) else {
            return Reader::LeftAlone("no Gemfile here".into());
        };
        let Ok(text) = std::fs::read_to_string(&lockfile) else {
            return Reader::LeftAlone(format!("{} can't be read", lockfile.display()));
        };
        let files = seed_files(&text);
        if files.is_empty() {
            return Reader::LeftAlone(format!("{} names no gem", lockfile.display()));
        }
        match cache_dir(&gemfile_dir) {
            Ok(into) => Reader::Seed(Seed { into, files }),
            Err(why) => Reader::LeftAlone(why),
        }
    }
}

/// The lockfiles among a manifest set's files (path and text).
fn lockfiles(manifests: &[(String, String)]) -> impl Iterator<Item = &(String, String)> {
    manifests
        .iter()
        .filter(|(path, _)| matches!(file_name(path), "Gemfile.lock" | "gems.locked"))
}

/// Whether a lockfile has a `CHECKSUMS` section (Bundler 2.5 with `bundle lock
/// --add-checksums`; every new lockfile from Bundler 4).
pub fn has_checksums(text: &str) -> bool {
    text.lines().any(|l| l == "CHECKSUMS")
}

/// A gem a lockfile names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    /// Its `.gem` file's name: `<name>-<version>[-<platform>].gem`.
    pub file: String,
    /// Its hash as the lockfile's `CHECKSUMS` writes it (`sha256=<hex>`), when it has one.
    pub locked: Option<String>,
}

/// The gems a lockfile's `GEM` sections name (not `GIT` or `PATH`: those are no download), each
/// once, with the hash `CHECKSUMS` gives it.
pub fn gems(text: &str) -> Vec<Locked> {
    let mut section = "";
    let mut in_specs = false;
    let mut specs: Vec<String> = Vec::new();
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        if !line.starts_with(' ') {
            section = line.trim_end();
            in_specs = false;
            continue;
        }
        match section {
            "GEM" => {
                if line.trim_end() == "  specs:" {
                    in_specs = true;
                } else if in_specs
                    && let Some(spec) = line.strip_prefix("    ")
                    && !spec.starts_with(' ')
                    && let Some((file, _)) = gem_file(spec)
                {
                    specs.push(file);
                }
            }
            "CHECKSUMS" => {
                if let Some(entry) = line.strip_prefix("  ")
                    && let Some((file, rest)) = gem_file(entry)
                    && let Some(sum) = rest.split(',').map(str::trim).find(|s| {
                        s.strip_prefix("sha256=")
                            .is_some_and(|hex| Hex32::parse(hex).is_some())
                    })
                {
                    sums.insert(file, sum.to_string());
                }
            }
            _ => {}
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    specs
        .into_iter()
        .filter(|f| seen.insert(f.clone()))
        .map(|file| Locked {
            locked: sums.get(&file).cloned(),
            file,
        })
        .collect()
}

/// `name (version)` and what follows it: the `.gem` file's name and the rest. `None` for a name
/// or version that can't be one file's name.
fn gem_file(entry: &str) -> Option<(String, &str)> {
    let (name, rest) = entry.split_once(" (")?;
    let (version, rest) = rest.split_once(')')?;
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && !s
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\'))
    };
    (plain(name) && plain(version)).then(|| (format!("{name}-{version}.gem"), rest.trim()))
}

/// What a run seeds from a lockfile: every gem it names, from the part's `cache/` folder, with the
/// hash its `CHECKSUMS` gives.
pub fn seed_files(lockfile: &str) -> Vec<SeedFile> {
    gems(lockfile)
        .into_iter()
        .map(|gem| SeedFile {
            path: format!("{CACHE}/{}", gem.file),
            sha256: gem
                .locked
                .as_deref()
                .and_then(|l| l.strip_prefix("sha256="))
                .and_then(Hex32::parse),
            name: gem.file,
        })
        .collect()
}

/// The folder of the Gemfile Bundler reads from `start` and its lockfile: `BUNDLE_GEMFILE`'s, or
/// the nearest `Gemfile` or `gems.rb` from `start` up to the checkout's root.
pub fn lockfile_for(start: &Path, checkout: &Path, env: Env<'_>) -> Option<(PathBuf, PathBuf)> {
    if let Some(gemfile) = env("BUNDLE_GEMFILE").filter(|v| !v.is_empty()) {
        let gemfile = start.join(gemfile);
        let lock = if gemfile.file_name().is_some_and(|n| n == "gems.rb") {
            gemfile.with_file_name("gems.locked")
        } else {
            let mut name = gemfile.clone().into_os_string();
            name.push(".lock");
            PathBuf::from(name)
        };
        return Some((gemfile.parent()?.to_path_buf(), lock));
    }
    let mut dir = if start.starts_with(checkout) {
        start
    } else {
        checkout
    };
    loop {
        for (gemfile, lock) in [("Gemfile", "Gemfile.lock"), ("gems.rb", "gems.locked")] {
            if dir.join(gemfile).is_file() {
                return Some((dir.to_path_buf(), dir.join(lock)));
            }
        }
        if dir == checkout {
            return None;
        }
        dir = dir.parent()?;
    }
}

/// What Ruby prints: `cache=<folder>`, or `global` when Bundler's global gem cache is on.
const QUERY: &str = r#"require "bundler"
if Bundler.settings[:global_gem_cache]
  print "global"
else
  print "cache=", Bundler.bundle_path.join("cache")
end"#;

/// The gem cache Bundler uses from `gemfile_dir`, `<Bundler.bundle_path>/cache`, as the `ruby` on
/// `PATH` says: Bundler's path follows `BUNDLE_PATH`, `.bundle/config`, deployment and
/// `GEM_HOME`, so only Bundler knows it. `RUBYOPT` is left out of the query, so that a
/// `-rbundler/setup` there doesn't need the gems the seed is for.
pub fn cache_dir(gemfile_dir: &Path) -> Result<PathBuf, String> {
    let out = Command::new("ruby")
        .args(["-e", QUERY])
        .current_dir(gemfile_dir)
        .env_remove("RUBYOPT")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "there's no ruby to ask where Bundler keeps its gems".to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    match text.strip_prefix("cache=") {
        Some(dir) if out.status.success() && Path::new(dir).is_absolute() => Ok(PathBuf::from(dir)),
        _ if text == "global" => Err(
            "Bundler's global gem cache is on, and hushrun seeds only the cache in Bundler's path"
                .into(),
        ),
        _ => Err("ruby couldn't say where Bundler keeps its gems".into()),
    }
}

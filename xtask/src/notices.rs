//! `cargo xtask notices [--check]`: writes `THIRD-PARTY-NOTICES.md`, the open-source crates that
//! hushrun's release binaries are built with, and their license texts (ADR 0034).
//!
//! The crates are those `cargo tree --package hushrun --edges normal,build --target <triple>`
//! prints for each target in `dist-workspace.toml`: what dist's workspace build compiles for the
//! binary, procedural macros and the dependencies of build scripts included, dev-dependencies and
//! this workspace's own crates left out. A crate's license is the one its Cargo.toml declares, and
//! its texts are the license files its package holds, in any of its directories: files whose names
//! start with LICENSE, LICENCE, COPYING, COPYRIGHT, NOTICE or UNLICENSE, files in a `LICENSES`
//! directory, and the file its `license-file` names. A package that holds none takes the Apache
//! License 2.0 (`xtask/licenses/Apache-2.0.txt`) when its license offers it; any other such
//! package stops the command, for someone to decide.
//!
//! The file lists the crates, then gives each distinct text once, with the crates that hold it.
//! `--check` writes nothing, and fails when the file isn't what this command would write.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// The file this command writes, at the workspace's root.
const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
/// The package whose binary the releases ship.
const PACKAGE: &str = "hushrun";
/// The Apache License 2.0, for a package without license files whose license offers it.
const APACHE_TEXT: &str = "xtask/licenses/Apache-2.0.txt";
const APACHE: &str = "Apache-2.0";
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// What the name of a license file starts with, in lowercase.
const PREFIXES: [&str; 6] = [
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "unlicense",
];
/// Extensions of files whose names start like a license file's but that hold code or data.
const NOT_TEXT: [&str; 10] = [
    "rs", "toml", "json", "yml", "yaml", "py", "sh", "c", "h", "js",
];

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let config = fs::read_to_string(dist_workspace(root)).context("reading dist-workspace.toml")?;
    let targets = dist_targets(&config)?;
    let crates = crates(root, &targets)?;
    let notices = render(&targets, &crates);
    let path = root.join(NOTICES);
    if check {
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current == notices {
            eprintln!("{NOTICES} is up to date: {} crates", crates.len());
            return Ok(true);
        }
        eprintln!(
            "{NOTICES} is out of date for Cargo.lock and dist-workspace.toml's targets: run \
             `cargo xtask notices` and commit the result"
        );
        return Ok(false);
    }
    fs::write(&path, &notices).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("wrote {NOTICES}: {} crates", crates.len());
    Ok(true)
}

/// A crate the release binaries are built with.
#[derive(Debug)]
struct Crate {
    name: String,
    version: String,
    /// The license its Cargo.toml declares: an SPDX expression, or none with `license-file`.
    license: Option<String>,
    /// Its license texts, each with where it comes from.
    texts: Vec<(Origin, String)>,
}

#[derive(Debug)]
enum Origin {
    /// A file in the crate's package, by its path there.
    File(String),
    /// The package holds no license file, and its license offers this one.
    Chosen(&'static str),
}

/// dist-workspace.toml: beside the workspace in the public repository, one level up in the
/// maintainers' repository, where the workspace is `cli/`.
fn dist_workspace(root: &Path) -> PathBuf {
    let here = root.join("dist-workspace.toml");
    if here.is_file() {
        here
    } else {
        root.join("../dist-workspace.toml")
    }
}

/// The `targets` array of `[dist]` in dist-workspace.toml, in its order.
fn dist_targets(config: &str) -> Result<Vec<String>> {
    let mut section = String::new();
    let mut lines = config
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim());
    while let Some(line) = lines.next() {
        if line.starts_with('[') && line.ends_with(']') && !line.contains('=') {
            section = line.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if section != "[dist]" || key.trim() != "targets" {
            continue;
        }
        let mut array = value.to_string();
        while !array.contains(']') {
            array.push_str(
                lines
                    .next()
                    .context("dist-workspace.toml: `targets` has no `]`")?,
            );
        }
        let targets: Vec<String> = array
            .split('"')
            .skip(1)
            .step_by(2)
            .map(String::from)
            .collect();
        if targets.is_empty() {
            bail!("dist-workspace.toml: `targets` names no target");
        }
        return Ok(targets);
    }
    bail!("dist-workspace.toml has no `targets` in [dist]")
}

/// The crates `PACKAGE` is built with for any of `targets`, sorted by name and version.
fn crates(root: &Path, targets: &[String]) -> Result<Vec<Crate>> {
    let metadata: Value = serde_json::from_str(&cargo(
        root,
        &["metadata", "--format-version", "1", "--locked"],
    )?)
    .context("reading cargo metadata's output")?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata printed no packages")?;
    let key = |p: &Value| -> (String, String) {
        let field = |name: &str| p[name].as_str().unwrap_or_default().to_string();
        (field("name"), field("version"))
    };
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata printed no workspace members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let workspace: BTreeSet<(String, String)> = packages
        .iter()
        .filter(|p| p["id"].as_str().is_some_and(|id| members.contains(id)))
        .map(key)
        .collect();
    let mut registry = BTreeMap::new();
    for package in packages.iter().filter(|p| p["source"] == CRATES_IO) {
        if registry.insert(key(package), package).is_some() {
            let (name, version) = key(package);
            bail!("cargo metadata lists {name} {version} from crates.io twice");
        }
    }

    let mut used = BTreeSet::new();
    for target in targets {
        let tree = cargo(
            root,
            &[
                "tree",
                "--locked",
                "--package",
                PACKAGE,
                "--target",
                target,
                "--edges",
                "normal,build",
                "--prefix",
                "none",
                "--format",
                "{p}",
                "--color",
                "never",
            ],
        )?;
        for line in tree.lines().filter(|l| !l.trim().is_empty()) {
            let (name, version, source) = tree_line(line)
                .with_context(|| format!("cargo tree --target {target} printed {line:?}"))?;
            let id = (name.to_string(), version.to_string());
            if workspace.contains(&id) {
                continue;
            }
            if let Some(source) = source {
                bail!(
                    "{name} {version} comes from {source}, not crates.io: deny.toml allows \
                     crates.io only"
                );
            }
            used.insert(id);
        }
    }

    let apache = read_text(&root.join(APACHE_TEXT))?;
    let mut crates = Vec::new();
    for (name, version) in used {
        let package = registry
            .get(&(name.clone(), version.clone()))
            .with_context(|| {
                format!("cargo metadata lists no crates.io package {name} {version}")
            })?;
        let manifest = package["manifest_path"]
            .as_str()
            .with_context(|| format!("cargo metadata gives {name} {version} no manifest path"))?;
        let dir = Path::new(manifest)
            .parent()
            .with_context(|| format!("{manifest} has no directory"))?;
        let license = package["license"].as_str().map(String::from);
        let mut texts: Vec<(Origin, String)> =
            license_files(dir, package["license_file"].as_str())?
                .into_iter()
                .map(|(path, text)| (Origin::File(path), text))
                .collect();
        if texts.is_empty() {
            match &license {
                Some(expression) if offers(expression, APACHE) => {
                    texts.push((Origin::Chosen(APACHE), apache.clone()));
                }
                _ => bail!(
                    "{name} {version} holds no license file, and its license ({}) doesn't offer \
                     {APACHE}: find the text it is under, add it to xtask/licenses, and have \
                     `cargo xtask notices` use it",
                    license.as_deref().unwrap_or("none declared")
                ),
            }
        }
        crates.push(Crate {
            name,
            version,
            license,
            texts,
        });
    }
    crates.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| version_key(&a.version).cmp(&version_key(&b.version)))
    });
    Ok(crates)
}

/// Runs cargo in `root`, the one running this xtask when there is one: its stdout.
fn cargo(root: &Path, args: &[&str]) -> Result<String> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let output = Command::new(&cargo)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running {}", cargo.display()))?;
    if !output.status.success() {
        bail!(
            "cargo {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("cargo printed something that isn't UTF-8")
}

/// A line of `cargo tree --prefix none --format {p}`: the name, the version, and the source when
/// it isn't crates.io, such as a path. `(proc-macro)` and `(*)`, a crate shown before, go.
fn tree_line(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut rest = line.trim();
    while let Some(shorter) = rest
        .strip_suffix(" (*)")
        .or_else(|| rest.strip_suffix(" (proc-macro)"))
    {
        rest = shorter;
    }
    let (name, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix('v')?;
    let (version, source) = match rest.split_once(' ') {
        Some((version, source)) => (version, Some(source.strip_prefix('(')?.strip_suffix(')')?)),
        None => (rest, None),
    };
    (!name.is_empty() && !version.is_empty()).then_some((name, version, source))
}

/// Whether the SPDX expression `expression` lets a user take `license` alone.
fn offers(expression: &str, license: &str) -> bool {
    // "MIT/Apache-2.0" is the old way of writing "MIT OR Apache-2.0".
    let spaced = expression
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    offers_tokens(&tokens, license)
}

fn offers_tokens(tokens: &[&str], license: &str) -> bool {
    let mut depth = 0;
    let mut start = 0;
    let mut alternatives = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        match *token {
            "(" => depth += 1,
            ")" => depth -= 1,
            "OR" if depth == 0 => {
                alternatives.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    alternatives.push(&tokens[start..]);
    alternatives
        .into_iter()
        .any(|alternative| match alternative {
            [only] => *only == license,
            _ => unparenthesized(alternative).is_some_and(|inner| offers_tokens(inner, license)),
        })
}

/// `tokens` without the parentheses around all of them, when one pair is around all of them.
fn unparenthesized<'a>(tokens: &'a [&'a str]) -> Option<&'a [&'a str]> {
    if tokens.first() != Some(&"(") || tokens.last() != Some(&")") {
        return None;
    }
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate() {
        match *token {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 && i + 1 != tokens.len() {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(&tokens[1..tokens.len() - 1])
}

/// The license files of the package in `dir`, with `license_file` (relative to it) if its
/// Cargo.toml names one: each by its path in the package, sorted, with its text.
fn license_files(dir: &Path, license_file: Option<&str>) -> Result<Vec<(String, String)>> {
    let mut found = BTreeMap::new();
    walk(dir, dir, false, &mut found)?;
    if let Some(file) = license_file {
        let path = dir.join(file);
        if let std::collections::btree_map::Entry::Vacant(entry) = found.entry(relative(dir, &path))
        {
            entry.insert(read_text(&path)?);
        }
    }
    Ok(found.into_iter().collect())
}

fn walk(
    dir: &Path,
    base: &Path,
    licenses_dir: bool,
    found: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            let licenses = matches!(name.to_ascii_lowercase().as_str(), "licenses" | "licences");
            walk(&entry.path(), base, licenses, found)?;
        } else if kind.is_file() && (licenses_dir || is_license_file(&name)) {
            found.insert(relative(base, &entry.path()), read_text(&entry.path())?);
        }
    }
    Ok(())
}

/// Whether a file named `name` is a license file: LICENSE, LICENSE-MIT, COPYING, NOTICE.txt, …
fn is_license_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(rest) = PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
    else {
        return false;
    };
    let extension = Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    (rest.is_empty() || rest.starts_with(['-', '_', '.'])) && !NOT_TEXT.contains(&extension)
}

/// `path` relative to `base`, with `/` between its parts on every OS.
fn relative(base: &Path, path: &Path) -> String {
    let path = path.strip_prefix(base).unwrap_or(path);
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A text file, the same on every OS and in every editor: no byte order mark, `\n` line ends, no
/// spaces before them, and one at the end.
fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    Ok(format!("{}\n", lines.join("\n").trim_end()))
}

/// A version's parts, numbers compared as numbers: 0.9 before 0.10.
fn version_key(version: &str) -> Vec<Result<u64, &str>> {
    version
        .split(['.', '-', '+'])
        .map(|part| part.parse().map_err(|_| part))
        .collect()
}

/// The notices: an introduction, the crates, then each distinct text once.
fn render(targets: &[String], crates: &[Crate]) -> String {
    // Each distinct text gets a number, in the order the sorted crates first hold it, and the
    // list of the crates that hold it.
    let mut numbers: BTreeMap<&str, usize> = BTreeMap::new();
    let mut texts: Vec<(&str, Vec<String>)> = Vec::new();
    for krate in crates {
        for (origin, text) in &krate.texts {
            let number = *numbers.entry(text.as_str()).or_insert_with(|| {
                texts.push((text.as_str(), Vec::new()));
                texts.len()
            });
            let from = match origin {
                Origin::File(path) => format!("`{path}`"),
                Origin::Chosen(license) => format!(
                    "its package holds no license file; {license} is one of the licenses it offers"
                ),
            };
            texts[number - 1]
                .1
                .push(format!("{} {} ({from})", krate.name, krate.version));
        }
    }

    let mut out = String::new();
    let targets: Vec<String> = targets.iter().map(|t| format!("`{t}`")).collect();
    let targets = match targets.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    let _ = write!(
        out,
        "# Third-party notices\n\n\
         hushrun is built with the open-source Rust crates below, for {targets}. Each crate is \
         under its own license, the one its Cargo.toml declares, and its source is on crates.io \
         at the version listed.\n\n\
         The license texts follow the list. Each is given once, with the crates whose packages \
         hold it.\n\n\
         This file is generated from Cargo.lock and the crates' own license files; don't edit it \
         by hand.\n\n\
         ## Crates\n\n\
         | Crate | Version | License | Texts |\n\
         |---|---|---|---|\n"
    );
    for krate in crates {
        let numbered: BTreeSet<usize> = krate
            .texts
            .iter()
            .map(|(_, text)| numbers[text.as_str()])
            .collect();
        let links: Vec<String> = numbered
            .into_iter()
            .map(|n| format!("[{n}](#text-{n})"))
            .collect();
        let license = krate
            .license
            .as_deref()
            .unwrap_or("in its license file")
            .replace('|', "\\|");
        let _ = writeln!(
            out,
            "| [{name}](https://crates.io/crates/{name}/{version}) | {version} | {license} | {} |",
            links.join(", "),
            name = krate.name,
            version = krate.version,
        );
    }
    out.push_str("\n## License texts\n");
    for (i, (text, holders)) in texts.iter().enumerate() {
        let fence = fence(text);
        let _ = write!(
            out,
            "\n### Text {}\n\n{}.\n\n{fence}text\n{text}{fence}\n",
            i + 1,
            holders.join(", ")
        );
    }
    out
}

/// A code fence longer than any run of backticks in `text`.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_targets_are_dist_workspace_tomls() {
        let config = fs::read_to_string(dist_workspace(&crate::workspace_root())).unwrap();
        let targets = dist_targets(&config).unwrap();
        assert_eq!(targets.len(), 6);
        for target in crate::installer::TARGETS {
            assert!(targets.iter().any(|t| t == target), "{target}");
        }
        assert!(targets.iter().any(|t| t == "x86_64-pc-windows-msvc"));
    }

    #[test]
    fn targets_are_read_from_the_dist_section_only() {
        let config = "[workspace]\ntargets = [\"no\"]\n\n[dist]\n# targets = [\"commented\"]\n\
                      targets = [\n  \"a-b-c\", # the first\n  \"d-e-f\",\n]\n";
        assert_eq!(dist_targets(config).unwrap(), ["a-b-c", "d-e-f"]);
        assert!(dist_targets("[dist]\nci = \"github\"\n").is_err());
        assert!(dist_targets("[dist]\ntargets = [\n").is_err());
    }

    #[test]
    fn tree_lines() {
        assert_eq!(
            tree_line("anyhow v1.0.104"),
            Some(("anyhow", "1.0.104", None))
        );
        assert_eq!(
            tree_line("serde_derive v1.0.219 (proc-macro) (*)"),
            Some(("serde_derive", "1.0.219", None))
        );
        assert_eq!(
            tree_line("hushrun-core v0.1.0 (/src/cli/crates/hushrun-core) (*)"),
            Some((
                "hushrun-core",
                "0.1.0",
                Some("/src/cli/crates/hushrun-core")
            ))
        );
        assert_eq!(
            tree_line("windows-sys v0.61.2"),
            Some(("windows-sys", "0.61.2", None))
        );
        assert_eq!(tree_line("[build-dependencies]"), None);
        assert_eq!(tree_line("anyhow"), None);
    }

    #[test]
    fn licenses_on_offer() {
        for yes in [
            "Apache-2.0",
            "Apache-2.0 OR MIT",
            "MIT OR Apache-2.0",
            "MIT/Apache-2.0",
            "Apache-2.0 / MIT",
            "(Apache-2.0 OR MIT)",
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
            "CC0-1.0 OR MIT-0 OR Apache-2.0",
            "(MIT AND ISC) OR (Apache-2.0)",
        ] {
            assert!(offers(yes, APACHE), "{yes}");
        }
        for no in [
            "MIT",
            "Apache-2.0 AND ISC",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "(Apache-2.0 OR MIT) AND BSD-3-Clause",
            "Apache-2.0 WITH LLVM-exception",
            "(Apache-2.0) AND (MIT)",
            "",
        ] {
            assert!(!offers(no, APACHE), "{no}");
        }
    }

    #[test]
    fn license_file_names() {
        for yes in [
            "LICENSE",
            "LICENSE-MIT",
            "license-apache-2.0",
            "LICENSE.txt",
            "LICENSE-ZLIB.md",
            "LICENCE",
            "COPYING",
            "COPYRIGHT",
            "NOTICE",
            "UNLICENSE",
            "LICENSE-Apache-2.0_WITH_LLVM-exception",
        ] {
            assert!(is_license_file(yes), "{yes}");
        }
        for no in [
            "license.rs",
            "licenses.md",
            "notice_board.json",
            "README.md",
            "Cargo.toml",
            "unlicensed.txt",
            "copy.txt",
        ] {
            assert!(!is_license_file(no), "{no}");
        }
    }

    #[test]
    fn a_packages_license_files_in_any_directory() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        };
        write("LICENSE-MIT", "\u{feff}MIT text\r\nsecond line\r\n\r\n");
        write("c_src/lib/LICENSE", "C library's text  \n\tindented\t\n");
        write("LICENSES/Zlib.txt", "Zlib text\n");
        write("src/license.rs", "fn license() {}");
        write("LEGAL", "named by license-file");
        write("README.md", "# readme");
        let found = license_files(dir.path(), Some("LEGAL")).unwrap();
        assert_eq!(
            found,
            [
                ("LEGAL".to_string(), "named by license-file\n".to_string()),
                ("LICENSE-MIT".into(), "MIT text\nsecond line\n".into()),
                ("LICENSES/Zlib.txt".into(), "Zlib text\n".into()),
                (
                    "c_src/lib/LICENSE".into(),
                    "C library's text\n\tindented\n".into()
                ),
            ]
        );
    }

    #[test]
    fn versions_sort_by_number() {
        let mut versions = ["0.10.0", "0.9.1", "0.9.0", "1.0.0-rc.1", "1.0.0"];
        versions.sort_by_key(|v| version_key(v));
        assert_eq!(
            versions,
            ["0.9.0", "0.9.1", "0.10.0", "1.0.0", "1.0.0-rc.1"]
        );
    }

    #[test]
    fn fences_outlast_the_texts_backticks() {
        assert_eq!(fence("no backticks"), "```");
        assert_eq!(fence("a ``` b"), "````");
        assert_eq!(fence("`````"), "``````");
    }

    #[test]
    fn each_text_once_with_the_crates_that_hold_it() {
        let file = |path: &str, text: &str| (Origin::File(path.into()), text.to_string());
        let crates = [
            Crate {
                name: "alpha".into(),
                version: "1.0.0".into(),
                license: Some("MIT OR Apache-2.0".into()),
                texts: vec![
                    file("LICENSE-APACHE", "Apache text\n"),
                    file("LICENSE-MIT", "Copyright alpha\n"),
                ],
            },
            Crate {
                name: "beta".into(),
                version: "0.2.0".into(),
                license: Some("Apache-2.0 OR MIT".into()),
                texts: vec![(Origin::Chosen(APACHE), "Apache text\n".into())],
            },
            Crate {
                name: "gamma".into(),
                version: "3.1.4".into(),
                license: None,
                texts: vec![file("COPYING", "has ``` inside\n")],
            },
        ];
        let notices = render(
            &[
                "x86_64-unknown-linux-musl".into(),
                "aarch64-apple-darwin".into(),
            ],
            &crates,
        );
        assert!(notices.starts_with("# Third-party notices\n\n"));
        assert!(
            notices
                .contains("for `x86_64-unknown-linux-musl` and `aarch64-apple-darwin`. Each crate")
        );
        assert!(notices.contains(
            "| [alpha](https://crates.io/crates/alpha/1.0.0) | 1.0.0 | MIT OR Apache-2.0 | \
             [1](#text-1), [2](#text-2) |\n"
        ));
        assert!(notices.contains(
            "| [beta](https://crates.io/crates/beta/0.2.0) | 0.2.0 | Apache-2.0 OR MIT | \
             [1](#text-1) |\n"
        ));
        assert!(notices.contains("| 3.1.4 | in its license file | [3](#text-3) |\n"));
        assert!(notices.contains(
            "\n### Text 1\n\nalpha 1.0.0 (`LICENSE-APACHE`), beta 0.2.0 (its package holds no \
             license file; Apache-2.0 is one of the licenses it offers).\n\n```text\nApache text\n```\n"
        ));
        assert!(notices.contains("\n### Text 2\n\nalpha 1.0.0 (`LICENSE-MIT`).\n\n"));
        assert!(notices.contains("````text\nhas ``` inside\n````\n"));
        assert_eq!(notices.matches("Apache text").count(), 1);
        assert!(notices.ends_with("````\n"));
    }
}

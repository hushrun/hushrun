//! \[ADR 0087\] A `ctest` command line as hushrun's `ctest` reads it (CMake 3.21 to 4.4): which of
//! the arguments make it no test run, which select tests by position, which only shape the run
//! and are never repeated for a listing, and the test presets it names.
//!
//! hushrun's `ctest` never rewrites what was written: it removes the one option it merges with
//! its own (`-E`, `-R` or `--exclude-from-file`) and appends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Options that take a value, as the next argument or as `--option=value`.
const VALUED: &[&str] = &[
    "-C",
    "--build-config",
    "-L",
    "--label-regex",
    "-LE",
    "--label-exclude",
    "-R",
    "--tests-regex",
    "-E",
    "--exclude-regex",
    "-FA",
    "--fixture-exclude-any",
    "-FS",
    "--fixture-exclude-setup",
    "-FC",
    "--fixture-exclude-cleanup",
    "-I",
    "--tests-information",
    "-O",
    "--output-log",
    "--output-junit",
    "--test-dir",
    "--preset",
    "--repeat",
    "--repeat-until-fail",
    "--timeout",
    "--stop-time",
    "--test-load",
    "--resource-spec-file",
    "-A",
    "--add-notes",
    "--test-output-size-passed",
    "--test-output-size-failed",
    "--test-output-truncation",
    "--max-width",
    "--interactive-debug-mode",
    "--tests-from-file",
    "--exclude-from-file",
    "-M",
    "--test-model",
    "-T",
    "--test-action",
    "-D",
    "--dashboard",
    "-S",
    "-SP",
    "-SR",
    "--track",
    "--group",
    "--submit-index",
    "--http-header",
    "--schedule-random-seed",
    "--extra-submit",
    "--overwrite",
    "--no-tests",
];

/// Options that take no value.
const FLAGS: &[&str] = &[
    "-V",
    "--verbose",
    "-VV",
    "--extra-verbose",
    "--debug",
    "--output-on-failure",
    "-Q",
    "--quiet",
    "--rerun-failed",
    "--stop-on-failure",
    "--schedule-random",
    "-F",
    "--force-new-ctest-process",
    "--no-label-summary",
    "--no-subproject-summary",
    "--no-compress-output",
    "--progress",
    "-U",
    "--union",
    "--http1.0",
    "--print-labels",
    "-N",
    "--show-only",
    "--list-presets",
    "--version",
    "-version",
    "/V",
    "/?",
    "-h",
    "-H",
    "--help",
    "-help",
    "-usage",
    "--launch",
];

/// `-j` and `--parallel` take a number, optionally.
const PARALLEL: &[&str] = &["-j", "--parallel"];

/// Anything here means it isn't a plain test run: the real `ctest` gets everything untouched.
const PASSTHROUGH: &[&str] = &[
    "-S",
    "-SP",
    "-SR",
    "-D",
    "--dashboard",
    "-M",
    "--test-model",
    "-N",
    "--show-only",
    "--print-labels",
    "--help",
    "-help",
    "-h",
    "-H",
    "/?",
    "/V",
    "-usage",
    "--version",
    "-version",
    "--list-presets",
    "--extra-submit",
    "--launch",
];

/// These pick tests by their position after filtering, so a filter of hushrun's would change
/// what they pick (ran: `--rerun-failed` with `-E` picked the wrong tests).
const BY_POSITION: &[&str] = &["--rerun-failed", "-I", "--tests-information"];

/// Never repeated for a listing: they only shape the run, or write files (ran: `-O` makes even
/// `--show-only` write the user's log).
const RUN_ONLY: &[&str] = &[
    "-j",
    "--parallel",
    "-O",
    "--output-log",
    "--output-junit",
    "--repeat",
    "--repeat-until-fail",
    "--stop-on-failure",
    "--schedule-random",
    "--schedule-random-seed",
    "--timeout",
    "--stop-time",
    "-V",
    "--verbose",
    "-VV",
    "--extra-verbose",
    "--debug",
    "-Q",
    "--quiet",
    "--output-on-failure",
    "--progress",
    "--test-load",
    "--resource-spec-file",
    "--no-tests",
    "--test-output-size-passed",
    "--test-output-size-failed",
    "--test-output-truncation",
    "--max-width",
    "-T",
    "--test-action",
    "-A",
    "--add-notes",
    "--no-label-summary",
    "--no-subproject-summary",
    "--interactive-debug-mode",
    "-F",
    "--force-new-ctest-process",
    "--no-compress-output",
    "--track",
    "--group",
    "--http-header",
    "--submit-index",
    "--http1.0",
    "--overwrite",
];

/// One argument, or an option with its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The option, `-R`; `None` for an argument that is none.
    pub opt: Option<String>,
    pub value: Option<String>,
    /// The words as written.
    pub raw: Vec<String>,
}

/// What a command line is to hushrun's `ctest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Not a test run: everything goes to the real `ctest` untouched, and nothing is observed.
    Passthrough,
    /// A test run hushrun observes and leaves nothing out of, and why.
    Observe(String),
    /// A test run hushrun may leave tests out of.
    Select,
    /// An argument hushrun doesn't read: nothing is observed, and why.
    Unread(String),
}

/// The words of a command line as items. `--build-and-test` takes the rest.
pub fn parse(args: &[String]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let word = &args[i];
        if word == "--build-and-test" {
            items.push(Item {
                opt: Some(word.clone()),
                value: None,
                raw: args[i..].to_vec(),
            });
            break;
        }
        if let Some((opt, value)) = word.split_once('=').filter(|(o, _)| o.starts_with("--")) {
            items.push(Item {
                opt: Some(opt.to_string()),
                value: Some(value.to_string()),
                raw: vec![word.clone()],
            });
            i += 1;
            continue;
        }
        if PARALLEL.contains(&word.as_str()) {
            let next = args.get(i + 1).filter(|n| n.parse::<u32>().is_ok());
            items.push(Item {
                opt: Some(word.clone()),
                value: next.cloned(),
                raw: std::iter::once(word.clone()).chain(next.cloned()).collect(),
            });
            i += 1 + usize::from(next.is_some());
            continue;
        }
        if let Some(n) = word.strip_prefix("-j").filter(|n| n.parse::<u32>().is_ok()) {
            items.push(Item {
                opt: Some("-j".into()),
                value: Some(n.to_string()),
                raw: vec![word.clone()],
            });
            i += 1;
            continue;
        }
        if VALUED.contains(&word.as_str()) && i + 1 < args.len() {
            items.push(Item {
                opt: Some(word.clone()),
                value: Some(args[i + 1].clone()),
                raw: vec![word.clone(), args[i + 1].clone()],
            });
            i += 2;
            continue;
        }
        items.push(Item {
            opt: word.starts_with('-').then(|| word.clone()),
            value: (!word.starts_with('-')).then(|| word.clone()),
            raw: vec![word.clone()],
        });
        i += 1;
    }
    items
}

/// The value of the last of `names`, as the last one given wins (ran: two `-E`, only the last
/// counted).
pub fn last<'a>(items: &'a [Item], names: &[&str]) -> Option<&'a str> {
    items
        .iter()
        .rev()
        .find(|item| item.opt.as_deref().is_some_and(|o| names.contains(&o)))
        .and_then(|item| item.value.as_deref())
}

/// Whether any of `names` is given.
pub fn has(items: &[Item], names: &[&str]) -> bool {
    items
        .iter()
        .any(|item| item.opt.as_deref().is_some_and(|o| names.contains(&o)))
}

/// What the command line is to hushrun's `ctest`.
pub fn mode(items: &[Item]) -> Mode {
    for item in items {
        let Some(opt) = item.opt.as_deref() else {
            return Mode::Unread(format!(
                "ctest's argument {} isn't one hushrun reads",
                item.raw[0]
            ));
        };
        if PASSTHROUGH.contains(&opt) || opt.starts_with("--build-") || opt.starts_with("--help") {
            return Mode::Passthrough;
        }
        if matches!(opt, "-T" | "--test-action")
            && !item
                .value
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case("test") || v.eq_ignore_ascii_case("start"))
        {
            return Mode::Passthrough;
        }
    }
    for item in items {
        let opt = item.opt.as_deref().unwrap_or_default();
        if !(VALUED.contains(&opt) || FLAGS.contains(&opt) || PARALLEL.contains(&opt)) {
            return Mode::Unread(format!(
                "ctest's argument {} isn't one hushrun reads",
                item.raw[0]
            ));
        }
        if VALUED.contains(&opt) && item.value.is_none() {
            return Mode::Unread(format!("ctest's {opt} has no value"));
        }
    }
    if let Some(item) = items
        .iter()
        .find(|i| i.opt.as_deref().is_some_and(|o| BY_POSITION.contains(&o)))
    {
        return Mode::Observe(format!(
            "{} picks tests by their position, which leaving any out would change",
            item.raw[0]
        ));
    }
    Mode::Select
}

/// The words for a listing: every item but those that only shape the run, and but `drop`.
pub fn listing(items: &[Item], drop: &[&str]) -> Vec<String> {
    items
        .iter()
        .filter(|item| {
            let opt = item.opt.as_deref().unwrap_or_default();
            !RUN_ONLY.contains(&opt) && !drop.contains(&opt)
        })
        .flat_map(|item| item.raw.iter().cloned())
        .collect()
}

/// The words of every item but `drop`.
pub fn without(items: &[Item], drop: &[&str]) -> Vec<String> {
    items
        .iter()
        .filter(|item| !item.opt.as_deref().is_some_and(|opt| drop.contains(&opt)))
        .flat_map(|item| item.raw.iter().cloned())
        .collect()
}

/// `--repeat until-pass:N` or `after-timeout:N`, or with no `--repeat` a preset's
/// `execution.repeat` of those modes: a test's pass may come after it failed, and the JUnit file
/// keeps only the last attempt.
pub fn retries(items: &[Item], preset: Option<&Preset>) -> bool {
    match last(items, &["--repeat"]) {
        Some(mode) => {
            let mode = mode.to_ascii_lowercase();
            mode.starts_with("until-pass") || mode.starts_with("after-timeout")
        }
        None => preset.is_some_and(|p| p.retries),
    }
}

/// What hushrun's `ctest` must know of a test preset (ran, CMake 4.4: a command line's `-E`
/// replaces the preset's `filter.exclude.name`, and its `--output-junit` the preset's
/// `output.outputJUnitFile`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preset {
    /// The configure preset's `binaryDir`, where the tests are.
    pub binary_dir: Option<PathBuf>,
    pub exclude_name: Option<String>,
    pub junit: Option<String>,
    /// It sets `filter.exclude.fromFile`, which a command line's file would replace.
    pub exclude_file: bool,
    /// Its `execution.repeat` is `until-pass` or `after-timeout`: a pass may come after a failure,
    /// as with `--repeat` (which replaces it).
    pub retries: bool,
    /// It sets `filter.include.index`, which picks tests by their position, as `-I` does.
    pub by_position: bool,
    /// It sets `execution.showOnly`: `ctest --preset` runs no test.
    pub show_only: bool,
    /// Its `output.outputLogFile`, which even a listing writes, and a command line's `-O` doesn't
    /// replace (ran, CMake 3.22 and 4.4).
    pub log: Option<String>,
}

/// The test preset `name` of the presets in `dir`, merged as CMake merges them (`include`,
/// `inherits`, earlier parents first); `Err` why it can't be read, when a field hushrun needs
/// holds a macro it doesn't expand, or the files don't read.
pub fn preset(name: &str, dir: &Path) -> Result<Preset, String> {
    let mut seen = Vec::new();
    let mut all = Presets::default();
    for file in ["CMakePresets.json", "CMakeUserPresets.json"] {
        load(&dir.join(file), &mut seen, &mut all)?;
    }
    let tp = resolve(&all.test, name, 0).ok_or_else(|| format!("no test preset named {name}"))?;
    let mut out = Preset::default();
    let expand = |text: &str, preset: &str| expand(text, dir, preset);
    if let Some(cp_name) = tp.get("configurePreset").and_then(|v| v.as_str())
        && let Some(cp) = resolve(&all.configure, cp_name, 0)
        && let Some(binary) = cp.get("binaryDir").and_then(|v| v.as_str())
    {
        let binary = PathBuf::from(expand(binary, cp_name)?);
        out.binary_dir = Some(if binary.is_absolute() {
            binary
        } else {
            dir.join(binary)
        });
    }
    let filter = tp.get("filter");
    let exclude = filter.and_then(|f| f.get("exclude"));
    if let Some(regex) = exclude.and_then(|e| e.get("name")).and_then(|v| v.as_str()) {
        out.exclude_name = Some(expand(regex, name)?);
    }
    out.exclude_file = exclude.and_then(|e| e.get("fromFile")).is_some();
    out.by_position = filter
        .and_then(|f| f.get("include"))
        .and_then(|i| i.get("index"))
        .is_some();
    let execution = tp.get("execution");
    out.retries = execution
        .and_then(|e| e.get("repeat"))
        .and_then(|r| r.get("mode"))
        .and_then(|m| m.as_str())
        .is_some_and(|mode| matches!(mode, "until-pass" | "after-timeout"));
    if let Some(log) = tp
        .get("output")
        .and_then(|o| o.get("outputLogFile"))
        .and_then(|v| v.as_str())
    {
        out.log = Some(expand(log, name)?);
    }
    out.show_only = execution
        .and_then(|e| e.get("showOnly"))
        .is_some_and(|v| !v.is_null());

    if let Some(junit) = tp
        .get("output")
        .and_then(|o| o.get("outputJUnitFile"))
        .and_then(|v| v.as_str())
    {
        out.junit = Some(expand(junit, name)?);
    }
    Ok(out)
}

#[derive(Default)]
struct Presets {
    configure: BTreeMap<String, serde_json::Value>,
    test: BTreeMap<String, serde_json::Value>,
}

fn load(path: &Path, seen: &mut Vec<PathBuf>, into: &mut Presets) -> Result<(), String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&real) || !path.exists() {
        return Ok(());
    }
    seen.push(real);
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
    for (kind, map) in [
        ("configurePresets", &mut into.configure),
        ("testPresets", &mut into.test),
    ] {
        for preset in doc
            .get(kind)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(name) = preset.get("name").and_then(|v| v.as_str()) {
                map.entry(name.to_string())
                    .or_insert_with(|| preset.clone());
            }
        }
    }
    let base = path.parent().unwrap_or(Path::new("."));
    for include in doc
        .get("include")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(file) = include.as_str() {
            load(&base.join(file), seen, into)?;
        }
    }
    Ok(())
}

/// A preset with what it inherits merged in: earlier parents win over later ones, and its own
/// fields over all.
fn resolve(
    presets: &BTreeMap<String, serde_json::Value>,
    name: &str,
    depth: usize,
) -> Option<serde_json::Value> {
    let preset = presets.get(name)?;
    if depth > 32 {
        return None;
    }
    let mut out = serde_json::Value::Object(serde_json::Map::new());
    let parents: Vec<String> = match preset.get("inherits") {
        Some(serde_json::Value::String(one)) => vec![one.clone()],
        Some(serde_json::Value::Array(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    for parent in parents.iter().rev() {
        merge(&mut out, &resolve(presets, parent, depth + 1)?);
    }
    let mut own = preset.clone();
    if let Some(map) = own.as_object_mut() {
        map.remove("inherits");
        map.remove("hidden");
    }
    merge(&mut out, &own);
    Some(out)
}

fn merge(into: &mut serde_json::Value, from: &serde_json::Value) {
    match (into, from) {
        (serde_json::Value::Object(into), serde_json::Value::Object(from)) => {
            for (key, value) in from {
                match into.get_mut(key) {
                    Some(have) if have.is_object() && value.is_object() => merge(have, value),
                    _ => {
                        into.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (into, from) => *into = from.clone(),
    }
}

/// The macros of a preset's string hushrun expands; `Err` for any other, which it can't say.
fn expand(text: &str, dir: &Path, preset: &str) -> Result<String, String> {
    let dir_text = dir.to_string_lossy();
    let mut out = text
        .replace("${sourceDir}", &dir_text)
        .replace(
            "${sourceParentDir}",
            &dir.parent().unwrap_or(dir).to_string_lossy(),
        )
        .replace(
            "${sourceDirName}",
            &dir.file_name().unwrap_or_default().to_string_lossy(),
        )
        .replace("${presetName}", preset)
        .replace("${dollar}", "$");
    while let Some(start) = out.find("$env{") {
        let Some(len) = out[start + 5..].find('}') else {
            return Err(format!("a preset's {text} doesn't close a macro"));
        };
        let name = out[start + 5..start + 5 + len].to_string();
        let value = std::env::var(&name).unwrap_or_default();
        out.replace_range(start..start + 6 + len, &value);
    }
    if out.contains("${") || out.contains("$penv{") || out.contains("$vendor{") {
        return Err(format!(
            "a preset's {text} holds a macro hushrun doesn't expand"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-49
    #[test]
    fn a_command_line_is_classified_as_the_spike_found() {
        let items = parse(&words(
            "--test-dir build -j8 -R ^add --output-on-failure -O log.txt --repeat until-pass:3 -E sleep|crash --no-tests=error",
        ));
        assert_eq!(mode(&items), Mode::Select);
        assert_eq!(
            last(&items, &["-E", "--exclude-regex"]),
            Some("sleep|crash")
        );
        assert!(retries(&items, None));
        // The listing keeps what selects and drops what only shapes the run, or writes a file.
        assert_eq!(
            listing(&items, &[]),
            words("--test-dir build -R ^add -E sleep|crash")
        );
        assert_eq!(
            without(&items, &["-E"]),
            words(
                "--test-dir build -j8 -R ^add --output-on-failure -O log.txt --repeat until-pass:3 --no-tests=error"
            )
        );
        for passthrough in [
            "-N",
            "--show-only=json-v1",
            "-S script.cmake",
            "--build-and-test src out --build-generator Ninja",
            "--version",
            "--help",
            "-T Build",
            "--print-labels",
            "-D Experimental",
        ] {
            assert_eq!(
                mode(&parse(&words(passthrough))),
                Mode::Passthrough,
                "{passthrough}"
            );
        }
        assert_eq!(mode(&parse(&words("-T Test -E x"))), Mode::Select);
        for observe in ["--rerun-failed", "-I 1,3", "--tests-information 0,0,0,2"] {
            assert!(
                matches!(mode(&parse(&words(observe))), Mode::Observe(_)),
                "{observe}"
            );
        }
        for unread in ["--mystery", "stray", "-R"] {
            assert!(
                matches!(mode(&parse(&words(unread))), Mode::Unread(_)),
                "{unread}"
            );
        }
        // `-j` with no number, and `--parallel 4`.
        let items = parse(&words("-j --parallel 4 -R x"));
        assert_eq!(items.len(), 3);
        assert_eq!(mode(&items), Mode::Select);
        assert_eq!(listing(&items, &[]), words("-R x"));
    }

    // covers: C-49
    #[test]
    fn a_test_preset_is_merged_with_what_it_inherits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("CMakePresets.json"),
            r#"{"version": 6, "include": ["more.json"],
               "configurePresets": [{"name": "dev", "binaryDir": "${sourceDir}/build-${presetName}"}],
               "testPresets": [
                 {"name": "base", "hidden": true, "configurePreset": "dev",
                  "filter": {"exclude": {"name": "^gamma$"}}},
                 {"name": "ci", "inherits": "base", "output": {"outputJUnitFile": "junit-${presetName}.xml"}}
               ]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("more.json"),
            r#"{"version": 6, "testPresets": [{"name": "odd", "configurePreset": "dev",
               "output": {"outputJUnitFile": "$penv{X}.xml"}}]}"#,
        )
        .unwrap();
        let ci = preset("ci", dir.path()).unwrap();
        assert_eq!(ci.binary_dir, Some(dir.path().join("build-dev")));
        assert_eq!(ci.exclude_name.as_deref(), Some("^gamma$"));
        assert_eq!(ci.junit.as_deref(), Some("junit-ci.xml"));
        assert!(preset("odd", dir.path()).is_err());
        assert!(preset("nope", dir.path()).is_err());
    }
}

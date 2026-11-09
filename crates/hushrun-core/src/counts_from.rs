//! \[ADR 0045\] `counts_from` in `hushrun.yaml` (C-25): what a check needs of the machine a run
//! happens on. A rule is about where a run happened, never about whose evidence counts.
//!
//! This holds the rule as the file writes it and what the file may write: nothing here measures
//! anything, so a rule's place in a run's key (C-9) can't depend on the machine.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use crate::agent;
use crate::names::{CheckName, ScopeName};
use crate::yaml::{Errors, Map, Pos, Spanned};

/// What `os` takes.
pub const OS: &[&str] = &["macos", "linux", "windows"];
/// What `arch` takes.
pub const ARCH: &[&str] = &["arm64", "amd64"];
/// The tools a rule can name, each taking version prefixes. The binary's measuring has a row for
/// each.
pub const TOOLS: &[&str] = &["go", "rust", "node", "xcode"];

/// What a check needs: all of its keys, each of them one of its values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// Sorted. Empty: any.
    pub os: Vec<String>,
    /// Sorted. Empty: any.
    pub arch: Vec<String>,
    /// Version prefixes by tool, each list sorted.
    pub tools: BTreeMap<String, Vec<String>>,
    /// The values each variable may hold, each list sorted.
    pub env: BTreeMap<String, Vec<String>>,
}

impl Rule {
    /// The rule as a run's key holds it (C-9): one form, however the file writes it.
    pub fn normal(&self) -> Value {
        let mut rule = serde_json::Map::new();
        if !self.os.is_empty() {
            rule.insert("os".into(), json!(self.os));
        }
        if !self.arch.is_empty() {
            rule.insert("arch".into(), json!(self.arch));
        }
        for (tool, versions) in &self.tools {
            rule.insert(tool.clone(), json!(versions));
        }
        if !self.env.is_empty() {
            rule.insert("env".into(), json!(self.env));
        }
        Value::Object(rule)
    }
}

/// `counts_from` at one level of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    /// `counts_from: anywhere`: no rule, and the levels further out don't apply.
    Anywhere,
    Rule(Rule),
}

/// Every `counts_from` of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub top: Option<Level>,
    pub scopes: BTreeMap<ScopeName, Level>,
    pub checks: BTreeMap<CheckName, Level>,
}

impl Rules {
    /// Whether the file writes `counts_from` nowhere: `anywhere` is writing it.
    pub fn is_empty(&self) -> bool {
        self.top.is_none() && self.scopes.is_empty() && self.checks.is_empty()
    }

    /// The rule of `check` in `scope`: the check's, else the scope's, else the file's. The nearest
    /// wins whole, with no merging.
    pub fn resolve(&self, check: &CheckName, scope: &ScopeName) -> Option<&Rule> {
        let level = self
            .checks
            .get(check)
            .or_else(|| self.scopes.get(scope))
            .or(self.top.as_ref())?;
        match level {
            Level::Anywhere => None,
            Level::Rule(rule) => Some(rule),
        }
    }
}

/// Whether `version`, measured, is one a rule's `prefix` names, compared by dotted segment: `1.25`
/// names 1.25 and 1.25.14, not 1.250 or 1.2. A pre-release, such as `1.25rc2` or `1.94.0-nightly`,
/// isn't the release its number names: only a prefix that writes it out whole names it.
pub fn version_matches(prefix: &str, version: &str) -> bool {
    if prefix == version {
        return true;
    }
    let released = version.chars().all(|c| c.is_ascii_digit() || c == '.');
    let mut segments = version.split('.');
    released && prefix.split('.').all(|want| segments.next() == Some(want))
}

/// A prefix as a rule may write it: dotted numbers, then anything a pre-release adds.
fn is_version(text: &str) -> bool {
    let end = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, rest) = text.split_at(end);
    !number.is_empty()
        && number.split('.').all(|part| !part.is_empty())
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// A YAML node as written, for messages a closed schema can't give: that a number needs quotes
/// isn't something a parser into strings can say.
#[derive(Debug)]
pub(crate) enum Raw {
    /// The key isn't there.
    Absent,
    Text(String),
    /// A scalar that isn't a string, as YAML reads it: `1.2`, `true`, `null`.
    Other(String),
    List(Vec<Spanned<Raw>>),
    Map(Map<String, Spanned<Raw>>),
}

/// What a level is when the file doesn't write the key.
pub(crate) fn absent() -> Spanned<Raw> {
    Spanned::new(Raw::Absent, Pos::UNKNOWN)
}

impl<'de> Deserialize<'de> for Raw {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RawVisitor;

        impl<'de> Visitor<'de> for RawVisitor {
            type Value = Raw;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value")
            }

            fn visit_str<E>(self, text: &str) -> Result<Raw, E> {
                Ok(Raw::Text(text.to_string()))
            }

            fn visit_bool<E>(self, value: bool) -> Result<Raw, E> {
                Ok(Raw::Other(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Raw, E> {
                Ok(Raw::Other(value.to_string()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Raw, E> {
                Ok(Raw::Other(value.to_string()))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Raw, E> {
                Ok(Raw::Other(value.to_string()))
            }

            fn visit_unit<E>(self) -> Result<Raw, E> {
                Ok(Raw::Other("null".to_string()))
            }

            fn visit_none<E>(self) -> Result<Raw, E> {
                Ok(Raw::Other("null".to_string()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Raw, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = access.next_element::<Spanned<Raw>>()? {
                    items.push(item);
                }
                Ok(Raw::List(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Raw, A::Error> {
                let mut map = Map::default();
                while let Some(key) = access.next_key::<Spanned<String>>()? {
                    let value = access.next_value::<Spanned<Raw>>()?;
                    map.insert(key.value, key.pos, value);
                }
                Ok(Raw::Map(map))
            }
        }

        deserializer.deserialize_any(RawVisitor)
    }
}

/// `node`, one `counts_from`, checked (C-8); `None` when the file doesn't write it, or writes it
/// wrongly, which `errors` says.
pub(crate) fn level(node: &Spanned<Raw>, errors: &mut Errors) -> Option<Level> {
    let keys = match &node.value {
        Raw::Absent => return None,
        Raw::Text(word) if word == "anywhere" => return Some(Level::Anywhere),
        Raw::Map(keys) => keys,
        _ => {
            errors.push(
                node.pos,
                "`counts_from` is `anywhere`, or what a check needs, such as {os: linux, go: \"1.25\"}",
            );
            return None;
        }
    };
    if keys.is_empty() {
        errors.push(
            node.pos,
            "`counts_from` is empty: write what a check needs, or `anywhere`",
        );
        return None;
    }
    let before = errors.len();
    let mut rule = Rule::default();
    for (key, pos, value) in keys.iter() {
        match key.as_str() {
            "os" => rule.os = chosen(key, value, OS, errors),
            "arch" => rule.arch = chosen(key, value, ARCH, errors),
            "env" => rule.env = variables(value, errors),
            tool if TOOLS.contains(&tool) => {
                rule.tools
                    .insert(tool.to_string(), versions(tool, value, errors));
            }
            other => errors.push(
                pos,
                format!(
                    "`counts_from` has no key `{other}`: it takes os, arch, {} and env",
                    TOOLS.join(", ")
                ),
            ),
        }
    }
    (errors.len() == before).then_some(Level::Rule(rule))
}

/// A key's values, one or a list, each a string; sorted. `say` says what a wrong one should be.
fn strings(
    name: &str,
    value: &Spanned<Raw>,
    errors: &mut Errors,
    mut say: impl FnMut(&str, Pos, &mut Errors),
) -> Vec<String> {
    let items: Vec<&Spanned<Raw>> = match &value.value {
        Raw::List(items) => items.iter().collect(),
        _ => vec![value],
    };
    if items.is_empty() {
        errors.push(value.pos, format!("`{name}` lists nothing"));
    }
    let mut found: Vec<String> = Vec::new();
    for item in items {
        match &item.value {
            Raw::Text(text) if found.contains(text) => {
                errors.push(item.pos, format!("`{text}` is listed twice"));
            }
            Raw::Text(text) => {
                say(text, item.pos, errors);
                found.push(text.clone());
            }
            Raw::Other(text) => errors.push(
                item.pos,
                format!(
                    "`{name}` is {text} here, which isn't text: YAML makes 1.20 into 1.2, so quote it, as {name}: \"1.25\""
                ),
            ),
            _ => errors.push(
                item.pos,
                format!("`{name}` is one value or a list of values"),
            ),
        }
    }
    found.sort();
    found
}

/// `os` or `arch`: words from a closed list, with what a person may write instead.
fn chosen(name: &str, value: &Spanned<Raw>, list: &[&str], errors: &mut Errors) -> Vec<String> {
    strings(name, value, errors, |text, pos, errors| {
        if list.contains(&text) {
            return;
        }
        let lower = text.to_ascii_lowercase();
        let meant = match lower.as_str() {
            "darwin" | "mac" | "osx" => "macos",
            "win" | "win32" => "windows",
            "x86_64" | "x64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        match list.contains(&meant) {
            true => errors.push(pos, format!("`{name}` is `{meant}`, not `{text}`")),
            false => errors.push(pos, format!("`{name}` is one of {}", list.join(", "))),
        }
    })
}

/// A tool's version prefixes.
fn versions(tool: &str, value: &Spanned<Raw>, errors: &mut Errors) -> Vec<String> {
    strings(tool, value, errors, |text, pos, errors| {
        if !is_version(text) {
            errors.push(
                pos,
                format!("`{tool}` is a version such as \"1.25\", not {text:?}"),
            );
        }
    })
}

/// \[ADR 0046\] What `env` can't name, in any case, since Windows' variables have none: the string,
/// and what hushrun answers from inside it. A rule compares, and these it would compare to a secret.
const HOLD_THE_STRING: [&str; 3] = [agent::VARIABLE, "HUSHRUN_TOKEN", "HUSHRUN_MACHINE_KEY"];

/// `env`: variables and the values each may hold.
fn variables(value: &Spanned<Raw>, errors: &mut Errors) -> BTreeMap<String, Vec<String>> {
    let Raw::Map(vars) = &value.value else {
        errors.push(
            value.pos,
            "`env` maps variables to what they must hold, such as {SANDBOX: go-7}",
        );
        return BTreeMap::new();
    };
    if vars.is_empty() {
        errors.push(value.pos, "`env` lists nothing");
    }
    let mut found = BTreeMap::new();
    for (name, pos, values) in vars.iter() {
        if !crate::policy::is_env_name(name) {
            errors.push(pos, format!("{name:?} is not an environment variable name"));
        } else if HOLD_THE_STRING
            .iter()
            .any(|it| it.eq_ignore_ascii_case(name))
        {
            errors.push(
                pos,
                format!(
                    "`env` can't name {name}, which holds a secret: a rule would compare to it"
                ),
            );
        }
        let values = strings(name, values, errors, |_, _, _| {});
        found.insert(name.clone(), values);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-25
    #[test]
    fn a_prefix_compares_by_dotted_segment() {
        for (prefix, version) in [
            ("1.25", "1.25"),
            ("1.25", "1.25.14"),
            ("1", "1.25.14"),
            ("24", "24.1.0"),
            ("16.2", "16.2"),
            ("1.25.14", "1.25.14"),
        ] {
            assert!(version_matches(prefix, version), "{prefix} {version}");
        }
        for (prefix, version) in [
            ("1.25", "1.250"),
            ("1.25", "1.2"),
            ("1.2", "1.25"),
            ("1.25.1", "1.25"),
            ("1.25.14", "1.25.1"),
            ("2", "24.1.0"),
            ("", "1.25"),
        ] {
            assert!(!version_matches(prefix, version), "{prefix} {version}");
        }
    }

    // covers: C-25
    #[test]
    fn a_pre_release_isnt_the_release_its_number_names() {
        assert!(!version_matches("1.25", "1.25rc2"));
        assert!(!version_matches("1.94", "1.94.0-nightly"));
        assert!(!version_matches("1", "1.94.0-beta.2"));
        assert!(version_matches("1.94.0-nightly", "1.94.0-nightly"));
        assert!(version_matches("1.25rc2", "1.25rc2"));
    }

    // covers: C-8
    #[test]
    fn a_version_is_written_with_numbers_first() {
        for good in [
            "1",
            "1.25",
            "1.25.14",
            "1.94.0-nightly",
            "1.25rc2",
            "1.94.0-beta.2",
        ] {
            assert!(is_version(good), "{good}");
        }
        for bad in [
            "", "v1.25", "go1.25", ".1", "1.", "1..2", "1.25.x", "latest", "1 2",
        ] {
            assert!(!is_version(bad), "{bad}");
        }
    }

    // covers: C-17, C-25 (ADR 0046: a rule compares, so it can't name the string or what hushrun
    // answers from inside it; the rest of hushrun's variables are a rule's to name)
    #[test]
    fn a_rule_cant_name_what_holds_the_string() {
        let parse = |name: &str| {
            let file = format!(
                "version: 1\nrepository: github.com/acme/app\ncounts_from: {{env: {{{name}: x}}}}\n"
            );
            crate::policy::Policy::parse("hushrun.yaml", file.as_bytes())
        };
        for name in [
            "HUSHRUN_AGENT",
            "hushrun_agent",
            "HUSHRUN_TOKEN",
            "Hushrun_Token",
            "HUSHRUN_MACHINE_KEY",
        ] {
            let errors = parse(name).unwrap_err().to_string();
            assert!(errors.contains(&format!("can't name {name}")), "{errors}");
        }
        for name in ["SANDBOX", "HUSHRUN_PROVIDER", "HUSHRUN_AGENT_NAME"] {
            assert!(parse(name).is_ok(), "{name}");
        }
    }

    // covers: C-9
    #[test]
    fn a_rule_has_one_form_however_it_is_written() {
        let rule = Rule {
            os: vec!["linux".into()],
            tools: BTreeMap::from([("go".to_string(), vec!["1.25".to_string()])]),
            env: BTreeMap::from([("SANDBOX".to_string(), vec!["go-7".to_string()])]),
            ..Rule::default()
        };
        assert_eq!(
            rule.normal(),
            json!({"os": ["linux"], "go": ["1.25"], "env": {"SANDBOX": ["go-7"]}})
        );
    }
}

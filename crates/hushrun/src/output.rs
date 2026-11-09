//! Output formats (§10.2): `text` for people, `json` for programs, `env` for `eval`, and
//! `dotenv` for GitLab's dotenv reports. Logs go to stderr; stdout carries only output.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use clap::ValueEnum;

/// `--format`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum Format {
    #[default]
    Text,
    Json,
    Env,
    Dotenv,
}

/// The most variables a dotenv output may hold (GitLab's default limit, per job).
pub const DOTENV_MAX_VARIABLES: usize = 20;
/// GitLab's dotenv size limit: a report must be smaller than this many bytes.
pub const DOTENV_MAX_BYTES: usize = 5 * 1024;

/// Named output values, in order. Keys are lowercase (`digest`, `ref_image`); the env and
/// dotenv forms prefix and uppercase them (`HUSHRUN_DIGEST`, `HUSHRUN_REF_IMAGE`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outputs {
    values: Vec<(String, String)>,
}

impl Outputs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.values.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.values.push((key, value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// `HUSHRUN_<KEY>='<value>'` lines, shell-quoted for `eval`.
    pub fn to_env(&self) -> String {
        let mut out = String::new();
        for (key, value) in self.iter() {
            let _ = writeln!(out, "{}={}", env_name(key), shell_quote(value));
        }
        out
    }

    /// Unquoted `HUSHRUN_<KEY>=<value>` lines, as GitLab's dotenv reports read them: no blank
    /// lines, comments or quotes, and keys of ASCII letters, digits and `_`. Refuses what they
    /// can't hold: multi-line values, more than 20 variables, 5 KiB or more, and the `json`
    /// key (pass the plan between jobs as a file instead).
    pub fn to_dotenv(&self) -> anyhow::Result<String> {
        let mut out = String::new();
        let mut count = 0;
        for (key, value) in self.iter() {
            if key == "json" {
                continue;
            }
            let name = env_name(key);
            if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                bail!("output {key} has no dotenv name: {name} isn't letters, digits and _");
            }
            if value.contains('\n') || value.contains('\r') {
                bail!("output {key} has several lines, which dotenv can't hold");
            }
            let _ = writeln!(out, "{name}={value}");
            count += 1;
        }
        if count > DOTENV_MAX_VARIABLES {
            bail!("dotenv output has {count} variables; the limit is {DOTENV_MAX_VARIABLES}");
        }
        if out.len() >= DOTENV_MAX_BYTES {
            bail!(
                "dotenv output is {} bytes; GitLab takes less than {DOTENV_MAX_BYTES}",
                out.len()
            );
        }
        Ok(out)
    }

    /// Writes the dotenv form to `path` (`--dotenv`).
    pub fn write_dotenv(&self, path: &Path) -> anyhow::Result<()> {
        let text = self.to_dotenv()?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// `HUSHRUN_` and the key in uppercase.
pub fn env_name(key: &str) -> String {
    format!(
        "HUSHRUN_{}",
        key.chars()
            .map(|c| if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            })
            .collect::<String>()
    )
}

/// Quotes `value` for a POSIX shell.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.,:/@%+=".contains(&b))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Prints `text` to stdout.
pub fn print(text: &str) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.is_empty() && !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;
    Ok(())
}

/// Prints `value` as compact JSON, followed by a newline.
pub fn print_json(value: &serde_json::Value) -> anyhow::Result<()> {
    print(&serde_json::to_string(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_are_shell_quoted() {
        let mut out = Outputs::new();
        out.set("digest", "sha256:abc");
        out.set("reason", "it's done");
        out.set("ref_image", "");
        assert_eq!(
            out.to_env(),
            "HUSHRUN_DIGEST=sha256:abc\nHUSHRUN_REASON='it'\\''s done'\nHUSHRUN_REF_IMAGE=''\n"
        );
    }

    #[test]
    fn dotenv_refuses_what_gitlab_cannot_hold() {
        let mut out = Outputs::new();
        out.set("mode", "promote");
        out.set("json", "{\"a\":1}");
        assert_eq!(out.to_dotenv().unwrap(), "HUSHRUN_MODE=promote\n");
        out.set("reason", "two\nlines");
        assert!(out.to_dotenv().is_err());
        let mut many = Outputs::new();
        for i in 0..21 {
            many.set(format!("k{i}"), "v");
        }
        assert!(many.to_dotenv().is_err());
        let mut big = Outputs::new();
        big.set("x", "y".repeat(DOTENV_MAX_BYTES));
        assert!(big.to_dotenv().is_err());
        // GitLab wants a report strictly under 5 KiB: `HUSHRUN_X=` and a newline are 11 bytes.
        let mut edge = Outputs::new();
        edge.set("x", "y".repeat(DOTENV_MAX_BYTES - 12));
        assert_eq!(edge.to_dotenv().unwrap().len(), DOTENV_MAX_BYTES - 1);
        edge.set("x", "y".repeat(DOTENV_MAX_BYTES - 11));
        assert!(edge.to_dotenv().is_err());
        // Keys GitLab can't hold are refused, not mangled.
        let mut odd = Outputs::new();
        odd.set("ref_web.app", "x");
        assert!(
            odd.to_dotenv()
                .unwrap_err()
                .to_string()
                .contains("dotenv name")
        );
        // Every line is `KEY=value`: an empty value is fine, a blank line never appears.
        let mut empty = Outputs::new();
        empty.set("checks", "");
        empty.set("mode", "current");
        assert_eq!(
            empty.to_dotenv().unwrap(),
            "HUSHRUN_CHECKS=\nHUSHRUN_MODE=current\n"
        );
    }
}

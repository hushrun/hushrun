//! hushrun's own variables, read once the command line is parsed (§10.1): a flag wins, then its
//! variable, then the default. A variable never stops a command, so that a CI setting gone
//! wrong, such as `HUSHRUN_NO_CI_OUTPUT: ${{ vars.X }}` with `X` unset, costs a warning, never
//! the check: an empty one is unset; a boolean is `true` or `false`, `1` or `0`, `yes` or `no`,
//! `on` or `off`, in any case; and a value hushrun can't read is ignored, with one line on
//! stderr. A bad flag is still a usage error: clap refuses it (exit 2).
//!
//! hushrun reads them itself rather than through clap's `env`, which holds a variable to its
//! flag's rules, and rather than by cleaning the process environment first, which takes unsafe
//! code and would change the environment of the command `hushrun run` starts.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use tracing_subscriber::EnvFilter;

use hushrun_vcs::git::ReaderChoice;

use crate::cli::{Cli, Command};
use crate::output::Format;

/// Fills what `cli`'s command line leaves unset from the variables `var` reads, and gives the
/// lines that say which ones hushrun ignored, one each.
pub fn read(cli: &mut Cli, var: impl Fn(&str) -> Option<OsString>) -> Vec<String> {
    let mut vars = Vars {
        var: &var,
        ignored: Vec::new(),
    };
    let global = &mut cli.global;
    // `--config-ref` reads hushrun.yaml from a ref, which no path stands in for (T-5).
    if global.config.is_none() && global.config_ref.is_none() {
        global.config = vars.os("HUSHRUN_CONFIG").map(PathBuf::from);
    }
    global.format = match global.format_flag {
        Some(format) => format,
        None => vars
            .read_as("HUSHRUN_FORMAT", &one_of::<Format>(), |value| {
                <Format as ValueEnum>::from_str(value, true).ok()
            })
            .unwrap_or_default(),
    };
    global.no_ci_output = global.no_ci_output || vars.boolean("HUSHRUN_NO_CI_OUTPUT");
    global.git_reader = match global.git_reader_flag {
        Some(reader) => reader,
        None => vars
            .read_as("HUSHRUN_GIT_READER", "auto, gix or cli", |value| {
                value.to_ascii_lowercase().parse::<ReaderChoice>().ok()
            })
            .unwrap_or_default(),
    };
    // -v and -q set the level themselves.
    if global.verbose == 0 && global.quiet == 0 {
        global.log = vars.read_as(
            "HUSHRUN_LOG",
            "a log filter such as debug or hushrun=trace",
            |value| EnvFilter::try_new(value).is_ok().then(|| value.to_string()),
        );
    }
    if let Command::Plan(plan) = &mut cli.command {
        plan.force_full = plan.force_full || vars.boolean("HUSHRUN_FORCE_FULL");
    }
    // \[ADR 0047\] Unset says nothing: on GitHub Actions, the workflow's steps decide (C-26).
    if let Command::Run(run) = &mut cli.command {
        run.reports_read = vars.yes_or_no("HUSHRUN_REPORTS_READ");
    }
    vars.ignored
}

/// hushrun's line for a variable it ignores, where `why` names it, its value and what it takes.
pub fn ignoring(why: impl fmt::Display) -> String {
    format!("hushrun: {why}, so it's ignored")
}

/// \[ADR 0046\] hushrun's line for what a `HUSHRUN_AGENT` overrode, once: the command a `hushrun run`
/// starts sees the same variables, and has nothing to add.
pub fn overridden(env: &hushrun_ci::Env) -> Option<String> {
    let names = env.overridden();
    let (last, rest) = names.split_last()?;
    if env.get(crate::cmd::setup::variants::INSIDE_A_RUN).is_some() {
        return None;
    }
    let names = match rest {
        [] => (*last).to_string(),
        rest => format!("{} and {last}", rest.join(", ")),
    };
    Some(format!(
        "hushrun: HUSHRUN_AGENT wins over {names}: it holds the backend, the token and the key together"
    ))
}

/// The variables `var` reads, and the lines for those hushrun ignored.
struct Vars<'a> {
    var: &'a dyn Fn(&str) -> Option<OsString>,
    ignored: Vec<String>,
}

impl Vars<'_> {
    /// `name`'s value, unless it is unset or empty.
    fn os(&self, name: &str) -> Option<OsString> {
        (self.var)(name).filter(|value| !value.to_string_lossy().trim().is_empty())
    }

    /// `name`'s value, trimmed, as `parse` reads it; when it can't, nothing, and a line that says
    /// so, with what `name` takes.
    fn read_as<T>(
        &mut self,
        name: &str,
        takes: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Option<T> {
        let value = self.os(name)?;
        let value = value.to_string_lossy();
        let parsed = parse(value.trim());
        if parsed.is_none() {
            self.ignored
                .push(ignoring(format_args!("{name} is {value:?}, not {takes}")));
        }
        parsed
    }

    /// Whether `name` says yes: false when it is unset, and when hushrun can't read it.
    fn boolean(&mut self, name: &str) -> bool {
        self.yes_or_no(name).unwrap_or(false)
    }

    /// What `name` says, yes or no; nothing when it is unset, or when hushrun can't read it.
    fn yes_or_no(&mut self, name: &str) -> Option<bool> {
        self.read_as(name, "true or false", |value| {
            match value.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            }
        })
    }
}

/// What `T` takes, as people read it: `text, json, env or dotenv`.
fn one_of<T: ValueEnum>() -> String {
    let names: Vec<String> = T::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_string())
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use clap::Parser as _;

    use super::*;

    /// `hushrun ARGS` with the variables `vars`, and the lines for those hushrun ignored.
    fn parsed(args: &[&str], vars: &[(&str, &str)]) -> (Cli, Vec<String>) {
        let mut cli = Cli::try_parse_from(std::iter::once("hushrun").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"));
        let vars: BTreeMap<&str, OsString> = vars
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        let ignored = read(&mut cli, |name| vars.get(name).cloned());
        (cli, ignored)
    }

    fn force_full(cli: &Cli) -> bool {
        match &cli.command {
            Command::Plan(plan) => plan.force_full,
            _ => panic!("not plan"),
        }
    }

    #[test]
    fn booleans_read_as_people_write_them_in_ci_settings() {
        for yes in ["true", "TRUE", "1", "yes", "Yes", "on", "ON", " true\n"] {
            let (cli, ignored) = parsed(&["digest"], &[("HUSHRUN_NO_CI_OUTPUT", yes)]);
            assert!(cli.global.no_ci_output, "{yes:?}");
            assert!(ignored.is_empty(), "{yes:?}: {ignored:?}");
            let (cli, ignored) = parsed(&["plan"], &[("HUSHRUN_FORCE_FULL", yes)]);
            assert!(force_full(&cli), "{yes:?}");
            assert!(ignored.is_empty(), "{yes:?}: {ignored:?}");
        }
        // An empty one is unset, as `${{ vars.X }}` gives when X isn't set.
        for no in ["false", "False", "0", "no", "NO", "off", "Off", "", "  "] {
            let (cli, ignored) = parsed(&["digest"], &[("HUSHRUN_NO_CI_OUTPUT", no)]);
            assert!(!cli.global.no_ci_output, "{no:?}");
            assert!(ignored.is_empty(), "{no:?}: {ignored:?}");
        }
        let (cli, ignored) = parsed(
            &["plan"],
            &[
                ("HUSHRUN_NO_CI_OUTPUT", "maybe"),
                ("HUSHRUN_FORCE_FULL", "y"),
            ],
        );
        assert!(!cli.global.no_ci_output);
        assert!(!force_full(&cli));
        assert_eq!(
            ignored,
            [
                "hushrun: HUSHRUN_NO_CI_OUTPUT is \"maybe\", not true or false, so it's ignored",
                "hushrun: HUSHRUN_FORCE_FULL is \"y\", not true or false, so it's ignored",
            ]
        );
        // The flag wins, and the variable isn't read.
        let (cli, ignored) = parsed(
            &["--no-ci-output", "plan", "--force-full"],
            &[
                ("HUSHRUN_NO_CI_OUTPUT", "maybe"),
                ("HUSHRUN_FORCE_FULL", "y"),
            ],
        );
        assert!(cli.global.no_ci_output && force_full(&cli));
        assert!(ignored.is_empty(), "{ignored:?}");
        // HUSHRUN_FORCE_FULL is plan's alone.
        let (_, ignored) = parsed(&["digest"], &[("HUSHRUN_FORCE_FULL", "y")]);
        assert!(ignored.is_empty(), "{ignored:?}");
    }

    #[test]
    fn the_output_format_and_the_git_reader() {
        let (cli, ignored) = parsed(&["digest"], &[]);
        assert_eq!(cli.global.format, Format::Text);
        assert_eq!(cli.global.git_reader, ReaderChoice::Auto);
        assert!(ignored.is_empty());
        let (cli, ignored) = parsed(
            &["digest"],
            &[("HUSHRUN_FORMAT", "JSON"), ("HUSHRUN_GIT_READER", " gix ")],
        );
        assert_eq!(cli.global.format, Format::Json);
        assert_eq!(cli.global.git_reader, ReaderChoice::Gix);
        assert!(ignored.is_empty(), "{ignored:?}");
        let (cli, ignored) = parsed(
            &["digest"],
            &[("HUSHRUN_FORMAT", ""), ("HUSHRUN_GIT_READER", "")],
        );
        assert_eq!(cli.global.format, Format::Text);
        assert_eq!(cli.global.git_reader, ReaderChoice::Auto);
        assert!(ignored.is_empty(), "{ignored:?}");
        let (cli, ignored) = parsed(
            &["digest"],
            &[("HUSHRUN_FORMAT", "xml"), ("HUSHRUN_GIT_READER", "jgit")],
        );
        assert_eq!(cli.global.format, Format::Text);
        assert_eq!(cli.global.git_reader, ReaderChoice::Auto);
        assert_eq!(
            ignored,
            [
                "hushrun: HUSHRUN_FORMAT is \"xml\", not text, json, env or dotenv, so it's ignored",
                "hushrun: HUSHRUN_GIT_READER is \"jgit\", not auto, gix or cli, so it's ignored",
            ]
        );
        let (cli, ignored) = parsed(
            &["--format", "env", "--git-reader", "cli", "digest"],
            &[("HUSHRUN_FORMAT", "xml"), ("HUSHRUN_GIT_READER", "jgit")],
        );
        assert_eq!(cli.global.format, Format::Env);
        assert_eq!(cli.global.git_reader, ReaderChoice::Cli);
        assert!(ignored.is_empty(), "{ignored:?}");
    }

    #[test]
    fn the_config_path() {
        let config = |args: &[&str], value: &str| {
            let (cli, ignored) = parsed(args, &[("HUSHRUN_CONFIG", value)]);
            assert!(ignored.is_empty(), "{ignored:?}");
            cli.global.config
        };
        assert_eq!(
            config(&["digest"], "ci/hushrun.yaml").as_deref(),
            Some(Path::new("ci/hushrun.yaml"))
        );
        assert_eq!(config(&["digest"], ""), None);
        assert_eq!(config(&["digest"], " "), None);
        assert_eq!(
            config(&["--config", "other.yaml", "digest"], "ci/hushrun.yaml").as_deref(),
            Some(Path::new("other.yaml"))
        );
        // A pull request's pipeline reads hushrun.yaml from the branch it merges into: the
        // variable doesn't conflict with --config-ref, and doesn't replace it.
        assert_eq!(
            config(
                &["--config-ref", "origin/main", "digest"],
                "ci/hushrun.yaml"
            ),
            None
        );
    }

    #[test]
    fn the_log_filter() {
        let log = |args: &[&str], value: &str| {
            let (cli, ignored) = parsed(args, &[("HUSHRUN_LOG", value)]);
            (cli.global.log, ignored)
        };
        assert_eq!(log(&["digest"], "debug"), (Some("debug".into()), vec![]));
        // Empty, it is unset: the default level, not a filter that shows nothing.
        assert_eq!(log(&["digest"], ""), (None, vec![]));
        assert_eq!(
            log(&["digest"], "hushrun=loud"),
            (
                None,
                vec![
                    "hushrun: HUSHRUN_LOG is \"hushrun=loud\", not a log filter such as debug or hushrun=trace, so it's ignored"
                        .to_string()
                ]
            )
        );
        // -v and -q set the level, and the variable isn't read.
        assert_eq!(log(&["-v", "digest"], "hushrun=loud"), (None, vec![]));
        assert_eq!(log(&["-q", "digest"], "debug"), (None, vec![]));
    }

    #[test]
    fn a_flag_hushrun_cant_read_is_still_a_usage_error() {
        for args in [
            &["hushrun", "--format", "xml", "digest"][..],
            &["hushrun", "--format", "", "digest"],
            &["hushrun", "--config", "", "digest"],
            &["hushrun", "--git-reader", "jgit", "digest"],
            &["hushrun", "--no-ci-output=1", "digest"],
            &[
                "hushrun",
                "--config",
                "a.yaml",
                "--config-ref",
                "origin/main",
                "digest",
            ],
        ] {
            let err = Cli::try_parse_from(args).expect_err("a usage error");
            assert_eq!(err.exit_code(), 2, "{args:?}");
        }
    }
}

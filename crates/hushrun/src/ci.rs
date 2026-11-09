//! Writing to the CI runner (§10.2). On GitHub Actions, `plan`, `digest`, `artifact`,
//! `attest` and `guard` also append their outputs to `$GITHUB_OUTPUT`, write a Markdown step
//! summary and annotate the run, unless `--no-ci-output` is given.

use hushrun_ci::Env;
use hushrun_ci::github::{self, Level};
use hushrun_core::run::Provider;

use crate::app::App;
use crate::cli::GlobalOpts;
use crate::exit::Failure;
use crate::output::Outputs;

/// The runner a command reports to, if any.
pub struct Ci(Option<github::Runner>);

impl Ci {
    pub fn of(app: &App<'_>) -> Ci {
        Ci::new(app.run.provider, app.global.no_ci_output, &app.env)
    }

    /// For commands that run without an [`App`] (`digest`).
    pub fn from_process(global: &GlobalOpts) -> Ci {
        let env = Env::from_process();
        Ci::new(hushrun_ci::detect(&env), global.no_ci_output, &env)
    }

    fn new(provider: Provider, off: bool, env: &Env) -> Ci {
        Ci((provider == Provider::Github && !off).then(|| github::Runner::from_env(env)))
    }

    /// Appends `outputs` and a Markdown `summary` for the runner.
    pub fn report(&self, outputs: &Outputs, summary: &str) -> Result<(), Failure> {
        let Some(runner) = &self.0 else {
            return Ok(());
        };
        runner
            .write_outputs(outputs.iter())
            .and_then(|()| runner.write_summary(summary))
            .map_err(|e| Failure::from(anyhow::anyhow!("writing the GitHub step outputs: {e}")))
    }

    /// \[ADR 0048\] Appends `outputs` alone, as `hushrun run` writes them on every run (C-27).
    pub fn outputs(&self, outputs: &Outputs) -> std::io::Result<()> {
        match &self.0 {
            Some(runner) => runner.write_outputs(outputs.iter()),
            None => Ok(()),
        }
    }

    /// \[ADR 0048\] Appends Markdown to the step summary alone, as a skip does (C-27).
    pub fn summary(&self, markdown: &str) -> std::io::Result<()> {
        match &self.0 {
            Some(runner) => runner.write_summary(markdown),
            None => Ok(()),
        }
    }

    /// Whether this command annotates the runner's run: on GitHub Actions, without
    /// `--no-ci-output`.
    pub fn annotates(&self) -> bool {
        self.0.is_some()
    }

    /// A run annotation (`::notice`, `::warning`, `::error`), on stderr.
    pub fn annotate(&self, level: Level, title: &str, message: &str) {
        if self.0.is_some() {
            eprintln!("{}", github::annotation(level, title, message));
        }
    }
}

/// Escapes text for a Markdown table cell.
pub fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

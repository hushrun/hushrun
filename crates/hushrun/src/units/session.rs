//! \[ADR 0068\] One run's observing as `hushrun run` does it: the [`Observer`] and the adapter
//! [`adapter::choose_kept`] chose, each step of the observer around one call of the adapter
//! through its trait object. Apart from `mod.rs`, so that a test that drives one adapter's steps
//! through the [`Observer`] has none of this file's code in what it reached (CONTRIBUTING.md,
//! "Which tests CI runs again").

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use hushrun_core::units::{KeyBase, State, Tests, Tree};

use super::adapter::{self, Adapter};
use super::{Again, Finish, Observed, Observer, PlanInput, Planned, Profile, pytest, store};

/// What starting to observe a run came to.
pub enum Start {
    /// `tests` is off: nothing changes.
    Off,
    /// No adapter can observe this COMMAND, and why, which the run says once.
    Not(String),
    On(Box<Session>),
}

/// One run's observing, as `hushrun run` does it: the [`Observer`], and the adapter
/// [`adapter::choose_kept`] chose, whose every step goes through the trait object. Each method is
/// the observer's step around one call of the adapter.
pub struct Session {
    observer: Observer,
    adapter: Box<dyn Adapter>,
}

impl Session {
    /// Starts observing a run of `command` in `cwd`, in the repository at `root`, for a check
    /// whose `tests` are `tests`: `Off` without them, and then nothing is written or set.
    /// \[C-32\] `stored`: the adapter that last reported the check's test files here, as this
    /// machine's store names it ([`super::db::adapter_of`]), which keeps a COMMAND pytest reported
    /// pytest's ([`adapter::choose_kept`]).
    pub fn start(
        tests: Option<&Tests>,
        command: &[String],
        root: &Path,
        cwd: &Path,
        stored: Option<&str>,
    ) -> Start {
        let Some(tests) = tests.filter(|t| t.is_on()) else {
            return Start::Off;
        };
        let Some(cache) = crate::context::cache_dir() else {
            return Start::Not("there is no cache directory".into());
        };
        let adapter = match adapter::choose_kept(command, cwd, stored) {
            Ok(adapter) => adapter,
            Err(why) => return Start::Not(why),
        };
        // \[C-32\] Whether the stored word chose pytest where CTest or Swift would take COMMAND
        // now: then a run in which pytest reports nothing clears it.
        let kept = adapter.word() == pytest::WORD
            && adapter::choose(command, cwd).is_ok_and(|fresh| fresh.word() != pytest::WORD);
        let profile = Profile::of(adapter.as_ref());
        let mut observer = match Observer::open(tests, profile, &cache, command, root, cwd, kept) {
            Ok(observer) => observer,
            Err(why) => return Start::Not(why),
        };
        let env = |name: &str| observer.env(name);
        let variables = adapter.variables(&observer.start_at(&env));
        if let Err(why) = observer.started(variables) {
            return Start::Not(why);
        }
        Start::On(Box::new(Session { observer, adapter }))
    }

    /// B-2: [`Observer::compile_with`].
    pub fn compile_with(&mut self, env: Vec<(OsString, OsString)>) {
        self.observer.compile_with(env);
    }

    /// \[C-39\] [`Observer::notes`].
    pub fn notes(&self) -> &[String] {
        self.observer.notes()
    }

    /// \[C-39\] [`Observer::test_file`].
    pub fn test_file(&self) -> &'static str {
        self.observer.test_file()
    }

    /// Gives COMMAND the variables that load the adapter.
    pub fn apply(&self, command: &mut Command) {
        let start = self.adapter.start_variables(self.observer.run_dir());
        self.observer.apply(command, &start);
    }

    /// \[C-46\] [`Observer::learned`].
    pub fn learned(&self, base: &KeyBase, repository: Option<&str>) {
        self.observer.learned(base, repository);
    }

    /// \[ADR 0069\] The plan (RFC 0003 §3.5 step 1), for a check in `reuse`, before COMMAND:
    /// [`Observer::plan_context`], the adapter's [`Adapter::plan`], and [`Observer::planned`].
    pub fn plan(&mut self, input: &PlanInput<'_>) -> Planned {
        let context = match self.observer.plan_context(input) {
            Ok(context) => context,
            Err(planned) => return planned,
        };
        let planning = self.adapter.plan(&self.observer.plan_at(input, &context));
        self.observer.planned(input, context, planning)
    }

    /// \[C-39\] [`Observer::plan_notes`].
    pub fn plan_notes(&self) -> &[String] {
        self.observer.plan_notes()
    }

    /// What the adapter saw, decided: [`Observer::read_context`], the adapter's
    /// [`Adapter::read`], and [`Observer::finished`]. `None` when the adapter wrote nothing, so
    /// that everything ran and no unit is recorded.
    pub fn finish(&self, end: &Finish<'_>) -> Option<Observed> {
        let context = match self.observer.read_context(end) {
            Ok(context) => context,
            Err(observed) => return observed.map(|observed| *observed),
        };
        let read = self.adapter.read(&self.observer.read_at(end, &context));
        self.observer.finished(end, context, read)
    }

    /// \[C-32\] [`Observer::whole_again`], with what the adapter's [`Adapter::again`] says of a
    /// run that listed no test file.
    pub fn whole_again(&mut self) -> Result<(), String> {
        let extras =
            self.adapter
                .again(self.observer.root(), self.observer.cwd(), &BTreeSet::new());
        self.observer.whole_again(extras)
    }

    /// \[C-34, ADR 0069\] [`Observer::again`], with the adapter's [`Adapter::again`] and
    /// [`Adapter::start_variables`].
    pub fn again(&self, observed: &Observed, again: &BTreeSet<String>) -> Option<Again> {
        let extras = self
            .adapter
            .again(self.observer.root(), &observed.cwd, &observed.unit_files);
        let again = self.observer.again(observed, again, extras)?;
        let start = self.adapter.start_variables(again.dir());
        Some(again.with_start(start))
    }

    /// \[C-34\] [`Observer::again_ended`], with what the adapter's [`Adapter::read`] made of the
    /// second run's files.
    pub fn again_ended(
        &self,
        again: &Again,
        observed: &Observed,
        tree: &Tree,
        passed: bool,
    ) -> BTreeMap<String, State> {
        let files = again.files();
        let map = store::Map::default();
        let read = self
            .adapter
            .read(
                &self
                    .observer
                    .again_read_at(&files, observed, tree, passed, &map),
            )
            .ok()
            .flatten();
        self.observer.again_ended(again, observed, passed, read)
    }
}

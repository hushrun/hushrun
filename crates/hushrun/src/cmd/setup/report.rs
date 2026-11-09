//! `hushrun init --dry-run --format json` (C-15): the plan as one document,
//! `hushrun.init-plan/v1`, made from the data init's text is made of, never from the text. Its
//! field names are a contract.
//!
//! A command's `ci` predicts what `hushrun run` decides in its job, given a pass of that job on
//! record: it is the run's own rule (ADR 0044), applied to an environment like the job's and to the
//! files as init would leave them, so the two can't drift. A workflow can change after init, so the
//! run decides from the file it runs in, and this only predicts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use hushrun_ci::Env;

use super::projects::{self, Layout};
use super::scripts::{self, Decision};
use super::started::Ran;
use super::variants::{self, Matrix};
use super::workflows::{self, Host, Job, Step, Workflow};
use super::{Look, checks, readers, script_check};

pub const SCHEMA: &str = "hushrun.init-plan/v1";

/// What init would write and say, beside what [`Look`] found.
pub struct Writes {
    /// Each file it writes, from the repository root, `/`-separated, and its new text: in the
    /// order it writes them.
    pub files: Vec<(String, String)>,
    /// The files for coding agents (C-24), and what each is for: listed, not diffed, since their
    /// text is hushrun's own.
    pub agents: Vec<(String, String)>,
    /// What the text says beside its lists, a sentence each.
    pub notes: Vec<String>,
}

/// The plan.
pub fn document(look: &Look, writes: &Writes) -> Value {
    let written: BTreeMap<&str, &str> = writes
        .files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    json!({
        "schema": SCHEMA,
        "tool": format!("hushrun {}", env!("CARGO_PKG_VERSION")),
        "repository": look.repository,
        "scopes": scopes(&look.proposal.layout),
        "scripts": package_scripts(look),
        "jobs": jobs(look, &written),
        "suggestions": suggested(look),
        "edits": edits(&look.root, &writes.files),
        "agentFiles": writes.agents.iter().map(|(path, change)| json!({"path": path, "change": change})).collect::<Vec<_>>(),
        "notes": writes.notes,
    })
}

fn scopes(layout: &Layout) -> Value {
    match layout {
        Layout::One(why) => json!([{"name": "default", "why": why}]),
        Layout::Projects(projects) => projects
            .iter()
            .map(|project| {
                json!({
                    "name": project.name,
                    "why": null,
                    "dir": project.dir,
                    "include": project.include,
                    "exclude": project.exclude,
                    "external": project.external,
                })
            })
            .collect(),
        Layout::Declared(dirs) => {
            let mut names: Vec<&String> = dirs.values().collect();
            names.sort();
            names.dedup();
            names
                .into_iter()
                .map(|name| json!({"name": name, "why": "hushrun.yaml declares it"}))
                .collect()
        }
    }
}

/// \[ADR 0050\] The test scripts a person could wrap, each with the line to use: init applies none.
fn suggested(look: &Look) -> Vec<Value> {
    look.proposal
        .suggestions
        .iter()
        .map(|suggestion| {
            json!({
                "file": look.workflows[suggestion.file].path,
                "job": suggestion.job,
                "line": suggestion.line + 1,
                "dir": suggestion.dir,
                "check": suggestion.check,
                "before": suggestion.before,
                "after": suggestion.after,
                "setup": super::suggestion_setup(look, suggestion),
                "why": suggestion.why,
            })
        })
        .collect()
}

/// package.json's scripts init looks at: wrapped, already through hushrun, or left. \[ADR 0051\]
/// Then each workspace package's, by its package.json.
fn package_scripts(look: &Look) -> Value {
    let (mut wrapped, mut already, mut left) = (Vec::new(), Vec::new(), Vec::new());
    let root = look
        .package
        .iter()
        .map(|package| ("package.json".to_string(), package));
    let members = look
        .members
        .iter()
        .map(|(dir, member)| (format!("{dir}/package.json"), member));
    for (path, check) in root.chain(members).flat_map(|(path, package)| {
        package
            .checks
            .iter()
            .map(move |check| (path.clone(), check))
    }) {
        match &check.decision {
            Decision::Wrap { command, .. } => wrapped.push(json!({
                "package": path,
                "script": check.name,
                "check": checks_of(&check.name, command).first(),
                "before": check.command,
                "after": command,
            })),
            Decision::Wrapped => already.push(json!({
                "package": path,
                "script": check.name,
                "check": checks_of(&check.name, &check.command).first(),
                "command": check.command,
            })),
            Decision::Leave(why) => {
                let mut entry = json!({"package": path, "script": check.name, "why": why});
                // \[ADR 0050\] Only a script left to its tool's remote cache says so: every other
                // entry stays as it was.
                if let Some(aside) = aside(why) {
                    entry["ciWhy"] = json!(aside);
                }
                left.push(entry);
            }
        }
    }
    json!({"wrapped": wrapped, "already": already, "left": left})
}

/// \[ADR 0050\] What makes CI run, as it is, a command or script init leaves for `why`:
/// `stepped-aside` when its tool's remote cache keeps its results, else nothing.
fn aside(why: &str) -> Option<&'static str> {
    super::caches::stepped_aside(why).then_some("stepped-aside")
}

/// The checks the script `name` runs through hushrun when its command is `command`, in order.
fn checks_of(name: &str, command: &str) -> Vec<String> {
    let mut checks = Vec::new();
    for run in scripts::runs(command).unwrap_or_default() {
        if let Some(check) = script_check(name, &run).map(|c| c.as_str().to_string())
            && !checks.contains(&check)
        {
            checks.push(check);
        }
    }
    checks
}

/// The jobs that run a check, wrapped, left or through hushrun already. A file no push or pull
/// request starts holds none init wraps or leaves, but its jobs may run a wrapped script, as the
/// text's `workflows` lines say.
fn jobs(look: &Look, written: &BTreeMap<&str, &str>) -> Vec<Value> {
    let read = |path: &str| -> Option<Vec<u8>> {
        match written.get(path) {
            Some(text) => Some(text.as_bytes().to_vec()),
            None => std::fs::read(look.root.join(path)).ok(),
        }
    };
    // \[ADR 0051\] Every package.json's scripts, the workspace packages' too.
    let index_of_scripts = look.script_index();
    // init edits CI's files only where hushrun.yaml can say where results go (C-15): what it
    // would wrap there otherwise, it leaves.
    let left_as_is = super::ci_left_as_is(look);
    let wraps = look.proposal.wrapped();
    let calls = Calls::of(&look.workflows);
    let repository = github_repository(look.repository.as_deref());
    let mut jobs = Vec::new();
    for (index, file) in look.workflows.iter().enumerate() {
        let yaml = (file.host == Host::GitHub)
            .then(|| variants::parse(&file.path, file.source().as_bytes()))
            .flatten();
        // \[ADR 0049\] The file as init leaves it, whose jobs' env may say which variant each run is.
        let as_left = (file.host == Host::GitHub)
            .then(|| read(&file.path).and_then(|bytes| variants::parse(&file.path, &bytes)))
            .flatten();
        // \[ADR 0047\] GitLab's jobs say they read reports in their variables, as init leaves them.
        let gitlab = (file.host == Host::GitLab)
            .then(|| read(&file.path).and_then(|bytes| variants::parse(&file.path, &bytes)))
            .flatten();
        let started = match file.host {
            Host::GitHub => calls.started(&file.path),
            Host::GitLab => Started::Itself,
        };
        for job in &file.jobs {
            let reports_read = gitlab
                .as_ref()
                .and_then(|file| says_reports_read(file, &job.id));
            let here = InJob {
                file,
                job,
                started: &started,
                repository: &repository,
                read: &read,
                reports_skip: &look.reports_skip,
                reports_read,
            };
            // \[ADR 0050\] Each in its order: the job's own lines first, by line, then those of the
            // composite actions it uses, file by file. Such a line says its action.
            let mut commands: Vec<((usize, usize), Value)> = Vec::new();
            let action = |entry: &mut Value, at: usize| {
                if at != index {
                    entry["action"] = json!(look.workflows[at].path);
                }
            };
            for wrap in wraps
                .iter()
                .filter(|w| w.job_file == index && w.job == job.id)
            {
                let order = (usize::from(wrap.file != index) * wrap.file, wrap.line);
                if let Some(why) = left_as_is {
                    let mut entry = left(Some(wrap.line), &wrap.check, &wrap.before, why);
                    action(&mut entry, wrap.file);
                    commands.push((order, entry));
                    continue;
                }
                let mut entry = json!({
                    "line": wrap.line + 1,
                    "dir": wrap.dir,
                    "check": wrap.check,
                    "status": "wrapped",
                    "before": wrap.before,
                    "after": wrap.after,
                    "why": null,
                });
                // init writes `hushrun run <name> --`, so the line names its check.
                let words: Vec<&str> = wrap.before.split_whitespace().collect();
                extend(
                    &mut entry,
                    here.decides(&wrap.check, true, None, false, &words, Some(&wrap.dir)),
                );
                action(&mut entry, wrap.file);
                commands.push((order, entry));
            }
            for l in look
                .proposal
                .left
                .iter()
                .filter(|l| l.job_file == file.path && l.job == job.id)
            {
                let at = look
                    .workflows
                    .iter()
                    .position(|other| other.path == l.file)
                    .unwrap_or(index);
                let mut entry = json!({
                    "line": l.line.map(|line| line + 1),
                    "dir": null,
                    "check": l.check,
                    "status": "left",
                    "before": l.command,
                    "after": null,
                    "why": l.why,
                    "ci": null,
                    "ciWhy": aside(&l.why),
                    "ciText": null,
                });
                action(&mut entry, at);
                let order = (usize::from(at != index) * at, l.line.unwrap_or(usize::MAX));
                commands.push((order, entry));
            }
            let in_actions = workflows::in_actions(job, &look.workflows);
            let found_here = already(&job.steps).into_iter().map(|found| (index, found));
            let found_in_actions = in_actions.iter().flat_map(|step| {
                already([step.step])
                    .into_iter()
                    .map(|found| (step.file, found))
            });
            for (at, found) in found_here.chain(found_in_actions) {
                let order = (usize::from(at != index) * at, found.line);
                let run = match found.run {
                    Ok(run) => run,
                    // Only the run knows its key: CI fills in its words.
                    Err(why) => {
                        let mut entry = json!({
                            "line": found.line + 1, "dir": found.dir, "check": null,
                            "status": "already", "before": null, "after": null,
                            "why": format!("its key can't be told here: {why}"),
                            "ci": null, "ciWhy": null, "ciText": null,
                        });
                        action(&mut entry, at);
                        commands.push((order, entry));
                        continue;
                    }
                };
                let program = run.command.first().map(String::as_str).unwrap_or_default();
                let check = match run.name.as_deref() {
                    Some(name) => Some(name.to_string()),
                    None => crate::cmd::wrap::derived_name(None, program)
                        .map(|c| c.as_str().to_string()),
                };
                let mut entry = json!({
                    "line": found.line + 1,
                    "dir": found.dir,
                    "check": check,
                    "status": "already",
                    "before": null,
                    "after": null,
                    "why": found.outside,
                    "command": run.command.join(" "),
                });
                match (&check, &found.outside) {
                    (Some(_), Some(_)) => extend(&mut entry, cant_tell()),
                    (Some(check), None) => {
                        let words: Vec<&str> = run.command.iter().map(String::as_str).collect();
                        extend(
                            &mut entry,
                            here.decides(
                                check,
                                run.name.is_some(),
                                None,
                                false,
                                &words,
                                found.dir.as_deref(),
                            ),
                        );
                    }
                    (None, _) => extend(&mut entry, (Value::Null, Value::Null, Value::Null)),
                }
                action(&mut entry, at);
                commands.push((order, entry));
            }
            commands.sort_by_key(|(order, _)| *order);
            // \[ADR 0050\] A job that uses an action twice runs its lines twice: they show once.
            commands.dedup();
            let inside = started_inside_a_run(
                job.steps
                    .iter()
                    .chain(in_actions.iter().map(|step| step.step)),
            );
            let mut runs_scripts = Vec::new();
            // \[ADR 0051\] The wrapped scripts it starts: the root's its lines name, first, as
            // before, then those these start and the workspace packages' it starts.
            let order = look.job_scripts(index, job);
            // What lines through hushrun start, whose own run decides (ADR 0044).
            let inside_runs: BTreeSet<Ran> = job
                .steps
                .iter()
                .filter_map(|step| Some((step, step.dir.as_deref().ok()?)))
                .flat_map(|(step, dir)| {
                    scripts::line_runs(&step.commands)
                        .into_iter()
                        .filter_map(|run| run.run.ok())
                        .flat_map(|run| index_of_scripts.started(&run.command.join(" "), dir))
                        .collect::<Vec<_>>()
                })
                .collect();
            for ran in &order {
                let script = &ran.script;
                let Some(check) = look
                    .package_at(&ran.dir)
                    .into_iter()
                    .flat_map(|package| &package.checks)
                    .find(|check| &check.name == script)
                else {
                    continue;
                };
                // Where the steps that start it run: none of them in the checkout, and a run there
                // can't tell which code it is.
                let steps: Vec<_> = job
                    .steps
                    .iter()
                    .filter(|step| match ran.dir.is_empty() {
                        true => scripts::scripts_run(&step.commands).contains(script),
                        false => step.dir.as_deref().is_ok_and(|dir| {
                            index_of_scripts.started(&step.commands, dir).contains(ran)
                        }),
                    })
                    .collect();
                let outside = steps
                    .iter()
                    .map(|step| step.outside_the_checkout())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|whys| whys.first().map(|why| (*why).to_string()));
                let inside =
                    inside.contains(script) && ran.dir.is_empty() || inside_runs.contains(ran);
                for run in check.runs() {
                    let Some(name) = script_check(script, &run) else {
                        continue;
                    };
                    let mut entry = json!({"script": script, "check": name.as_str()});
                    if !ran.dir.is_empty() {
                        entry["package"] = json!(ran.package());
                    }
                    // \[ADR 0051\] Under turbo, a turbo.json init can't edit keeps hushrun's
                    // variables from it.
                    let kept = look
                        .turbo_keeps_from(index, &job.id, ran)
                        .then_some(TURBO_KEEPS);
                    match outside.as_deref().or(kept) {
                        Some(why) => {
                            extend(&mut entry, cant_tell());
                            entry["why"] = json!(why);
                        }
                        None => extend(
                            &mut entry,
                            here.decides(
                                name.as_str(),
                                run.name.is_some(),
                                Some(script),
                                inside,
                                &run.command.iter().map(String::as_str).collect::<Vec<_>>(),
                                Some(&ran.dir),
                            ),
                        ),
                    }
                    runs_scripts.push(entry);
                }
            }
            if commands.is_empty() && runs_scripts.is_empty() {
                continue;
            }
            let job_yaml = yaml.as_ref().map(|file| &file["jobs"][&job.id]);
            let several = match (file.host, job_yaml) {
                (Host::GitLab, _) => Some(job.matrix),
                (Host::GitHub, Some(job)) => Some(variants::matrix_of(job) != Matrix::One),
                (Host::GitHub, None) => None,
            };
            // \[ADR 0049\] Its own env, as init leaves it, says which run it is: its variant, in a
            // job of several, and its file, in a workflow another calls. With both a name and the
            // variable, the variable decides. GitLab names each instance apart itself.
            let called = calls.called.contains(&file.path);
            let says = as_left.as_ref().is_some_and(|left| {
                let job = &left["jobs"][&job.id];
                (several == Some(true) || called)
                    && (several != Some(true) || variants::sets_variant(job))
                    && (!called || variants::names_its_file(job, &file.path))
            });
            let identity = match (several, job_yaml) {
                (Some(true), _) if file.host == Host::GitLab => "named",
                _ if says => "env",
                (Some(true), Some(job)) if variants::names_its_variants(job) => "named",
                _ => "none",
            };
            let setup = if left_as_is.is_none() && look.proposal.installs_in(index, &job.id) {
                "added"
            } else if job.installs_hushrun {
                "present"
            } else {
                "none"
            };
            // \[ADR 0047\] The steps that read reports, as the run finds them.
            let readers: Vec<Value> = match (file.host, job_yaml) {
                (Host::GitHub, Some(job)) => readers::readers(job, &read)
                    .into_iter()
                    .map(|reader| {
                        json!({
                            "step": reader.step,
                            "name": reader.name,
                            "kinds": reader.kinds.iter().map(|kind| kind.word()).collect::<Vec<_>>(),
                            "paths": reader.paths,
                            "onlyOnFailure": reader.only_on_failure,
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            jobs.push(json!({
                "file": file.path,
                "job": job.id,
                "provider": match file.host { Host::GitHub => "github", Host::GitLab => "gitlab" },
                "variants": match several { Some(true) => "several", Some(false) => "one", None => "unknown" },
                "variantIdentity": identity,
                "calledBy": calls.callers_of(&file.path),
                "setup": setup,
                "commands": commands.into_iter().map(|(_, entry)| entry).collect::<Vec<_>>(),
                "scripts": runs_scripts,
                "readers": readers,
            }));
        }
    }
    jobs
}

/// \[ADR 0047\] The `HUSHRUN_REPORTS_READ` that GitLab's job `id` of `file` runs with: its own
/// variables' value, else the file's, unless the job doesn't inherit them.
pub(super) fn reports_read_value<'a>(file: &'a Value, id: &str) -> Option<&'a Value> {
    const NAME: &str = "HUSHRUN_REPORTS_READ";
    let job = &file[id];
    let inherits = match &job["inherit"]["variables"] {
        Value::Bool(yes) => *yes,
        Value::Array(names) => names.iter().any(|name| name == NAME),
        _ => true,
    };
    let value = job["variables"]
        .get(NAME)
        .or_else(|| inherits.then(|| file["variables"].get(NAME)).flatten())?;
    // Its long form keeps it in `value`.
    Some(value.get("value").unwrap_or(value))
}

/// \[ADR 0047\] What that value says, as a run reads the variable (§10.1).
pub(super) fn says_reports_read(file: &Value, id: &str) -> Option<bool> {
    let text = match reports_read_value(file, id)? {
        Value::Bool(yes) => return Some(*yes),
        Value::String(text) => text.trim().to_ascii_lowercase(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    match text.as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// \[ADR 0047\] init's line for each check CI runs every time, since a later step reads its
/// reports, then what Codecov's own file means for them: from the plan's own predictions, so that
/// the text and the JSON say the same.
pub fn every_time(look: &Look, writes: &Writes) -> Vec<String> {
    let written: BTreeMap<&str, &str> = writes
        .files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for job in jobs(look, &written) {
        let gitlab = job["provider"] == "gitlab";
        let entries = job["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["status"] == "wrapped");
        let scripts = job["scripts"].as_array().into_iter().flatten();
        for entry in entries.chain(scripts).filter(|e| e["ciWhy"] == "reports") {
            let check = entry["check"].as_str().unwrap_or_default();
            let text = entry["ciText"].as_str().unwrap_or_default();
            let reader = match gitlab {
                // A GitLab job says it in its variables, for what its artifacts keep.
                true => gitlab_reader(look, &written, &job),
                false => text
                    .strip_prefix("a later step reads its reports (")
                    .and_then(|rest| rest.strip_suffix("), and a skip writes none"))
                    .map(str::to_string),
            };
            let Some(reader) = reader else {
                continue;
            };
            let line = format!(
                "CI runs {check} every time: a later step reads its reports ({reader}). Laptops and agents skip it on a pass."
            );
            if !lines.contains(&line) {
                lines.push(line);
            }
            if !gitlab && reader == "codecov/codecov-action" {
                for note in codecov_notes(look, &written, &job, check) {
                    if !lines.contains(&note) {
                        lines.push(note);
                    }
                }
            }
        }
    }
    lines
}

/// Where Codecov finds its file, first to last.
const CODECOV_FILES: &[&str] = &[
    "codecov.yml",
    ".codecov.yml",
    ".github/codecov.yml",
    ".github/.codecov.yml",
    "dev/codecov.yml",
    "dev/.codecov.yml",
];

/// \[ADR 0048\] How to let CI skip `check`, which CI runs every time for a Codecov upload in
/// `job`, and when that is safe (C-26); \[ADR 0047\] then what Codecov's file means for it:
/// Codecov waits for a number of uploads (`after_n_builds`), and a flag that doesn't carry forward
/// loses its coverage on a run that sends none. init never writes `reports.ci: skip` itself.
fn codecov_notes(
    look: &Look,
    written: &BTreeMap<&str, &str>,
    job: &Value,
    check: &str,
) -> Vec<String> {
    let read = |path: &str| match written.get(path) {
        Some(text) => Some(text.as_bytes().to_vec()),
        None => std::fs::read(look.root.join(path)).ok(),
    };
    let Some(file) = job["file"]
        .as_str()
        .and_then(|path| Some((path, read(path)?)))
        .and_then(|(path, bytes)| variants::parse(path, &bytes))
    else {
        return Vec::new();
    };
    let steps = file["jobs"][job["job"].as_str().unwrap_or_default()]["steps"].clone();
    // A skip says so in its step's outputs, which the upload can wait on.
    let (id, with_id) = match step_id(&steps, check) {
        Some(id) => (id, String::new()),
        None => {
            let id = id_for(check);
            let with = format!(", with id: {id} on the step that runs {check}");
            (id, with)
        }
    };
    let mut notes = vec![format!(
        "To let CI skip {check} too, set reports.ci: skip for it in hushrun.yaml and give the Codecov step if: steps.{id}.outputs.hushrun-skipped != 'true'{with_id}. That is safe when Codecov carries the step's flag forward (carryforward: true) and after_n_builds doesn't count its upload."
    )];
    let Some((name, config)) = CODECOV_FILES.iter().find_map(|name| {
        let bytes = read(name)?;
        Some((*name, variants::parse(name, &bytes)?))
    }) else {
        return notes;
    };
    let flags: Vec<String> = steps
        .as_array()
        .into_iter()
        .flatten()
        .filter(|step| {
            step["uses"]
                .as_str()
                .and_then(|uses| uses.split('@').next())
                .is_some_and(|action| action.eq_ignore_ascii_case("codecov/codecov-action"))
        })
        .filter_map(|step| step["with"]["flags"].as_str())
        .flat_map(|flags| flags.split(','))
        .map(|flag| flag.trim().to_string())
        .filter(|flag| !flag.is_empty() && !flag.contains("${{"))
        .collect();
    let waits = |value: &Value| value.as_u64().filter(|uploads| *uploads > 1);
    if let Some(uploads) = waits(&config["codecov"]["notify"]["after_n_builds"]) {
        notes.push(format!(
            "{name} sets codecov.notify.after_n_builds: {uploads}, so Codecov waits for {uploads} uploads before it reports: {check} uploads one each time CI runs it."
        ));
    }
    let defaults = config["flag_management"]["default_rules"]["carryforward"] == true;
    for flag in &flags {
        if let Some(uploads) = waits(&config["flags"][flag.as_str()]["after_n_builds"]) {
            notes.push(format!(
                "{name} sets after_n_builds: {uploads} for the flag {flag}, so Codecov waits for {uploads} of its uploads before it reports."
            ));
        }
        let individual = config["flag_management"]["individual_flags"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["name"].as_str() == Some(flag.as_str()));
        let carried = config["flags"][flag.as_str()]["carryforward"] == true
            || individual.map_or(defaults, |entry| {
                entry["carryforward"] == true || (defaults && entry["carryforward"] != false)
            });
        if !carried {
            notes.push(format!(
                "Letting CI skip {check} would need carryforward: true for the flag {flag} in {name}, so that Codecov keeps the flag's coverage when a run sends none."
            ));
        }
    }
    notes
}

/// The `id:` of the step of `steps` whose commands run `check` through hushrun, when it has one.
fn step_id(steps: &Value, check: &str) -> Option<String> {
    let runs = |run: &str| {
        run.lines().any(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            (0..words.len().saturating_sub(1)).any(|i| {
                words[i] == "hushrun"
                    && words[i + 1] == "run"
                    && words[i + 2..]
                        .iter()
                        .take_while(|word| **word != "--")
                        .any(|word| *word == check)
            })
        })
    };
    steps
        .as_array()?
        .iter()
        .find(|step| step["run"].as_str().is_some_and(runs))?["id"]
        .as_str()
        .map(str::to_string)
}

/// An `id:` for the step that runs `check`: its name, when GitHub takes it as one.
fn id_for(check: &str) -> String {
    let starts = check
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest = check
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if starts && rest {
        check.to_string()
    } else {
        "hushrun".to_string()
    }
}

/// What keeps the reports of GitLab's `job`, a job of the plan, as init leaves the file.
fn gitlab_reader(look: &Look, written: &BTreeMap<&str, &str>, job: &Value) -> Option<String> {
    let path = job["file"].as_str()?;
    let text = match written.get(path) {
        Some(text) => text.as_bytes().to_vec(),
        None => std::fs::read(look.root.join(path)).ok()?,
    };
    let file = variants::parse(path, &text)?;
    readers::gitlab_reads(&file[job["job"].as_str()?])
}

/// A command init leaves, as the plan lists it.
fn left(line: Option<usize>, check: &str, command: &str, why: &str) -> Value {
    json!({
        "line": line.map(|line| line + 1),
        "dir": null,
        "check": check,
        "status": "left",
        "before": command,
        "after": null,
        "why": why,
        "ci": null,
        "ciWhy": null,
        "ciText": null,
    })
}

/// Adds `ci`, `ciWhy` and `ciText` to `entry`.
fn extend(entry: &mut Value, (ci, why, text): (Value, Value, Value)) {
    entry["ci"] = ci;
    entry["ciWhy"] = why;
    entry["ciText"] = text;
}

/// A run that can't tell which code it is runs, saying so in words of its own, not after
/// `running: `.
fn cant_tell() -> (Value, Value, Value) {
    (json!("runs"), json!("cant-tell"), Value::Null)
}

/// \[ADR 0051\] Why a wrapped script turbo runs runs every time, when turbo.json needs a change by
/// hand.
const TURBO_KEEPS: &str = "turbo runs it without hushrun's variables, which turbo.json doesn't pass on, so it runs every time until turbo.json is changed as the plan says";

/// The workflow CI names in a job, `GITHUB_WORKFLOW_REF`, which the run reads to find its job.
enum Started {
    /// Its own file: a push, a pull request, a schedule or a person starts it.
    Itself,
    /// A reusable workflow's: the top-level workflow of this repository that calls it, the first
    /// when several do.
    By(String),
    /// A reusable workflow's whose caller init can't read: another repository's, or one that
    /// calls it by its repository's name rather than `./`.
    Elsewhere,
}

/// A job as CI starts it.
struct InJob<'a> {
    file: &'a Workflow,
    job: &'a Job,
    started: &'a Started,
    repository: &'a str,
    read: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    /// \[ADR 0047\] The checks hushrun.yaml lets CI skip when a later step reads their reports.
    reports_skip: &'a BTreeSet<String>,
    /// \[ADR 0047\] What the job's own `HUSHRUN_REPORTS_READ` says, on GitLab.
    reports_read: Option<bool>,
}

impl InJob<'_> {
    /// What `hushrun run` decides for `check` here with a pass of this job on record: `skips`, or
    /// `runs` with the wall and what the run says (C-14). `script` is the package.json script that
    /// starts it, which npm names in `npm_lifecycle_event`; `inside` says a `hushrun run` starts
    /// that script, and tells it so (`HUSHRUN_INSIDE_RUN`). `command` is what the run starts, whose
    /// kind says whether reports a later step reads concern it, \[ADR 0050\] and `dir` where, which
    /// a task runner's target needs to be read.
    fn decides(
        &self,
        check: &str,
        named: bool,
        script: Option<&str>,
        inside: bool,
        command: &[&str],
        dir: Option<&str>,
    ) -> (Value, Value, Value) {
        let started = match self.started {
            Started::Itself => &self.file.path,
            Started::By(caller) => caller,
            // The run names the caller in its words, which init can't know.
            Started::Elsewhere => return (json!("runs"), json!("reusable"), Value::Null),
        };
        let workflow_ref = format!("{}/{started}@refs/heads/main", self.repository);
        let mut pairs: Vec<(&str, &str)> = match self.file.host {
            Host::GitHub => vec![
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REPOSITORY", self.repository),
                ("GITHUB_WORKFLOW_REF", &workflow_ref),
                ("GITHUB_JOB", &self.job.id),
            ],
            // GitLab tells a job's instances apart in its variables, which init reads as its matrix.
            Host::GitLab => {
                let mut pairs = vec![("GITLAB_CI", "true"), ("CI_JOB_NAME", self.job.id.as_str())];
                if self.job.matrix {
                    pairs.push(("CI_NODE_TOTAL", "2"));
                }
                pairs
            }
        };
        if let Some(script) = script {
            pairs.push(("npm_lifecycle_event", script));
        }
        if inside {
            pairs.push((variants::INSIDE_A_RUN, "1"));
        }
        let identity = self.identity();
        pairs.extend(identity.iter().map(|(name, value)| (*name, value.as_str())));
        let env = Env::from_pairs(pairs);
        let ci = variants::Ci::read(&env, self.read);
        // \[ADR 0047\] With a pass that counts here, a later step that reads its reports still
        // runs it (C-26).
        // \[ADR 0048\] What the pass wrote can narrow that in CI; init can't know it, so it says
        // what a pass that wrote what they read gets.
        let no = ci.variants(&env, check, named).no_skip().or_else(|| {
            ci.reports(
                self.read,
                checks::kind_in(command, dir, self.read),
                self.reports_skip.contains(check),
                self.reports_read,
            )
            .map(|read| read.no_skip())
        });
        match no {
            None => (json!("skips"), Value::Null, Value::Null),
            Some(no) => (json!("runs"), json!(no.wall.word()), json!(no.why)),
        }
    }
}

impl InJob<'_> {
    /// \[ADR 0049\] What the job's own env, as init leaves it, tells the run of which variant or
    /// call it is. GitHub fills in each expression, whose values init can't know: any does, since
    /// the run checks the file says the expression.
    fn identity(&self) -> Vec<(&'static str, String)> {
        if self.file.host != Host::GitHub {
            return Vec::new();
        }
        let Some(file) =
            (self.read)(&self.file.path).and_then(|bytes| variants::parse(&self.file.path, &bytes))
        else {
            return Vec::new();
        };
        let env = &file["jobs"][self.job.id.as_str()]["env"];
        let mut pairs = Vec::new();
        for name in [variants::VARIANT, variants::INPUTS] {
            if env.get(name).is_some() {
                pairs.push((name, "{}".to_string()));
            }
        }
        if let Some(value) = env.get(variants::WORKFLOW) {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            pairs.push((variants::WORKFLOW, value));
        }
        pairs
    }
}

/// `owner/name` for `GITHUB_REPOSITORY`, from the repository's id; any name does when it has none,
/// since the rule only matches it against the workflow's.
fn github_repository(repository: Option<&str>) -> String {
    repository.and_then(|id| id.split_once('/')).map_or_else(
        || "this/repository".to_string(),
        |(_, path)| path.to_string(),
    )
}

/// A run already in CI's files: the line its source starts on, where it runs when that can be
/// told, why it can't be in this repository's checkout, and the run, or why its key can't be told
/// from the file.
struct Already {
    line: usize,
    dir: Option<String>,
    outside: Option<String>,
    run: Result<scripts::Run, &'static str>,
}

/// The `hushrun run`s CI's files already hold in `steps`, a job's, or \[ADR 0050\] an action's it
/// uses.
fn already<'a>(steps: impl IntoIterator<Item = &'a Step>) -> Vec<Already> {
    let mut found = Vec::new();
    for step in steps {
        // A shell reads the step whole, so a `cd` on one line holds on the next; the lines say
        // where each run starts, as long as they find as many.
        let runs = scripts::line_runs(&step.commands);
        let lines: Vec<usize> = step
            .sources
            .iter()
            .flat_map(|source| {
                std::iter::repeat_n(source.line, scripts::line_runs(&source.shell()).len())
            })
            .collect();
        let count = runs.len();
        for (k, run) in runs.into_iter().enumerate() {
            let dir = match (&step.dir, &run.moved) {
                (Ok(dir), Some(moved)) => Some(projects::joined(dir, moved)),
                _ => None,
            };
            found.push(Already {
                line: if lines.len() == count {
                    lines[k]
                } else {
                    step.line
                },
                dir,
                outside: step.outside_the_checkout().map(str::to_string),
                run: run.run,
            });
        }
    }
    found
}

/// The scripts a `hushrun run` of `steps`, a job's and its actions', starts, which each run
/// inside it.
fn started_inside_a_run<'a>(steps: impl Iterator<Item = &'a Step>) -> BTreeSet<String> {
    steps
        .flat_map(|step| scripts::line_runs(&step.commands))
        .filter_map(|run| run.run.ok())
        .flat_map(|run| scripts::scripts_run(&run.command.join(" ")))
        .collect()
}

/// What the repository's workflows say of calls: the jobs that call each file with
/// `uses: ./<file>`, the files `workflow_call` starts, and those something else starts too.
struct Calls {
    callers: BTreeMap<String, Vec<(String, String)>>,
    called: BTreeSet<String>,
    otherwise: BTreeSet<String>,
}

impl Calls {
    fn of(files: &[Workflow]) -> Calls {
        let mut calls = Calls {
            callers: BTreeMap::new(),
            called: BTreeSet::new(),
            otherwise: BTreeSet::new(),
        };
        for file in files.iter().filter(|file| file.host == Host::GitHub) {
            let Some(yaml) = variants::parse(&file.path, file.source().as_bytes()) else {
                continue;
            };
            for trigger in triggers(&yaml) {
                match trigger.as_str() {
                    "workflow_call" => calls.called.insert(file.path.clone()),
                    _ => calls.otherwise.insert(file.path.clone()),
                };
            }
            let Some(jobs) = yaml.get("jobs").and_then(Value::as_object) else {
                continue;
            };
            for (id, job) in jobs {
                let Some(called) = job
                    .get("uses")
                    .and_then(Value::as_str)
                    .and_then(|uses| uses.strip_prefix("./"))
                else {
                    continue;
                };
                let called = called.split('@').next().unwrap_or(called);
                calls
                    .callers
                    .entry(called.to_string())
                    .or_default()
                    .push((file.path.clone(), id.clone()));
            }
        }
        calls
    }

    /// The jobs that call `path`, as the plan lists them.
    fn callers_of(&self, path: &str) -> Vec<Value> {
        self.callers
            .get(path)
            .into_iter()
            .flatten()
            .map(|(file, job)| json!({"file": file, "job": job}))
            .collect()
    }

    /// How CI names `path`'s jobs. GitHub names the top-level workflow in a reusable one's, so a
    /// caller that is itself called names its caller in turn.
    fn started(&self, path: &str) -> Started {
        if !self.called.contains(path) {
            return Started::Itself;
        }
        let mut seen = BTreeSet::from([path.to_string()]);
        let mut current = path.to_string();
        loop {
            let unseen = |file: &str, seen: &BTreeSet<String>| {
                self.callers
                    .get(file)
                    .into_iter()
                    .flatten()
                    .map(|(caller, _)| caller.clone())
                    .find(|caller| !seen.contains(caller))
            };
            let Some(caller) = unseen(&current, &seen) else {
                return Started::Elsewhere;
            };
            seen.insert(caller.clone());
            if self.called.contains(&caller) && unseen(&caller, &seen).is_some() {
                current = caller;
                continue;
            }
            return if self.otherwise.contains(&caller) || !self.called.contains(&caller) {
                Started::By(caller)
            } else {
                Started::Elsewhere
            };
        }
    }
}

/// What starts a workflow: the keys of its `on:`, whatever form it takes.
fn triggers(yaml: &Value) -> Vec<String> {
    // YAML 1.1 reads a bare `on` as true; GitHub reads it as the key.
    match yaml.get("on").or_else(|| yaml.get("true")) {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// A unified diff for each file init writes, in the order it writes them.
fn edits(root: &Path, files: &[(String, String)]) -> Vec<Value> {
    files
        .iter()
        .filter_map(|(path, text)| {
            let before = std::fs::read_to_string(root.join(path)).ok();
            let diff = super::diff::unified(path, before.as_deref(), text);
            (!diff.is_empty()).then(|| json!({"path": path, "diff": diff}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-26
    #[test]
    fn a_gitlab_job_runs_with_its_own_variable_else_the_files() {
        let file = |text: &str| -> Value {
            hushrun_core::yaml::parse(".gitlab-ci.yml", text.as_bytes()).unwrap()
        };
        let says = |text: &str| says_reports_read(&file(text), "test");
        assert_eq!(
            says("test:\n  variables:\n    HUSHRUN_REPORTS_READ: \"true\"\n"),
            Some(true)
        );
        assert_eq!(
            says("test:\n  variables:\n    HUSHRUN_REPORTS_READ: 0\n"),
            Some(false)
        );
        assert_eq!(
            says(
                "test:\n  variables:\n    HUSHRUN_REPORTS_READ:\n      value: \"yes\"\n      description: CI runs its checks\n"
            ),
            Some(true)
        );
        assert_eq!(
            says("test:\n  variables:\n    HUSHRUN_REPORTS_READ: maybe\n"),
            None
        );
        // The job's own wins over the file's, as GitLab sets them.
        let both = "variables:\n  HUSHRUN_REPORTS_READ: \"true\"\ntest:\n  variables:\n    HUSHRUN_REPORTS_READ: \"false\"\n";
        assert_eq!(says(both), Some(false));
        let global = "variables:\n  HUSHRUN_REPORTS_READ: \"true\"\ntest:\n  script: [pytest]\n";
        assert_eq!(says(global), Some(true));
        // Unless the job doesn't inherit it.
        for inherit in ["false", "[CI_DEBUG]"] {
            let alone = format!("{global}  inherit:\n    variables: {inherit}\n");
            assert_eq!(says(&alone), None, "{alone}");
        }
        assert_eq!(
            says(&format!(
                "{global}  inherit:\n    variables: [HUSHRUN_REPORTS_READ]\n"
            )),
            Some(true)
        );
        assert_eq!(says("test:\n  script: [pytest]\n"), None);
    }
}

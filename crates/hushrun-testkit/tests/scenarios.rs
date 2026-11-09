//! Every planner scenario in `testdata/scenarios/` (§8, VALIDATION.md §5).

use hushrun_core::names::CheckName;
use hushrun_core::plan::{check_status, plan};
use hushrun_testkit::scenario;

#[test]
fn every_scenario_decides_as_expected() {
    let scenarios = scenario::load_all();
    assert!(!scenarios.is_empty(), "no scenarios found");
    let mut failures = Vec::new();
    for (path, scenario) in &scenarios {
        let decision = scenario.decide();
        if std::env::var_os("HUSHRUN_SCENARIOS_VERBOSE").is_some() {
            eprintln!(
                "{:<70} {:<8} {}",
                scenario.name,
                decision.mode.as_str(),
                decision.reason
            );
        }
        let problems = scenario.mismatches(&decision);
        if !problems.is_empty() {
            failures.push(format!(
                "{} ({}):\n  {}\n  decision: {} — {}",
                scenario.name,
                path.strip_prefix(hushrun_testkit::testdata("scenarios"))
                    .unwrap_or(path)
                    .display(),
                problems.join("\n  "),
                decision.mode,
                decision.reason
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} scenarios failed:\n{}",
        failures.len(),
        scenarios.len(),
        failures.join("\n")
    );
    eprintln!("{} scenarios passed", scenarios.len());
}

/// \[ADR 0062\] `hushrun_core::plan::check_status`, which `hushrun run` skips by (§10.4) and
/// hushrun's server shows a check with, says what the planner says of every required check in
/// every scenario, with the same deciding event: they are one implementation.
// covers: P-1, P-4
#[test]
fn the_check_status_readers_see_is_the_planners() {
    let mut compared = 0;
    for (path, scenario) in &scenario::load_all() {
        let policy = scenario.policy();
        let input = scenario.input(&policy);
        let decision = plan(&input, &policy);
        // What a reader passes: this digest's, scope's and repository's events.
        let events: Vec<_> = input
            .events
            .iter()
            .filter(|e| {
                e.digest == input.digest
                    && e.event.scope == input.scope
                    && e.event.repository == policy.repository
            })
            .cloned()
            .collect();
        for state in &decision.requirements {
            let check = CheckName::new(&state.requirement).expect("a check name");
            let read = check_status(&check, &events, &policy, &input.ci_only, input.context.now);
            assert_eq!(
                read,
                (state.status, state.event.clone()),
                "{}: {check}",
                path.display()
            );
            compared += 1;
        }
    }
    assert!(compared > 100, "only {compared} requirements compared");
}

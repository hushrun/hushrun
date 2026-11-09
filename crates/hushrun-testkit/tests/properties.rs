//! Property tests of the planner's invariants I-1 … I-9 (§8.6), and of how it reads hushrun 0.8's
//! history (ADR 0062).
//!
//! Inputs are random histories on the fixture policies: lifecycle, check, artifact and approval
//! events from any signer (the repository's CI, members' machines, agents), for this digest or
//! others, some recorded at a stage as hushrun 0.8 did, plus deployed index entries, liveness
//! answers and run contexts, with the repository CI-only or not. Set `PROPTEST_CASES` to run more
//! cases (the acceptance criterion is 10,000 per property).

use std::collections::BTreeMap;

use proptest::prelude::*;

use hushrun_core::event::{
    EventKind, SupersededReason, VerifiedEvent, pinned_digest, reference_repository,
};
use hushrun_core::names::StageName;
use hushrun_core::plan::{
    Decision, Liveness, Mode, PlanInput, RunReason, Status, counts_for, plan,
};
use hushrun_core::policy::Policy;
use hushrun_core::run::{Custody, Trigger};
use hushrun_core::time::Dur;
use hushrun_testkit::scenario::{
    ContextSpec, DeployedSpec, EventSpec, Expect, LivenessSpec, Scenario,
};

/// A fixture and the scope to plan.
#[derive(Debug, Clone)]
struct World {
    policy_file: &'static str,
    /// Whether the signers answer says the repository is CI-only (T-16).
    ci_only: bool,
    scope: &'static str,
    principals: Vec<&'static str>,
    checks: Vec<&'static str>,
    refs: Vec<(&'static str, &'static str)>,
    /// The stages hushrun 0.8 recorded events at, in this repository's history.
    old_stages: Vec<&'static str>,
}

fn worlds() -> Vec<World> {
    let api_world = |ci_only| World {
        policy_file: "acme-api.yaml",
        ci_only,
        scope: "default",
        principals: vec!["ci", "ci", "ana", "agent-fleet"],
        checks: vec!["toolchain", "unit"],
        refs: vec![
            ("image", "us-central1-docker.pkg.dev/acme-prod/api/go@a"),
            ("image", "us-central1-docker.pkg.dev/acme-prod/api/go@b"),
            // hushrun 0.8's staging location, and somewhere else.
            ("image", "us-central1-docker.pkg.dev/acme-test/api/go@c"),
            ("image", "evil.example.com/back/go@d"),
            (
                "image",
                "us-central1-docker.pkg.dev/acme-prod/api/go:latest",
            ),
        ],
        old_stages: vec!["staging", "production"],
    };
    let deploy = |ci_only| World {
        policy_file: "deploy.yaml",
        ci_only,
        scope: "api",
        principals: vec!["ci", "ci", "ana", "agent-fleet"],
        checks: vec!["unit", "lint", "e2e", "sast"],
        refs: vec![
            ("api-image", "ghcr.io/acme/api@a"),
            ("api-image", "ghcr.io/acme/api@b"),
            ("api-image", "ghcr.io/acme/elsewhere/api@c"),
            // Another scope's artifact.
            ("web-image", "ghcr.io/acme/web@d"),
        ],
        old_stages: vec!["staging", "canary", "production"],
    };
    vec![
        api_world(false),
        api_world(true),
        deploy(false),
        deploy(true),
    ]
}

fn hhmm(minutes: u32) -> String {
    format!("{:02}:{:02}", 8 + minutes / 60, minutes % 60)
}

fn arb_event(world: World) -> impl Strategy<Value = EventSpec> {
    let kinds = prop_oneof![
        3 => Just(EventKind::Started),
        4 => Just(EventKind::Check),
        2 => Just(EventKind::Artifact),
        1 => Just(EventKind::Deployed),
        2 => Just(EventKind::Passed),
        2 => Just(EventKind::Failed),
        2 => Just(EventKind::Superseded),
        1 => Just(EventKind::Approval),
    ];
    (
        kinds,
        (
            prop::bool::weighted(0.2),
            prop::sample::select(world.old_stages.clone()),
            prop::bool::weighted(0.3),
        ),
        0u32..5,
        1u32..3,
        0u32..600,
        prop::sample::select(world.principals.clone()),
        prop::sample::select(vec![
            Custody::Ci,
            Custody::Ci,
            Custody::Isolated,
            Custody::Local,
        ]),
        prop::bool::weighted(0.85),
        prop::sample::select(world.checks.clone()),
        (
            prop::bool::weighted(0.8),
            prop::bool::weighted(0.85),
            1u32..3,
        ),
        prop::sample::select(world.refs.clone()),
        (any::<bool>(), 0u32..120),
    )
        .prop_map(
            move |(
                kind,
                (old, stage, deployment_check),
                run,
                attempt,
                minute,
                by,
                custody,
                this,
                check,
                (pass, current_def, attempts),
                (artifact, reference),
                (stale, lease),
            )| {
                // hushrun 0.8 recorded every approval at a stage, and some of everything else.
                let stage = (old || kind == EventKind::Approval).then(|| stage.to_string());
                let mut spec = EventSpec {
                    kind,
                    stage,
                    run: format!("r{run}"),
                    attempt,
                    at: hhmm(minute),
                    by: by.to_string(),
                    custody: Some(custody),
                    digest: Some(if this { "this" } else { "other" }.into()),
                    scope: None,
                    url: None,
                    trigger: None,
                    lease: Some(Dur::from_secs(u64::from(30 + lease) * 60)),
                    check: None,
                    result: None,
                    finished: None,
                    attempts: None,
                    definition: None,
                    image: None,
                    platform: None,
                    check_kind: None,
                    artifact: None,
                    reference: None,
                    artifacts: BTreeMap::new(),
                    reason: None,
                };
                match kind {
                    EventKind::Check => {
                        spec.check = Some(check.to_string());
                        spec.result = Some(if pass { "pass" } else { "fail" }.into());
                        spec.definition = (!current_def).then(|| "stale".into());
                        spec.attempts = Some(attempts);
                        if check == "lint" && current_def {
                            spec.image = Some("golangci/golangci-lint@1".into());
                            spec.platform = Some("linux/amd64".into());
                        }
                        // A deployment check of hushrun 0.8.
                        if spec.stage.is_some() && deployment_check {
                            spec.check_kind = Some("deployment".into());
                        }
                    }
                    EventKind::Approval => spec.check = Some(check.to_string()),
                    EventKind::Artifact => {
                        spec.artifact = Some(artifact.to_string());
                        spec.reference = Some(reference.to_string());
                    }
                    EventKind::Deployed | EventKind::Passed if pass => {
                        spec.artifacts =
                            BTreeMap::from([(artifact.to_string(), reference.to_string())]);
                    }
                    EventKind::Superseded => {
                        spec.reason = Some(if stale { "stale" } else { "cancelled" }.into());
                    }
                    _ => {}
                }
                spec
            },
        )
}

fn arb_scenario() -> impl Strategy<Value = Scenario> {
    prop::sample::select(worlds())
        .prop_flat_map(|world| {
            let events = prop::collection::vec(arb_event(world.clone()), 0..28);
            let deployed = prop::collection::vec(
                (
                    any::<bool>(),
                    0u32..600,
                    prop::sample::select(world.principals.clone()),
                    prop::option::weighted(0.2, prop::sample::select(world.old_stages.clone())),
                ),
                0..3,
            );
            let liveness = prop::collection::vec(
                (
                    0u32..5,
                    1u32..3,
                    prop::sample::select(vec![
                        Liveness::InFlight,
                        Liveness::NoVerdict,
                        Liveness::Superseded,
                        Liveness::Failed,
                    ]),
                ),
                0..3,
            );
            let context = (
                prop::sample::select(vec![
                    Some("main"),
                    Some("main"),
                    Some("main"),
                    Some("develop"),
                    None,
                ]),
                prop::sample::select(vec![
                    Trigger::Push,
                    Trigger::Upstream,
                    Trigger::Manual,
                    Trigger::Schedule,
                ]),
                1u32..4,
                prop::bool::weighted(0.2),
                600u32..900,
                0u32..8,
            );
            (Just(world), events, deployed, liveness, context)
        })
        .prop_map(
            |(world, events, deployed, liveness, (branch, trigger, attempt, force, now, run))| {
                Scenario {
                    name: "property".into(),
                    covers: Vec::new(),
                    description: None,
                    policy: Some(world.policy_file.into()),
                    policy_yaml: None,
                    scope: Some(world.scope.into()),
                    now: hhmm(now),
                    context: ContextSpec {
                        branch: branch.map(str::to_string),
                        trigger,
                        attempt,
                        force,
                        run: format!("this-run-{run}"),
                    },
                    events,
                    deployed: deployed
                        .into_iter()
                        .map(|(this, minute, by, stage)| DeployedSpec {
                            digest: Some(if this { "this" } else { "other" }.into()),
                            at: hhmm(minute),
                            by: by.into(),
                            run: None,
                            stage: stage.map(str::to_string),
                        })
                        .collect(),
                    liveness: liveness
                        .into_iter()
                        .map(|(run, attempt, state)| LivenessSpec {
                            run: format!("r{run}"),
                            attempt,
                            state,
                        })
                        .collect(),
                    ci_only: world.ci_only,
                    expect: Expect {
                        mode: String::new(),
                        reason: None,
                        build: None,
                        artifacts: None,
                        checks: None,
                        requirements: None,
                    },
                }
            },
        )
}

fn decide(scenario: &Scenario) -> (Policy, PlanInput, Decision) {
    let policy = scenario.policy();
    let input = scenario.input(&policy);
    let decision = plan(&input, &policy);
    (policy, input, decision)
}

/// Whether the attempt that recorded `event` has a `passed`, or a `superseded` with reason
/// `stale`: a lifecycle event of the pipeline (no stage) from a signer that counts (T-16).
fn its_attempt_passed_or_went_stale(input: &PlanInput, event: &VerifiedEvent) -> bool {
    input.events.iter().any(|e| {
        e.digest == input.digest
            && e.event.scope == input.scope
            && e.event.stage.is_none()
            && e.event.run.run_ref() == event.event.run.run_ref()
            && counts_for(&input.ci_only, e)
            && match e.event.kind {
                EventKind::Passed => true,
                EventKind::Superseded => e
                    .event
                    .superseded
                    .as_ref()
                    .is_some_and(|s| s.reason == SupersededReason::Stale),
                _ => false,
            }
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(512),
        ..ProptestConfig::default()
    })]

    // covers: I-1
    #[test]
    fn i1_promote_and_refresh_reuse_only_what_a_passed_attempt_built(scenario in arb_scenario()) {
        let (policy, input, decision) = decide(&scenario);
        if decision.mode == Mode::Promote {
            prop_assert!(decision.checks_to_run.is_empty(), "{:?}", decision);
            for state in &decision.requirements {
                prop_assert_eq!(state.status, Status::Satisfied, "{} in {:?}", state.requirement, decision);
            }
        }
        if decision.mode == Mode::Refresh {
            prop_assert!(!decision.checks_to_run.is_empty(), "{:?}", decision);
        }
        if matches!(decision.mode, Mode::Promote | Mode::Refresh) {
            prop_assert!(!decision.build);
            let declared: Vec<_> = policy.artifacts_of(&input.scope).map(|(name, _)| name.clone()).collect();
            let chosen: Vec<_> = decision.artifacts.keys().cloned().collect();
            prop_assert_eq!(chosen, declared, "every artifact of the scope, and only those");
            for (name, chosen) in &decision.artifacts {
                let event = input.events.iter().find(|e| e.key == chosen.event).expect("the justifying event is an input event");
                let artifact = event.event.artifact.as_ref().expect("an artifact event");
                prop_assert_eq!(&artifact.name, name);
                prop_assert_eq!(event.digest, input.digest);
                prop_assert_eq!(&event.event.scope, &input.scope);
                prop_assert!(counts_for(&input.ci_only, event));
                prop_assert_eq!(pinned_digest(&artifact.reference), Some(chosen.digest));
                prop_assert_eq!(reference_repository(&artifact.reference), policy.artifacts[name].location.as_str());
                prop_assert!(its_attempt_passed_or_went_stale(&input, event), "{} from {}", name, chosen.event);
            }
        }
    }

    // covers: I-3
    #[test]
    fn i3_event_order_never_matters(scenario in arb_scenario(), seed in any::<u64>()) {
        let (policy, mut input, decision) = decide(&scenario);
        // A deterministic shuffle.
        let mut state = seed | 1;
        for i in (1..input.events.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            input.events.swap(i, (state % (i as u64 + 1)) as usize);
        }
        input.deployed.reverse();
        input.liveness.reverse();
        prop_assert_eq!(plan(&input, &policy), decision);
    }

    // covers: I-4
    #[test]
    fn i4_foreign_events_never_matter(scenario in arb_scenario(), foreign in prop::collection::vec(arb_event(worlds()[0].clone()), 1..6)) {
        let (policy, input, decision) = decide(&scenario);
        let mut with_foreign = scenario.clone();
        // Events for another digest and for another scope.
        for (i, mut e) in foreign.into_iter().enumerate() {
            if i % 2 == 0 {
                e.digest = Some("other".into());
            } else {
                e.scope = Some(if scenario.scope.as_deref() == Some("api") { "web".into() } else { "nope".into() });
            }
            with_foreign.events.push(e);
        }
        let mut input2 = with_foreign.input(&policy);
        // Foreign events for another repository, too.
        let mut moved = input.events.clone();
        for e in &mut moved {
            e.event.repository = "github.com/acme/elsewhere".into();
            e.key = format!("{}-moved", e.key);
        }
        input2.events.extend(moved);
        prop_assert_eq!(plan(&input2, &policy), decision);
    }

    // covers: I-5
    #[test]
    fn i5_current_only_on_automatic_first_attempts(scenario in arb_scenario()) {
        let (policy, input, decision) = decide(&scenario);
        if decision.mode == Mode::Current {
            prop_assert_eq!(input.context.attempt, 1);
            prop_assert!(policy.deploy_or_default().dedupe.contains(&input.context.trigger));
            prop_assert!(matches!(input.context.trigger, Trigger::Push | Trigger::Upstream));
            prop_assert!(!input.context.force_full);
            prop_assert_eq!(decision.deployed.as_ref().map(|d| d.digest), Some(input.digest));
        }
    }

    // covers: I-6
    #[test]
    fn i6_forced_runs_never_promote_refresh_or_current(scenario in arb_scenario()) {
        let (_, input, decision) = decide(&scenario);
        if input.context.force_full {
            prop_assert!(matches!(decision.mode, Mode::Full | Mode::Blocked), "{:?}", decision.mode);
        }
    }

    // covers: I-7
    #[test]
    fn i7_time_never_promotes(scenario in arb_scenario(), minutes in 0u64..20_000) {
        let (policy, mut input, decision) = decide(&scenario);
        input.context.now = input.context.now.saturating_add(Dur::from_secs(minutes * 60));
        let later = plan(&input, &policy);
        if decision.mode != Mode::Promote {
            prop_assert_ne!(later.mode, Mode::Promote, "{:?} became promote", decision.mode);
        }
    }

    // covers: I-9
    #[test]
    fn i9_checks_run_only_when_required(scenario in arb_scenario()) {
        let (policy, input, decision) = decide(&scenario);
        let required = policy.required();
        let to_run: Vec<_> = decision.checks_to_run.iter().map(|c| c.check.clone()).collect();
        for check in &to_run {
            prop_assert!(required.contains(check), "{} in checks_to_run: {:?}", check, decision);
        }
        if decision.mode == Mode::Full && input.context.force_full {
            // P-9: every required check, in order.
            prop_assert_eq!(&to_run, &required);
            prop_assert!(decision.checks_to_run.iter().all(|c| c.reason == RunReason::Forced));
        } else if matches!(decision.mode, Mode::Refresh | Mode::Full) {
            // P-14, P-15: the required checks without valid evidence, in order.
            let lacking: Vec<_> = decision
                .requirements
                .iter()
                .filter(|r| r.status != Status::Satisfied)
                .map(|r| r.requirement.clone())
                .collect();
            let to_run: Vec<String> = to_run.iter().map(ToString::to_string).collect();
            prop_assert_eq!(to_run, lacking);
        } else {
            prop_assert!(to_run.is_empty(), "{:?}", decision);
        }
    }

    // covers: T-16
    #[test]
    fn t16_a_ci_only_repository_decides_on_cis_events(scenario in arb_scenario()) {
        let (_, input, decision) = decide(&scenario);
        if input.ci_only.current().next().is_some() {
            for state in &decision.requirements {
                if let Some(key) = &state.event {
                    let event = input.events.iter().find(|e| &e.key == key).expect("an input event");
                    prop_assert_eq!(event.signer.as_str(), "ci", "{} decided by {}", state.requirement, key);
                }
            }
            for chosen in decision.artifacts.values() {
                let event = input.events.iter().find(|e| e.key == chosen.event).expect("an input event");
                prop_assert_eq!(event.signer.as_str(), "ci");
            }
            if let Some(deployed) = &decision.deployed {
                let event = input.deployed.iter().find(|e| e.key == deployed.event).expect("a deployed entry");
                prop_assert_eq!(event.signer.as_str(), "ci");
            }
        }
    }

    /// \[ADR 0062\] What hushrun 0.8 recorded at a stage belongs to that stage: its lifecycle
    /// events form no attempt of the pipeline, its deployed index entries aren't the pipeline's,
    /// and its approvals are inert. Taking them all away never changes a decision.
    // covers: P-5, P-10
    #[test]
    fn staged_attempts_deployments_and_approvals_never_matter(scenario in arb_scenario()) {
        let (policy, mut input, decision) = decide(&scenario);
        input.events.retain(|e| {
            e.event.kind != EventKind::Approval
                && !(e.event.kind.is_lifecycle() && e.event.stage.is_some())
        });
        input.deployed.retain(|e| e.event.stage.is_none());
        prop_assert_eq!(plan(&input, &policy), decision);
    }

    /// \[ADR 0062\] A check's or an artifact's evidence is about the files: the stage hushrun 0.8
    /// recorded it at, or none, never changes a decision.
    // covers: P-1, P-13
    #[test]
    fn the_stage_of_a_check_or_artifact_never_matters(scenario in arb_scenario(), flip in prop::collection::vec(any::<bool>(), 28)) {
        let (policy, mut input, decision) = decide(&scenario);
        let staging = StageName::new("staging").expect("a stage name");
        for (e, flip) in input.events.iter_mut().zip(flip) {
            if flip && matches!(e.event.kind, EventKind::Check | EventKind::Artifact) {
                e.event.stage = match e.event.stage {
                    Some(_) => None,
                    None => Some(staging.clone()),
                };
            }
        }
        prop_assert_eq!(plan(&input, &policy), decision);
    }
}

#[test]
fn the_generator_reaches_every_mode() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let all = [
        Mode::Promote,
        Mode::Refresh,
        Mode::Full,
        Mode::Current,
        Mode::Blocked,
    ];
    let mut runner = TestRunner::deterministic();
    let mut seen = std::collections::BTreeSet::new();
    // Parsing the fixtures dominates; each world's policy is parsed once.
    let mut parsed: BTreeMap<Option<String>, Policy> = BTreeMap::new();
    for _ in 0..10_000 {
        let scenario = arb_scenario().new_tree(&mut runner).unwrap().current();
        let policy = parsed
            .entry(scenario.policy.clone())
            .or_insert_with(|| scenario.policy());
        seen.insert(plan(&scenario.input(policy), policy).mode);
        if seen.len() == all.len() {
            break;
        }
    }
    for mode in all {
        assert!(
            seen.contains(&mode),
            "the generator never produced {mode}: {seen:?}"
        );
    }
}

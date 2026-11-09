//! Unit tests of the attempt state machine (P-6) and the modes. The scenario catalog in
//! `testdata/scenarios/` covers the whole decision procedure.

use super::attempts::{AttemptState, attempts};
use super::*;
use crate::digest::Digest;
use crate::event::*;
use crate::names::{ArtifactName, CheckName, ScopeName, StageName};
use crate::run::{Custody, Provider, RunRef, Trigger};
use crate::time::Time;

const POLICY: &str = "version: 1\nrepository: github.com/acme/app\ndeploy:\n  lease: 1h\n";

fn at(hm: &str) -> Time {
    Time::parse(&format!("2026-10-13T{hm}:00.000Z")).unwrap()
}

fn event(n: usize, kind: EventKind, run: &str, when: &str) -> VerifiedEvent {
    event_with(n, kind, run, when, None)
}

/// An event whose kind's object is `object`, or a minimal one.
fn event_with(
    n: usize,
    kind: EventKind,
    run: &str,
    when: &str,
    object: Option<serde_json::Value>,
) -> VerifiedEvent {
    let time = at(when);
    let mut value = serde_json::json!({
        "kind": kind, "repository": "github.com/acme/app", "scope": "default",
        "canonicalization": "hushrun-source-v1", "source": {"vcs": "git"},
        "run": {"provider": "generic", "id": run, "attempt": 1, "trigger": "push"},
        "producer": {"tool": "hushrun", "version": "0", "custody": "ci"},
        "time": time, "policyDigest": Digest::of(b"p"),
    });
    let default = match kind {
        EventKind::Started => {
            serde_json::json!({"leaseUntil": time.saturating_add(crate::time::Dur::from_secs(3 * 3600))})
        }
        EventKind::Passed | EventKind::Deployed => serde_json::json!({"artifacts": []}),
        EventKind::Failed => serde_json::json!({"reason": "x"}),
        EventKind::Superseded => serde_json::json!({"reason": "stale"}),
        _ => serde_json::json!({}),
    };
    value[kind.as_str()] = object.unwrap_or(default);
    VerifiedEvent {
        key: format!("v1/events/default/x/{n:026}.{kind}._.json"),
        signer: "ci".into(),
        effective_custody: Custody::Ci,
        digest: Digest::of(b"d"),
        event: serde_json::from_value(value).unwrap(),
        trusted_time: None,
    }
}

fn states(events: &[VerifiedEvent], now: &str, liveness: &[LivenessAnswer]) -> Vec<AttemptState> {
    let policy = Policy::parse("hushrun.yaml", POLICY.as_bytes()).unwrap();
    attempts(events, liveness, &policy, &CiOnly::none(), at(now))
        .into_iter()
        .map(|a| a.state)
        .collect()
}

// covers: P-6
#[test]
fn terminal_events_decide_in_order() {
    use EventKind::*;
    let run = |kinds: &[(EventKind, &str)]| {
        let mut events = vec![event(0, Started, "r", "15:00")];
        for (i, (kind, when)) in kinds.iter().enumerate() {
            events.push(event(i + 1, *kind, "r", when));
        }
        states(&events, "15:30", &[])[0]
    };
    assert_eq!(run(&[(Passed, "15:05")]), AttemptState::Passed);
    assert_eq!(run(&[(Failed, "15:05")]), AttemptState::Failed);
    // failed after passed: a later job broke.
    assert_eq!(
        run(&[(Passed, "15:05"), (Failed, "15:06")]),
        AttemptState::Failed
    );
    // passed well after failed: a retried job succeeded.
    assert_eq!(
        run(&[(Failed, "15:05"), (Passed, "15:07")]),
        AttemptState::Passed
    );
    // passed within 60 s of failed: the failure wins (clock skew).
    let mut events = vec![
        event(0, Started, "r", "15:00"),
        event(1, Failed, "r", "15:05"),
    ];
    let mut close = event(2, Passed, "r", "15:05");
    close.event.time = close
        .event
        .time
        .saturating_add(crate::time::Dur::from_secs(59));
    events.push(close);
    assert_eq!(states(&events, "15:30", &[])[0], AttemptState::Failed);
    // A cancellation never hides a verdict.
    assert_eq!(
        run(&[(Passed, "15:05"), (Superseded, "15:06")]),
        AttemptState::Passed
    );
    assert_eq!(
        run(&[(Superseded, "15:05")]),
        AttemptState::Superseded(Some(SupersededReason::Stale))
    );
    assert_eq!(
        run(&[(Superseded, "15:05"), (Failed, "15:06")]),
        AttemptState::Failed
    );
}

// covers: P-6
#[test]
fn without_terminal_events_liveness_then_the_lease_decide() {
    let started = [event(0, EventKind::Started, "r", "15:00")];
    let answer = |liveness| {
        vec![LivenessAnswer {
            run: RunRef {
                provider: Provider::Generic,
                id: "r".into(),
                attempt: 1,
            },
            liveness,
        }]
    };
    assert_eq!(
        states(&started, "15:30", &answer(Liveness::InFlight))[0],
        AttemptState::InFlight
    );
    assert_eq!(
        states(&started, "15:30", &answer(Liveness::NoVerdict))[0],
        AttemptState::NoVerdict
    );
    assert_eq!(
        states(&started, "15:30", &answer(Liveness::Superseded))[0],
        AttemptState::Superseded(None)
    );
    assert_eq!(
        states(&started, "15:30", &answer(Liveness::Failed))[0],
        AttemptState::Failed
    );
    // No answer: in flight until the lease, capped by `deploy.lease`, 1 h (not the 3 h the
    // event asked for).
    assert_eq!(states(&started, "15:59", &[])[0], AttemptState::InFlight);
    assert_eq!(states(&started, "16:00", &[])[0], AttemptState::Unknown);
    assert_eq!(states(&started, "17:00", &[])[0], AttemptState::Unknown);
}

// covers: P-5 (ADR 0062: lifecycle events hushrun 0.8 recorded for a stage were that stage's
// pipeline's, so they make no attempt of this one)
#[test]
fn attempts_are_ordered_by_started_and_a_stages_events_are_left_out() {
    use EventKind::*;
    // Attempts are ordered by their started events, not by their keys.
    let events = [
        event(9, Started, "z", "14:00"),
        event(10, Failed, "z", "14:10"),
        event(0, Started, "a", "15:00"),
        event(1, Passed, "a", "15:10"),
    ];
    assert_eq!(
        states(&events, "18:00", &[]),
        [AttemptState::Failed, AttemptState::Passed]
    );
    let mut staging = event(2, Passed, "s", "16:00");
    staging.event.stage = Some(StageName::new("staging").unwrap());
    let policy = Policy::parse("hushrun.yaml", POLICY.as_bytes()).unwrap();
    assert!(!records_attempt(&staging, &CiOnly::none()));
    assert!(attempts(&[staging], &[], &policy, &CiOnly::none(), at("18:00")).is_empty());
}

#[test]
fn modes_print_and_say_whether_they_deploy() {
    let all = [
        (Mode::Promote, "promote", true),
        (Mode::Refresh, "refresh", true),
        (Mode::Full, "full", true),
        (Mode::Current, "current", false),
        (Mode::Blocked, "blocked", false),
    ];
    for (mode, text, deploys) in all {
        assert_eq!(mode.to_string(), text);
        assert_eq!(mode.as_str(), text);
        assert_eq!(serde_json::to_value(mode).unwrap(), text);
        assert_eq!(serde_json::from_value::<Mode>(text.into()).unwrap(), mode);
        assert_eq!(mode.deploys(), deploys, "{mode}");
    }
    // A plan file from hushrun 0.6.0 or earlier still reads: it called `current` `skip`.
    assert_eq!(
        serde_json::from_value::<Mode>("skip".into()).unwrap(),
        Mode::Current
    );
    // ADR 0062: `wait` went with upstream stages.
    assert!(serde_json::from_value::<Mode>("wait".into()).is_err());
}

fn input(events: Vec<VerifiedEvent>) -> PlanInput {
    PlanInput {
        scope: ScopeName::default_scope(),
        digest: Digest::of(b"d"),
        context: PlanContext {
            branch: Some("main".into()),
            trigger: Trigger::Push,
            attempt: 1,
            force_full: false,
            run: crate::run::RunRef {
                provider: Provider::Generic,
                id: "p1".into(),
                attempt: 1,
            },
            now: at("12:00"),
        },
        events,
        deployed: Vec::new(),
        liveness: Vec::new(),
        ci_only: CiOnly::none(),
    }
}

fn check_event(n: usize, policy: &Policy, check: &str, result: &str, when: &str) -> VerifiedEvent {
    let def = &policy.checks[&CheckName::new(check).unwrap()];
    let object = serde_json::json!({
        "name": check, "checkKind": "source", "definitionDigest": def.definition_digest,
        "result": result, "attempts": 1, "durationMs": 1, "startedAt": at(when),
        "finishedAt": at(when),
    });
    event_with(n, EventKind::Check, &format!("c{n}"), when, Some(object))
}

// covers: P-8, P-9, P-10, P-14, P-15 (ADR 0062: one pipeline)
#[test]
fn one_pipeline_runs_what_lacks_evidence_and_deploys_what_passed() {
    let policy = Policy::parse(
        "hushrun.yaml",
        b"version: 1\nrepository: github.com/acme/app\nchecks:\n  unit: {run: go test}\n  lint: {run: golangci-lint run}\ndeploy:\n  branch: main\n  require: [unit, lint]\n",
    )
    .unwrap();
    let to_run = |d: &Decision| -> Vec<String> {
        d.checks_to_run
            .iter()
            .map(|c| c.check.to_string())
            .collect()
    };
    // Nothing recorded: nothing to build, both checks to run first.
    let none = plan(&input(Vec::new()), &policy);
    assert_eq!(none.mode, Mode::Refresh, "{}", none.reason);
    assert_eq!(to_run(&none), ["unit", "lint"]);
    // unit passed: only lint runs. Both passed: nothing runs.
    let unit = check_event(1, &policy, "unit", "pass", "11:00");
    let lint = check_event(2, &policy, "lint", "pass", "11:01");
    let one = plan(&input(vec![unit.clone()]), &policy);
    assert_eq!(to_run(&one), ["lint"]);
    let both = plan(&input(vec![unit.clone(), lint.clone()]), &policy);
    assert_eq!(both.mode, Mode::Promote, "{}", both.reason);
    assert_eq!(both.reason, "every required check is met");
    // Its evidence from hushrun 0.8, recorded at a stage, counts the same: it is about the files.
    let mut old = unit.clone();
    old.event.stage = Some(StageName::new("staging").unwrap());
    assert_eq!(
        to_run(&plan(&input(vec![old, lint.clone()]), &policy)),
        Vec::<String>::new()
    );
    // P-8: another branch is blocked, whatever the evidence.
    let mut elsewhere = input(vec![unit.clone(), lint.clone()]);
    elsewhere.context.branch = Some("develop".into());
    let blocked = plan(&elsewhere, &policy);
    assert_eq!(blocked.mode, Mode::Blocked);
    assert!(
        blocked.reason.contains("deploys only from main"),
        "{}",
        blocked.reason
    );
    // P-9: forcing builds and runs both.
    let mut forced = input(vec![unit.clone(), lint.clone()]);
    forced.context.force_full = true;
    let full = plan(&forced, &policy);
    assert_eq!((full.mode, full.build), (Mode::Full, true));
    assert_eq!(to_run(&full), ["unit", "lint"]);
    // P-10: what is already deployed is current.
    let mut deployed = event(3, EventKind::Deployed, "d1", "11:30");
    deployed.key = "v1/deployed/default/_/x.json".into();
    let mut current = input(vec![unit, lint]);
    current.deployed = vec![deployed.clone()];
    assert_eq!(plan(&current, &policy).mode, Mode::Current);
    // A deployment hushrun 0.8 recorded for a stage was that stage's.
    deployed.event.stage = Some(StageName::new("production").unwrap());
    current.deployed = vec![deployed];
    assert_eq!(plan(&current, &policy).mode, Mode::Promote);
}

// covers: P-13, P-15
#[test]
fn an_artifact_is_reused_only_from_its_location_and_a_passed_attempt() {
    let policy = Policy::parse(
        "hushrun.yaml",
        b"version: 1\nrepository: github.com/acme/app\nartifacts:\n  image:\n    type: oci\n    location: ghcr.io/acme/app\n",
    )
    .unwrap();
    let hex = "a".repeat(64);
    let pinned = format!("sha256:{hex}");
    let artifact = |n: usize, run: &str, repository: &str| {
        let object = serde_json::json!({
            "name": "image", "type": "oci", "origin": "built",
            "ref": format!("{repository}@{pinned}"), "digest": {"sha256": hex},
        });
        event_with(n, EventKind::Artifact, run, "10:01", Some(object))
    };
    let attempt = |run: &str, outcome: EventKind| {
        vec![
            event(10, EventKind::Started, run, "10:00"),
            event(11, outcome, run, "10:05"),
        ]
    };
    let decide = |events: Vec<VerifiedEvent>| plan(&input(events), &policy);
    // Built by an attempt that passed, at its location: deployed by digest.
    let mut events = attempt("a", EventKind::Passed);
    events.push(artifact(12, "a", "ghcr.io/acme/app"));
    let reused = decide(events);
    assert_eq!(reused.mode, Mode::Promote, "{}", reused.reason);
    assert!(
        reused.artifacts[&ArtifactName::new("image").unwrap()]
            .reference
            .starts_with("ghcr.io/acme/app@")
    );
    // Elsewhere, or from an attempt that failed: built again.
    let mut elsewhere = attempt("a", EventKind::Passed);
    elsewhere.push(artifact(12, "a", "ghcr.io/acme/other"));
    assert_eq!(decide(elsewhere).mode, Mode::Full);
    let mut failed = attempt("b", EventKind::Failed);
    failed.push(artifact(12, "b", "ghcr.io/acme/app"));
    let full = decide(failed);
    assert_eq!((full.mode, full.build), (Mode::Full, true));
    assert!(full.reason.contains("aren't reusable"), "{}", full.reason);
}

// covers: P-5, P-6
#[test]
fn attempts_without_a_verdict_are_the_ones_to_ask_about() {
    let events = vec![
        event(1, EventKind::Started, "s1", "10:00"),
        event(2, EventKind::Passed, "s1", "10:05"),
        event(3, EventKind::Started, "s2", "11:00"),
        event(4, EventKind::Deployed, "s2", "11:02"),
    ];
    let runs = attempts_without_verdict(&events, &CiOnly::none());
    assert_eq!(
        runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["s2"]
    );
}

// covers: T-16
#[test]
fn a_ci_only_repository_counts_only_cis_events() {
    let policy = Policy::parse("hushrun.yaml", POLICY.as_bytes()).unwrap();
    // The signers answer of a hushrun before 0.9 names production CI-only since 10:30; read as
    // the repository's, it holds for every event.
    let ci_only = CiOnly::none().with(StageName::new("production").unwrap(), at("10:30"));
    let mut laptop = vec![
        event(1, EventKind::Started, "l1", "10:00"),
        event(2, EventKind::Passed, "l1", "10:05"),
    ];
    for e in &mut laptop {
        e.signer = "user:ana@acme.dev".into();
        e.effective_custody = Custody::Local;
    }
    let run = |events: &[VerifiedEvent], ci_only: &CiOnly| {
        attempts(events, &[], &policy, ci_only, at("11:00")).len()
    };
    // The laptop's untimestamped attempt doesn't count; CI's does.
    assert_eq!(run(&laptop, &ci_only), 0);
    assert!(!counts_for(&ci_only, &laptop[0]));
    assert!(counts_for(
        &ci_only,
        &event(3, EventKind::Started, "c", "10:00")
    ));
    // Timestamped before the rule began, the laptop's attempt counts; after, it doesn't.
    for e in &mut laptop {
        e.trusted_time = Some(at("10:06"));
    }
    assert_eq!(run(&laptop, &ci_only), 1);
    for e in &mut laptop {
        e.trusted_time = Some(at("10:31"));
    }
    assert_eq!(run(&laptop, &ci_only), 0);
    // Without the rule, it counts.
    assert!(counts_for(&CiOnly::none(), &laptop[0]));
    assert_eq!(run(&laptop, &CiOnly::none()), 1);
}

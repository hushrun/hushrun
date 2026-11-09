//! Stages that take evidence from the repository's CI only (T-16), as the server's answer names
//! them (L-13). They are set on the server, by the org's admins, for the whole org or one
//! repository, never in hushrun.yaml: a pull request can't drop the rule it is reviewed under.
//!
//! [ADR 0031 amendment 2] The answer keeps each stage's whole history: every period it was
//! CI-only, the org's and the repository's merged, so that lifting the rule and setting it again
//! doesn't make evidence made during an earlier period count.
//!
//! The answer may also list periods without a stage, `{"from", "until"?}`, beside or instead of
//! the per-stage entries: the repository was CI-only then whatever the stage, so they apply to
//! every stage. A question without a stage reads the union of every period, with a stage or
//! without: never looser than any stage was.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::names::StageName;
use crate::signers::CI_SIGNER;
use crate::time::Time;

/// One period a stage was CI-only: from `from`, until `until` (excluded), or still CI-only when
/// `until` is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Period {
    pub from: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Time>,
}

impl Period {
    /// Whether `t` falls in the period.
    pub fn contains(&self, t: Time) -> bool {
        self.from <= t && self.until.is_none_or(|until| t < until)
    }

    /// Whether the stage is still CI-only by this period.
    pub fn is_open(&self) -> bool {
        self.until.is_none()
    }
}

/// One stage of the list: every period it was CI-only, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiOnlyStage {
    pub stage: StageName,
    pub periods: Vec<Period>,
}

/// One entry of the list as written: a stage with its periods, or a period without a stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CiOnlyEntry {
    Stage(CiOnlyStage),
    Period(Period),
}

/// The CI-only stages of one repository, each with the periods it was CI-only (T-16). Written as
/// a list of `{"stage", "periods": [{"from", "until"?}]}`, one per stage, sorted by stage, in the
/// signers document and in a plan's input, followed by the periods without a stage, if any, as
/// `{"from", "until"?}`. Each stage's periods, and the periods without a stage, are sorted, apart
/// (one ends before the next begins) and non-empty, and only the last may be open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<CiOnlyEntry>", into = "Vec<CiOnlyEntry>")]
pub struct CiOnly {
    stages: BTreeMap<StageName, Vec<Period>>,
    /// Periods without a stage, which apply to every stage.
    stageless: Vec<Period>,
}

/// Adds the period from `from` until `until` (still, without one) to `periods`, sorted and apart:
/// overlapping and touching periods become one. An empty period (`until` not after `from`) adds
/// nothing.
fn merge(periods: &mut Vec<Period>, from: Time, until: Option<Time>) {
    if until.is_some_and(|until| until <= from) {
        return;
    }
    periods.push(Period { from, until });
    periods.sort_by_key(|p| p.from);
    let mut merged: Vec<Period> = Vec::with_capacity(periods.len());
    for p in periods.drain(..) {
        match merged.last_mut() {
            // `last` reaches `p`: open, or ending at or after `p` begins.
            Some(last) if last.until.is_none_or(|until| until >= p.from) => {
                last.until = match (last.until, p.until) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            _ => merged.push(p),
        }
    }
    *periods = merged;
}

/// T-16 for one list of sorted, apart periods: whether an event signed by `signer` with the
/// trusted time `trusted_time` counts.
fn counts_in(periods: &[Period], signer: &str, trusted_time: Option<Time>) -> bool {
    if signer == CI_SIGNER {
        return true;
    }
    match trusted_time {
        Some(t) => !periods.iter().any(|p| p.contains(t)),
        None => !periods.last().is_some_and(Period::is_open),
    }
}

/// Since when `periods` (sorted, apart) are CI-only, if they are now.
fn since_in(periods: &[Period]) -> Option<Time> {
    periods.last().filter(|p| p.is_open()).map(|p| p.from)
}

impl CiOnly {
    /// No stage is or was CI-only: every signer that counts (T-15) counts everywhere.
    pub fn none() -> CiOnly {
        CiOnly::default()
    }

    /// `stage` is CI-only from `since` on, still.
    pub fn with(self, stage: StageName, since: Time) -> CiOnly {
        self.with_period(stage, since, None)
    }

    /// `stage` was CI-only from `from` until `until` (still, without one), merged with the
    /// periods it already has: overlapping and touching periods become one. An empty period
    /// (`until` not after `from`) adds nothing.
    pub fn with_period(mut self, stage: StageName, from: Time, until: Option<Time>) -> CiOnly {
        if until.is_some_and(|until| until <= from) {
            return self;
        }
        merge(self.stages.entry(stage).or_default(), from, until);
        self
    }

    /// The repository was CI-only from `from` until `until` (still, without one), whatever the
    /// stage: a period without a stage, merged as [`CiOnly::with_period`] merges.
    pub fn with_stageless_period(mut self, from: Time, until: Option<Time>) -> CiOnly {
        merge(&mut self.stageless, from, until);
        self
    }

    /// The repository was CI-only in each of `periods`, whatever the stage: periods without a
    /// stage, in any order, merged into sorted, apart ones (overlapping and touching periods
    /// become one, an empty one adds nothing, and only the last may be open).
    pub fn from_periods(periods: Vec<Period>) -> CiOnly {
        periods.into_iter().fold(CiOnly::none(), |rule, p| {
            rule.with_stageless_period(p.from, p.until)
        })
    }

    /// Whether no stage is or was CI-only, and no period without a stage was either.
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty() && self.stageless.is_empty()
    }

    /// Since when `stage` has been CI-only, if it is now: by its own periods or by those without
    /// a stage.
    pub fn since(&self, stage: &StageName) -> Option<Time> {
        since_in(&self.periods(stage))
    }

    /// Every period `stage` was CI-only, oldest first: its own and those without a stage, merged.
    pub fn periods(&self, stage: &StageName) -> Vec<Period> {
        let mut periods = self.stageless.clone();
        for p in self.stages.get(stage).into_iter().flatten() {
            merge(&mut periods, p.from, p.until);
        }
        periods
    }

    /// The stages listed as CI-only now or before, by name, each with its own periods as listed
    /// (not those without a stage: [`CiOnly::stageless`]).
    pub fn stages(&self) -> impl Iterator<Item = (&StageName, &[Period])> {
        self.stages.iter().map(|(s, p)| (s, p.as_slice()))
    }

    /// The stages listed that are CI-only now, by name, each with since when.
    pub fn current(&self) -> impl Iterator<Item = (&StageName, Time)> {
        self.stages.keys().filter_map(|s| Some((s, self.since(s)?)))
    }

    /// The periods listed without a stage, oldest first.
    pub fn stageless(&self) -> &[Period] {
        &self.stageless
    }

    /// What a question without a stage reads: every period, of every stage and without one,
    /// merged into sorted, apart periods (overlapping and touching ones become one; only the last
    /// may be open), oldest first.
    pub fn any_periods(&self) -> Vec<Period> {
        let mut periods = self.stageless.clone();
        for p in self.stages.values().flatten() {
            merge(&mut periods, p.from, p.until);
        }
        periods
    }

    /// Since when the repository has been CI-only for a question without a stage, if it is now:
    /// the start of [`CiOnly::any_periods`]' open period.
    pub fn since_anywhere(&self) -> Option<Time> {
        since_in(&self.any_periods())
    }

    /// T-16: whether an event signed by `signer`, with the trusted time `trusted_time` (E-10),
    /// counts for `stage`. The CI always does. Anyone else's counts unless the stage was CI-only
    /// when it was made: with a trusted time, unless that time falls in one of the stage's
    /// periods (before the first, between two, or after the last one ended, it counts); without
    /// one, the event could have been made now, so it counts unless the stage is CI-only now. A
    /// signer's own `time` never decides.
    pub fn counts(&self, stage: &StageName, signer: &str, trusted_time: Option<Time>) -> bool {
        counts_in(&self.periods(stage), signer, trusted_time)
    }

    /// T-16 for an event without a stage: as [`CiOnly::counts`], against every period of every
    /// stage and those without one ([`CiOnly::any_periods`]). The CI always counts; anyone else's
    /// event counts when its trusted time falls outside every period, or, without a trusted time,
    /// when no period is open now.
    pub fn counts_anywhere(&self, signer: &str, trusted_time: Option<Time>) -> bool {
        counts_in(&self.any_periods(), signer, trusted_time)
    }
}

/// Refuses a list of periods that isn't sorted, apart and closed but for the last; `what` names
/// it in the message.
fn check_periods(what: &str, periods: &[Period]) -> Result<(), String> {
    for (i, p) in periods.iter().enumerate() {
        if p.until.is_some_and(|until| until <= p.from) {
            return Err(format!(
                "{what} has a period that ends when or before it begins"
            ));
        }
        if let Some(next) = periods.get(i + 1) {
            match p.until {
                None => {
                    return Err(format!("{what} has an open period before its last one"));
                }
                Some(until) if until >= next.from => {
                    return Err(format!(
                        "{what}'s periods are out of order, overlap or touch"
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

impl TryFrom<Vec<CiOnlyEntry>> for CiOnly {
    type Error = String;

    fn try_from(list: Vec<CiOnlyEntry>) -> Result<CiOnly, String> {
        let mut stages = BTreeMap::new();
        let mut stageless = Vec::new();
        for entry in list {
            let entry = match entry {
                CiOnlyEntry::Period(period) => {
                    stageless.push(period);
                    continue;
                }
                CiOnlyEntry::Stage(entry) => entry,
            };
            let stage = &entry.stage;
            if entry.periods.is_empty() {
                return Err(format!("the stage {stage} lists no period"));
            }
            check_periods(&format!("the stage {stage}"), &entry.periods)?;
            if stages.insert(entry.stage.clone(), entry.periods).is_some() {
                return Err(format!("the stage {} is listed twice", entry.stage));
            }
        }
        check_periods("the list without a stage", &stageless)?;
        Ok(CiOnly { stages, stageless })
    }
}

impl From<CiOnly> for Vec<CiOnlyEntry> {
    fn from(ci_only: CiOnly) -> Vec<CiOnlyEntry> {
        ci_only
            .stages
            .into_iter()
            .map(|(stage, periods)| CiOnlyEntry::Stage(CiOnlyStage { stage, periods }))
            .chain(ci_only.stageless.into_iter().map(CiOnlyEntry::Period))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        Time::parse(s).unwrap()
    }

    fn day(d: u32) -> Time {
        t(&format!("2026-10-{d:02}T00:00:00Z"))
    }

    const ANA: &str = "user:ana@acme.dev";

    // covers: T-16
    #[test]
    fn a_ci_only_stage_counts_the_ci_and_what_others_made_before_it() {
        let production = StageName::new("production").unwrap();
        let staging = StageName::new("staging").unwrap();
        let rule = CiOnly::none().with(production.clone(), t("2026-10-20T00:00:00Z"));
        assert!(rule.counts(&production, CI_SIGNER, None));
        assert!(rule.counts(&staging, ANA, None));
        // A laptop's event timestamped before the change keeps counting; after it, or without a
        // trusted time, it doesn't.
        assert!(rule.counts(&production, ANA, Some(t("2026-10-19T23:59:59Z"))));
        assert!(!rule.counts(&production, ANA, Some(t("2026-10-20T00:00:00Z"))));
        assert!(!rule.counts(&production, "agent:fleet", None));
    }

    // covers: T-16 (ADR 0031 amendment 2: every period counts, not only the current one)
    #[test]
    fn an_event_made_in_an_earlier_period_does_not_count_after_the_rule_is_set_again() {
        let production = StageName::new("production").unwrap();
        // CI-only on the 10th, lifted on the 15th, set again on the 20th.
        let rule = CiOnly::none()
            .with_period(production.clone(), day(10), Some(day(15)))
            .with(production.clone(), day(20));
        assert_eq!(rule.since(&production), Some(day(20)));
        let at = |d: &str| Some(t(d));
        assert!(
            rule.counts(&production, ANA, at("2026-10-09T12:00:00Z")),
            "before"
        );
        assert!(
            !rule.counts(&production, ANA, at("2026-10-10T00:00:00Z")),
            "from"
        );
        assert!(
            !rule.counts(&production, ANA, at("2026-10-12T12:00:00Z")),
            "inside"
        );
        assert!(
            rule.counts(&production, ANA, at("2026-10-15T00:00:00Z")),
            "until"
        );
        assert!(
            rule.counts(&production, ANA, at("2026-10-17T12:00:00Z")),
            "between"
        );
        assert!(
            !rule.counts(&production, ANA, at("2026-10-21T12:00:00Z")),
            "now"
        );
        assert!(!rule.counts(&production, ANA, None), "CI-only now");
        assert!(rule.counts(&production, CI_SIGNER, at("2026-10-12T12:00:00Z")));

        // Lifted again: an untimestamped event counts (it could be made now), one timestamped
        // in either period still doesn't.
        let lifted = CiOnly::none()
            .with_period(production.clone(), day(10), Some(day(15)))
            .with_period(production.clone(), day(20), Some(day(22)));
        assert_eq!(lifted.since(&production), None);
        assert_eq!(lifted.current().count(), 0);
        assert!(lifted.counts(&production, ANA, None));
        assert!(!lifted.counts(&production, ANA, at("2026-10-12T12:00:00Z")));
        assert!(!lifted.counts(&production, ANA, at("2026-10-21T12:00:00Z")));
        assert!(lifted.counts(&production, ANA, at("2026-10-22T00:00:00Z")));
    }

    // covers: T-16 (the org's and the repository's periods are one history)
    #[test]
    fn overlapping_and_touching_periods_merge() {
        let production = StageName::new("production").unwrap();
        let rule = CiOnly::none()
            // The repository's: the 12th to the 18th.
            .with_period(production.clone(), day(12), Some(day(18)))
            // The org's: the 10th to the 14th, then the 18th to the 19th, then the 25th on.
            .with_period(production.clone(), day(10), Some(day(14)))
            .with_period(production.clone(), day(18), Some(day(19)))
            .with(production.clone(), day(25))
            // Inside the open one, and empty: nothing changes.
            .with_period(production.clone(), day(26), Some(day(27)))
            .with_period(production.clone(), day(5), Some(day(5)));
        assert_eq!(
            rule.periods(&production),
            [
                Period {
                    from: day(10),
                    until: Some(day(19))
                },
                Period {
                    from: day(25),
                    until: None
                },
            ]
        );
        // An open period swallows a later one that starts after it.
        let open = CiOnly::none()
            .with(production.clone(), day(10))
            .with_period(production.clone(), day(20), Some(day(21)));
        assert_eq!(open.periods(&production).len(), 1);
        assert_eq!(open.since(&production), Some(day(10)));
    }

    // covers: L-13
    #[test]
    fn is_a_list_of_stages_with_their_periods() {
        let production = StageName::new("production").unwrap();
        let rule = CiOnly::none()
            .with_period(production.clone(), day(10), Some(day(15)))
            .with(production.clone(), day(20));
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{"stage": "production", "periods": [
                {"from": "2026-10-10T00:00:00.000Z", "until": "2026-10-15T00:00:00.000Z"},
                {"from": "2026-10-20T00:00:00.000Z"},
            ]}])
        );
        assert_eq!(serde_json::from_value::<CiOnly>(json).unwrap(), rule);
        // `until: null` reads as open.
        let null = serde_json::json!([{"stage": "production", "periods": [
            {"from": "2026-10-20T00:00:00Z", "until": null}
        ]}]);
        assert_eq!(
            serde_json::from_value::<CiOnly>(null)
                .unwrap()
                .since(&production),
            Some(day(20))
        );

        let refused = |value: serde_json::Value| serde_json::from_value::<CiOnly>(value).is_err();
        let open = |from: &str| serde_json::json!({"from": from});
        let closed = |from: &str, until: &str| serde_json::json!({"from": from, "until": until});
        let a = "2026-10-10T00:00:00Z";
        let b = "2026-10-15T00:00:00Z";
        let c = "2026-10-20T00:00:00Z";
        assert!(refused(serde_json::json!([
            {"stage": "production", "periods": [open(a)]},
            {"stage": "production", "periods": [open(b)]},
        ])));
        assert!(refused(
            serde_json::json!([{"stage": "Prod uction", "periods": [open(a)]}])
        ));
        assert!(refused(
            serde_json::json!([{"stage": "production", "periods": []}])
        ));
        // Ends before it begins; open before the last; out of order; touching; overlapping.
        for periods in [
            serde_json::json!([closed(b, a)]),
            serde_json::json!([closed(a, a)]),
            serde_json::json!([open(a), closed(b, c)]),
            serde_json::json!([closed(b, c), closed(a, a)]),
            serde_json::json!([closed(a, b), open(b)]),
            serde_json::json!([closed(a, c), open(b)]),
        ] {
            assert!(
                refused(serde_json::json!([{"stage": "production", "periods": periods}])),
                "{periods}"
            );
        }
        // The old shape is refused.
        assert!(refused(
            serde_json::json!([{"stage": "production", "since": a}])
        ));
    }

    // covers: L-13, T-16 (a list without stages; the per-stage list as their union)
    #[test]
    fn reads_periods_without_a_stage_and_the_old_list_as_their_union() {
        let production = StageName::new("production").unwrap();
        let staging = StageName::new("staging").unwrap();
        let at = |d: &str| Some(t(d));

        // A list of periods without a stage: every stage was CI-only then, and so is a question
        // without one.
        let json = serde_json::json!([
            {"from": "2026-10-10T00:00:00.000Z", "until": "2026-10-15T00:00:00.000Z"},
            {"from": "2026-10-20T00:00:00.000Z"},
        ]);
        let rule: CiOnly = serde_json::from_value(json.clone()).unwrap();
        assert!(!rule.is_empty());
        assert_eq!(rule.stages().count(), 0);
        assert_eq!(rule.stageless().len(), 2);
        assert_eq!(rule.since_anywhere(), Some(day(20)));
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            json,
            "written as read"
        );
        assert_eq!(
            rule,
            CiOnly::none()
                .with_stageless_period(day(10), Some(day(15)))
                .with_stageless_period(day(20), None)
        );
        assert!(rule.counts_anywhere(ANA, at("2026-10-09T12:00:00Z")));
        assert!(!rule.counts_anywhere(ANA, at("2026-10-12T12:00:00Z")));
        assert!(rule.counts_anywhere(ANA, at("2026-10-17T12:00:00Z")));
        assert!(!rule.counts_anywhere(ANA, None), "CI-only now");
        assert!(rule.counts_anywhere(CI_SIGNER, None));
        for stage in [&production, &staging] {
            assert_eq!(rule.since(stage), Some(day(20)));
            assert!(!rule.counts(stage, ANA, at("2026-10-12T12:00:00Z")));
            assert!(rule.counts(stage, ANA, at("2026-10-17T12:00:00Z")));
        }

        // The old shape, one entry per stage, reads as before for a stage; without one, as the
        // union of every stage's periods.
        let old: CiOnly = serde_json::from_value(serde_json::json!([
            {"stage": "production", "periods": [{"from": "2026-10-10T00:00:00Z", "until": "2026-10-15T00:00:00Z"}]},
            {"stage": "staging", "periods": [
                {"from": "2026-10-14T00:00:00Z", "until": "2026-10-16T00:00:00Z"},
                {"from": "2026-10-20T00:00:00Z"},
            ]},
        ]))
        .unwrap();
        assert!(old.stageless().is_empty());
        assert!(old.counts(&production, ANA, at("2026-10-15T12:00:00Z")));
        assert!(!old.counts(&staging, ANA, at("2026-10-15T12:00:00Z")));
        assert!(old.counts(&production, ANA, None));
        assert_eq!(old.since(&production), None);
        assert_eq!(
            old.any_periods(),
            [
                Period {
                    from: day(10),
                    until: Some(day(16))
                },
                Period {
                    from: day(20),
                    until: None
                },
            ]
        );
        assert_eq!(old.since_anywhere(), Some(day(20)));
        assert!(!old.counts_anywhere(ANA, at("2026-10-11T00:00:00Z")));
        assert!(!old.counts_anywhere(ANA, at("2026-10-15T12:00:00Z")));
        assert!(old.counts_anywhere(ANA, at("2026-10-17T00:00:00Z")));
        assert!(!old.counts_anywhere(ANA, None));
        // Written as before.
        assert!(
            serde_json::to_value(&old).unwrap()[0]
                .get("stage")
                .is_some()
        );
        assert!(CiOnly::none().counts_anywhere(ANA, None));

        // Both in one list: periods without a stage apply to the stages listed too.
        let both: CiOnly = serde_json::from_value(serde_json::json!([
            {"stage": "production", "periods": [{"from": "2026-10-10T00:00:00Z", "until": "2026-10-12T00:00:00Z"}]},
            {"from": "2026-10-20T00:00:00Z"},
        ]))
        .unwrap();
        assert!(!both.counts(&production, ANA, at("2026-10-11T00:00:00Z")));
        assert!(!both.counts(&production, ANA, at("2026-10-21T00:00:00Z")));
        assert!(!both.counts(&staging, ANA, at("2026-10-21T00:00:00Z")));
        assert!(both.counts(&staging, ANA, at("2026-10-11T00:00:00Z")));
        assert_eq!(
            serde_json::from_value::<CiOnly>(serde_json::to_value(&both).unwrap()).unwrap(),
            both
        );

        // Periods without a stage follow the same rules.
        let refused = |value: serde_json::Value| serde_json::from_value::<CiOnly>(value).is_err();
        let a = "2026-10-10T00:00:00Z";
        let b = "2026-10-15T00:00:00Z";
        for list in [
            serde_json::json!([{"from": b, "until": a}]),
            serde_json::json!([{"from": a}, {"from": b}]),
            serde_json::json!([{"from": b}, {"from": a, "until": a}]),
            serde_json::json!([{"from": a, "until": b}, {"from": b}]),
            serde_json::json!([{"from": a, "stage": "production"}]),
            serde_json::json!([{"from": a, "since": b}]),
        ] {
            assert!(refused(list.clone()), "{list}");
        }
    }

    // covers: T-16 (the union a question without a stage reads)
    #[test]
    fn any_periods_merge_every_stage_and_the_stageless_ones() {
        let production = StageName::new("production").unwrap();
        let staging = StageName::new("staging").unwrap();
        let closed = |a: u32, b: u32| Period {
            from: day(a),
            until: Some(day(b)),
        };
        let open = |a: u32| Period {
            from: day(a),
            until: None,
        };
        // An empty list: nothing is CI-only, everyone counts.
        assert!(CiOnly::from_periods(vec![]).is_empty());
        assert!(CiOnly::none().any_periods().is_empty());
        assert_eq!(CiOnly::none().since_anywhere(), None);
        assert!(CiOnly::none().counts_anywhere(ANA, None));
        assert!(CiOnly::none().counts_anywhere(ANA, Some(day(10))));

        // Overlapping (10-14 with 12-18) and touching (18-19 after 12-18) join; apart (22-23)
        // stays apart.
        let rule = CiOnly::none()
            .with_period(production.clone(), day(12), Some(day(18)))
            .with_period(staging.clone(), day(10), Some(day(14)))
            .with_period(staging.clone(), day(18), Some(day(19)))
            .with_stageless_period(day(22), Some(day(23)));
        assert_eq!(rule.any_periods(), [closed(10, 19), closed(22, 23)]);
        assert_eq!(rule.since_anywhere(), None);
        assert!(rule.counts_anywhere(ANA, None), "nothing is open now");
        assert!(!rule.counts_anywhere(ANA, Some(day(15))));
        assert!(rule.counts_anywhere(ANA, Some(day(20))));
        assert!(!rule.counts_anywhere(ANA, Some(day(22))));
        assert!(rule.counts_anywhere(CI_SIGNER, Some(day(15))));

        // Open last: it swallows a later closed one and decides `since_anywhere`.
        let rule = CiOnly::none()
            .with(production.clone(), day(20))
            .with_period(staging.clone(), day(21), Some(day(25)))
            .with_period(staging.clone(), day(5), Some(day(6)));
        assert_eq!(rule.any_periods(), [closed(5, 6), open(20)]);
        assert_eq!(rule.since_anywhere(), Some(day(20)));
        assert!(!rule.counts_anywhere(ANA, None), "open now");
        assert!(!rule.counts_anywhere(ANA, Some(day(26))));
        assert!(rule.counts_anywhere(ANA, Some(day(19))));
        assert!(rule.counts_anywhere(CI_SIGNER, None));

        // From periods without a stage, in any order: merged the same way, and written as the
        // stage-less list.
        let rule =
            CiOnly::from_periods(vec![open(20), closed(12, 18), closed(10, 14), closed(5, 5)]);
        assert_eq!(rule.stageless(), [closed(10, 18), open(20)]);
        assert_eq!(rule.any_periods(), rule.stageless());
        assert_eq!(rule.stages().count(), 0);
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            serde_json::json!([
                {"from": "2026-10-10T00:00:00.000Z", "until": "2026-10-18T00:00:00.000Z"},
                {"from": "2026-10-20T00:00:00.000Z"},
            ])
        );
    }
}

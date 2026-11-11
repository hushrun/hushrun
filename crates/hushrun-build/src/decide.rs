//! The one function that gives a compile its outcome (B-3), from facts. It does no I/O.
//!
//! [`before`] plans a compile from the mode, the class and what the lookup found. [`after`]
//! turns the plan and what then happened into the [`Outcome`] and whether the first key must be
//! suspended. [`passed_through`] is the outcome of a call that never reached a plan. No other
//! code makes an `Outcome`.

use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough};

/// What the lookup before a compile found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lookup {
    /// Computing a key failed (B-19).
    KeyFailed,
    /// Reading the store failed (B-19).
    StoreFailed,
    /// The first key is suspended (B-15): nothing was looked up.
    Suspended,
    /// No block under the second key.
    Missing,
    /// A block exists under the second key. `audit` is whether the audit picked this compile
    /// (B-16).
    Found {
        /// The audit's pick.
        audit: bool,
    },
}

/// What to do with a compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Plan {
    /// Run rustc exactly as asked; the outcome is passed through, for this reason.
    PassThrough(PassedThrough),
    /// Run rustc exactly as asked and store nothing, for this reason (B-19).
    AsAsked(NotStored),
    /// Put the block's files in place; rustc doesn't start.
    Reuse,
    /// Run rustc with the wrapper's changes (B-9). `store`: keep the block; it is false only
    /// for a suspended key. `compare`: compare the outputs with the block found (B-1, B-16).
    Compile {
        /// Whether to store the block.
        store: bool,
        /// Whether to compare the outputs with the block found.
        compare: bool,
    },
}

/// Plans a compile (B-1, B-15, B-16). In `observe` nothing is reused and a library whose block
/// exists is compared with it. In `on` a found block is reused, except a library the audit
/// picked, which is compiled and compared. A suspended key is never reused and not stored.
/// Programs, build scripts and proc macros are never compared: their bytes are not stable.
pub fn before(mode: Mode, class: Class, lookup: Lookup) -> Plan {
    let comparable = class == Class::Library;
    match (mode, lookup) {
        (Mode::Off, _) => Plan::PassThrough(PassedThrough::Off),
        (_, Lookup::KeyFailed) => Plan::AsAsked(NotStored::Key),
        (_, Lookup::StoreFailed) => Plan::AsAsked(NotStored::StoreRead),
        (_, Lookup::Suspended) => Plan::Compile {
            store: false,
            compare: false,
        },
        (_, Lookup::Missing) => Plan::Compile {
            store: true,
            compare: false,
        },
        (Mode::Observe, Lookup::Found { .. }) => Plan::Compile {
            store: true,
            compare: comparable,
        },
        (Mode::On, Lookup::Found { audit }) => {
            if audit && comparable {
                Plan::Compile {
                    store: true,
                    compare: true,
                }
            } else {
                Plan::Reuse
            }
        }
    }
}

/// How rustc ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rustc {
    /// rustc did not start. Only a reuse that put every file in place does this.
    NotRun,
    /// rustc exited with success.
    Succeeded,
    /// rustc exited with an error, or could not be started.
    Failed,
}

/// What the store said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stored {
    /// Storing was not tried.
    NotTried,
    /// The store published the block.
    Took,
    /// The store had the block already; the new one was dropped (B-13).
    AlreadyHad,
    /// The store refused the block.
    Refused,
}

/// What comparing the outputs with the block found (B-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compared {
    /// No comparison was made.
    NotTried,
    /// Every output is byte for byte the block's.
    Same,
    /// An output differs.
    Differed,
}

/// What happened after the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Facts {
    /// How rustc ended.
    pub rustc: Rustc,
    /// A read file changed while compiling, or carries a date at or after its start (B-11).
    pub changed: bool,
    /// The store's volume is below `min_free_gb` (B-22).
    pub low_space: bool,
    /// The first step that failed, as the reason nothing is stored: hashing, dep-info, the
    /// scan, the link inputs, the comparison, putting files in place, …
    pub error: Option<NotStored>,
    /// What the store said.
    pub stored: Stored,
    /// What the comparison found.
    pub compared: Compared,
}

impl Facts {
    /// rustc ended so, and nothing else happened yet.
    pub const fn ran(rustc: Rustc) -> Facts {
        Facts {
            rustc,
            changed: false,
            low_space: false,
            error: None,
            stored: Stored::NotTried,
            compared: Compared::NotTried,
        }
    }
}

/// A compile's outcome, and whether its first key must be suspended (B-15, B-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Verdict {
    /// The outcome (B-3).
    pub outcome: Outcome,
    /// Suspend the first key: the fresh outputs differed from the block's.
    pub suspend: bool,
}

/// The outcome of a call that runs as asked before any plan: B-4's clauses, CI, Windows, a
/// settings file that can't be read, a store that can't be written.
pub const fn passed_through(why: PassedThrough) -> Outcome {
    Outcome::PassedThrough(why)
}

/// The outcome of a planned compile, from what happened. The first of these that holds decides:
/// a plan that ran as asked keeps its reason; a reuse that rustc didn't follow is reused, and
/// one it did follow is not stored for the error that stopped it; then rustc failing; a read
/// that changed (no comparison is trusted after that); a failed step; outputs that differ from
/// the block (the key is suspended); a suspended key; low free space; the store's answer.
pub fn after(plan: Plan, facts: &Facts) -> Verdict {
    let outcome = match plan {
        Plan::PassThrough(why) => Outcome::PassedThrough(why),
        Plan::AsAsked(why) => Outcome::BuiltNotStored(why),
        Plan::Reuse => match (facts.rustc, facts.error) {
            (Rustc::NotRun, _) => Outcome::Reused,
            (_, Some(why)) => Outcome::BuiltNotStored(why),
            (_, None) => Outcome::BuiltNotStored(NotStored::Restore),
        },
        Plan::Compile { store, compare } => {
            if facts.rustc != Rustc::Succeeded {
                Outcome::BuiltNotStored(NotStored::RustcFailed)
            } else if facts.changed {
                Outcome::BuiltNotStored(NotStored::Changed)
            } else if let Some(why) = facts.error {
                Outcome::BuiltNotStored(why)
            } else if compare && facts.compared == Compared::Differed {
                return Verdict {
                    outcome: Outcome::BuiltNotStored(NotStored::Differed),
                    suspend: true,
                };
            } else if !store {
                Outcome::BuiltNotStored(NotStored::Suspended)
            } else if facts.low_space {
                Outcome::BuiltNotStored(NotStored::LowSpace)
            } else {
                match facts.stored {
                    Stored::Took | Stored::AlreadyHad => Outcome::BuiltAndStored,
                    Stored::Refused => Outcome::BuiltNotStored(NotStored::StoreRefused),
                    Stored::NotTried => Outcome::BuiltNotStored(NotStored::StoreSkipped),
                }
            }
        }
    };
    Verdict {
        outcome,
        suspend: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [Mode; 3] = [Mode::Off, Mode::Observe, Mode::On];
    const LOOKUPS: [Lookup; 6] = [
        Lookup::KeyFailed,
        Lookup::StoreFailed,
        Lookup::Suspended,
        Lookup::Missing,
        Lookup::Found { audit: false },
        Lookup::Found { audit: true },
    ];
    const LIB: &[Class] = &[Class::Library];
    const NOT_LIB: &[Class] = &[Class::ProcMacro, Class::BuildScript, Class::Program];
    const ANY: &[Class] = &Class::ALL;

    const fn compile(store: bool, compare: bool) -> Plan {
        Plan::Compile { store, compare }
    }

    // covers: B-1, B-15, B-16, B-19
    #[test]
    fn before_over_every_combination() {
        let off = Plan::PassThrough(PassedThrough::Off);
        let table: &[(Mode, Lookup, &[Class], Plan)] = &[
            (Mode::Off, Lookup::KeyFailed, ANY, off),
            (Mode::Off, Lookup::StoreFailed, ANY, off),
            (Mode::Off, Lookup::Suspended, ANY, off),
            (Mode::Off, Lookup::Missing, ANY, off),
            (Mode::Off, Lookup::Found { audit: false }, ANY, off),
            (Mode::Off, Lookup::Found { audit: true }, ANY, off),
            (
                Mode::Observe,
                Lookup::KeyFailed,
                ANY,
                Plan::AsAsked(NotStored::Key),
            ),
            (
                Mode::Observe,
                Lookup::StoreFailed,
                ANY,
                Plan::AsAsked(NotStored::StoreRead),
            ),
            (Mode::Observe, Lookup::Suspended, ANY, compile(false, false)),
            (Mode::Observe, Lookup::Missing, ANY, compile(true, false)),
            (
                Mode::Observe,
                Lookup::Found { audit: false },
                LIB,
                compile(true, true),
            ),
            (
                Mode::Observe,
                Lookup::Found { audit: false },
                NOT_LIB,
                compile(true, false),
            ),
            (
                Mode::Observe,
                Lookup::Found { audit: true },
                LIB,
                compile(true, true),
            ),
            (
                Mode::Observe,
                Lookup::Found { audit: true },
                NOT_LIB,
                compile(true, false),
            ),
            (
                Mode::On,
                Lookup::KeyFailed,
                ANY,
                Plan::AsAsked(NotStored::Key),
            ),
            (
                Mode::On,
                Lookup::StoreFailed,
                ANY,
                Plan::AsAsked(NotStored::StoreRead),
            ),
            (Mode::On, Lookup::Suspended, ANY, compile(false, false)),
            (Mode::On, Lookup::Missing, ANY, compile(true, false)),
            (Mode::On, Lookup::Found { audit: false }, ANY, Plan::Reuse),
            (
                Mode::On,
                Lookup::Found { audit: true },
                LIB,
                compile(true, true),
            ),
            (
                Mode::On,
                Lookup::Found { audit: true },
                NOT_LIB,
                Plan::Reuse,
            ),
        ];
        let mut checked = 0;
        for mode in MODES {
            for lookup in LOOKUPS {
                for class in Class::ALL {
                    let rows: Vec<_> = table
                        .iter()
                        .filter(|(m, l, c, _)| *m == mode && *l == lookup && c.contains(&class))
                        .collect();
                    assert_eq!(rows.len(), 1, "{mode:?} {lookup:?} {class:?}");
                    assert_eq!(
                        before(mode, class, lookup),
                        rows[0].3,
                        "{mode:?} {lookup:?} {class:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 72);
    }

    // covers: B-1
    #[test]
    fn observe_never_reuses_and_a_suspended_key_is_never_reused_or_stored() {
        for class in Class::ALL {
            for lookup in LOOKUPS {
                assert_ne!(before(Mode::Observe, class, lookup), Plan::Reuse);
                for mode in MODES {
                    let plan = before(mode, class, Lookup::Suspended);
                    assert!(
                        !matches!(plan, Plan::Reuse | Plan::Compile { store: true, .. }),
                        "{mode:?} {class:?}"
                    );
                }
            }
        }
    }

    /// What a row of the `after` table expects.
    #[derive(Debug, Clone, Copy)]
    enum Expect {
        Is(Outcome),
        Suspends(Outcome),
        /// The plan's own reason.
        PlanReason,
        /// The fact's error.
        TheError,
    }

    /// Which plans a row matches.
    #[derive(Debug, Clone, Copy)]
    enum PlanIs {
        PassThrough,
        AsAsked,
        Reuse,
        Compile {
            store: Option<bool>,
            compare: Option<bool>,
        },
    }

    impl PlanIs {
        fn matches(self, plan: Plan) -> bool {
            match (self, plan) {
                (PlanIs::PassThrough, Plan::PassThrough(_))
                | (PlanIs::AsAsked, Plan::AsAsked(_))
                | (PlanIs::Reuse, Plan::Reuse) => true,
                (
                    PlanIs::Compile {
                        store: s,
                        compare: c,
                    },
                    Plan::Compile { store, compare },
                ) => s.is_none_or(|s| s == store) && c.is_none_or(|c| c == compare),
                _ => false,
            }
        }
    }

    /// One row: each filter is `None` for "any".
    struct Row {
        plan: PlanIs,
        rustc: Option<&'static [Rustc]>,
        changed: Option<bool>,
        error: Option<bool>,
        compared: Option<Compared>,
        low_space: Option<bool>,
        stored: Option<&'static [Stored]>,
        expect: Expect,
    }

    const ROW: Row = Row {
        plan: PlanIs::Compile {
            store: None,
            compare: None,
        },
        rustc: None,
        changed: None,
        error: None,
        compared: None,
        low_space: None,
        stored: None,
        expect: Expect::TheError,
    };

    const BUILT: Option<&[Rustc]> = Some(&[Rustc::Succeeded]);

    fn not_stored(why: NotStored) -> Expect {
        Expect::Is(Outcome::BuiltNotStored(why))
    }

    // covers: B-3, B-11, B-15, B-16, B-19, B-22
    #[test]
    fn after_over_every_combination() {
        // In order: the first row that matches decides.
        let table = [
            Row {
                plan: PlanIs::PassThrough,
                expect: Expect::PlanReason,
                ..ROW
            },
            Row {
                plan: PlanIs::AsAsked,
                expect: Expect::PlanReason,
                ..ROW
            },
            Row {
                plan: PlanIs::Reuse,
                rustc: Some(&[Rustc::NotRun]),
                expect: Expect::Is(Outcome::Reused),
                ..ROW
            },
            Row {
                plan: PlanIs::Reuse,
                error: Some(true),
                expect: Expect::TheError,
                ..ROW
            },
            Row {
                plan: PlanIs::Reuse,
                expect: not_stored(NotStored::Restore),
                ..ROW
            },
            Row {
                rustc: Some(&[Rustc::NotRun, Rustc::Failed]),
                expect: not_stored(NotStored::RustcFailed),
                ..ROW
            },
            Row {
                rustc: BUILT,
                changed: Some(true),
                expect: not_stored(NotStored::Changed),
                ..ROW
            },
            Row {
                rustc: BUILT,
                error: Some(true),
                expect: Expect::TheError,
                ..ROW
            },
            Row {
                plan: PlanIs::Compile {
                    store: None,
                    compare: Some(true),
                },
                rustc: BUILT,
                compared: Some(Compared::Differed),
                expect: Expect::Suspends(Outcome::BuiltNotStored(NotStored::Differed)),
                ..ROW
            },
            Row {
                plan: PlanIs::Compile {
                    store: Some(false),
                    compare: None,
                },
                rustc: BUILT,
                expect: not_stored(NotStored::Suspended),
                ..ROW
            },
            Row {
                rustc: BUILT,
                low_space: Some(true),
                expect: not_stored(NotStored::LowSpace),
                ..ROW
            },
            Row {
                rustc: BUILT,
                stored: Some(&[Stored::Took, Stored::AlreadyHad]),
                expect: Expect::Is(Outcome::BuiltAndStored),
                ..ROW
            },
            Row {
                rustc: BUILT,
                stored: Some(&[Stored::Refused]),
                expect: not_stored(NotStored::StoreRefused),
                ..ROW
            },
            Row {
                rustc: BUILT,
                stored: Some(&[Stored::NotTried]),
                expect: not_stored(NotStored::StoreSkipped),
                ..ROW
            },
        ];

        let plans = [
            Plan::PassThrough(PassedThrough::Probe),
            Plan::AsAsked(NotStored::Key),
            Plan::AsAsked(NotStored::StoreRead),
            Plan::Reuse,
            compile(true, true),
            compile(true, false),
            compile(false, false),
            compile(false, true),
        ];
        let rustcs = [Rustc::NotRun, Rustc::Succeeded, Rustc::Failed];
        let errors = [None, Some(NotStored::Hash), Some(NotStored::Restore)];
        let stores = [
            Stored::NotTried,
            Stored::Took,
            Stored::AlreadyHad,
            Stored::Refused,
        ];
        let compares = [Compared::NotTried, Compared::Same, Compared::Differed];
        let mut checked = 0;
        for plan in plans {
            for rustc in rustcs {
                for changed in [false, true] {
                    for low_space in [false, true] {
                        for error in errors {
                            for stored in stores {
                                for compared in compares {
                                    let facts = Facts {
                                        rustc,
                                        changed,
                                        low_space,
                                        error,
                                        stored,
                                        compared,
                                    };
                                    let row = table
                                        .iter()
                                        .find(|row| {
                                            row.plan.matches(plan)
                                                && row.rustc.is_none_or(|r| r.contains(&rustc))
                                                && row.changed.is_none_or(|c| c == changed)
                                                && row.error.is_none_or(|e| e == error.is_some())
                                                && row.compared.is_none_or(|c| c == compared)
                                                && row.low_space.is_none_or(|l| l == low_space)
                                                && row.stored.is_none_or(|s| s.contains(&stored))
                                        })
                                        .unwrap_or_else(|| panic!("no row: {plan:?} {facts:?}"));
                                    let expected = match row.expect {
                                        Expect::Is(outcome) => Verdict {
                                            outcome,
                                            suspend: false,
                                        },
                                        Expect::Suspends(outcome) => Verdict {
                                            outcome,
                                            suspend: true,
                                        },
                                        Expect::PlanReason => Verdict {
                                            outcome: match plan {
                                                Plan::PassThrough(why) => {
                                                    Outcome::PassedThrough(why)
                                                }
                                                Plan::AsAsked(why) => Outcome::BuiltNotStored(why),
                                                _ => unreachable!(),
                                            },
                                            suspend: false,
                                        },
                                        Expect::TheError => Verdict {
                                            outcome: Outcome::BuiltNotStored(error.unwrap()),
                                            suspend: false,
                                        },
                                    };
                                    assert_eq!(after(plan, &facts), expected, "{plan:?} {facts:?}");
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 8 * 3 * 2 * 2 * 3 * 4 * 3);
    }

    // covers: B-3
    #[test]
    fn only_a_reuse_rustc_never_ran_is_reused() {
        let reused = after(Plan::Reuse, &Facts::ran(Rustc::NotRun));
        assert_eq!(reused.outcome, Outcome::Reused);
        let fell_back = Facts {
            error: Some(NotStored::Restore),
            ..Facts::ran(Rustc::Succeeded)
        };
        assert_eq!(
            after(Plan::Reuse, &fell_back).outcome,
            Outcome::BuiltNotStored(NotStored::Restore)
        );
        let stored = Facts {
            stored: Stored::Took,
            ..Facts::ran(Rustc::Succeeded)
        };
        assert_eq!(
            after(compile(true, false), &stored).outcome,
            Outcome::BuiltAndStored
        );
        assert_eq!(
            passed_through(PassedThrough::Ci),
            Outcome::PassedThrough(PassedThrough::Ci)
        );
    }

    // covers: B-16
    #[test]
    fn a_difference_suspends_only_when_compared_and_trusted() {
        let differed = Facts {
            compared: Compared::Differed,
            stored: Stored::AlreadyHad,
            ..Facts::ran(Rustc::Succeeded)
        };
        assert!(after(compile(true, true), &differed).suspend);
        assert!(!after(compile(true, false), &differed).suspend);
        let changed = Facts {
            changed: true,
            ..differed
        };
        assert_eq!(
            after(compile(true, true), &changed),
            Verdict {
                outcome: Outcome::BuiltNotStored(NotStored::Changed),
                suspend: false,
            }
        );
        let same = Facts {
            compared: Compared::Same,
            ..differed
        };
        assert_eq!(
            after(compile(true, true), &same).outcome,
            Outcome::BuiltAndStored
        );
    }
}

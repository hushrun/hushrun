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
    /// The key can't be complete, or nothing this compile gives can be stored, for this
    /// reason: the build script's output can't be read, the program writes debug files a block
    /// doesn't carry. Nothing was looked up.
    Unstorable(NotStored),
    /// No block under the second key.
    Missing,
    /// A block exists under the second key.
    Found {
        /// Whether the audit picked this compile (B-16).
        audit: bool,
        /// Whether the block's messages serve this call's form (B-12).
        serves_form: bool,
    },
}

/// The facts [`before`] plans from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Before {
    /// The run's mode.
    pub mode: Mode,
    /// What the call produces.
    pub class: Class,
    /// What the lookup found.
    pub lookup: Lookup,
    /// The call asks for `-C incremental` and its dep-info file is in the output folder
    /// already: a rebuild in a warm target, not a first build there (B-9).
    pub incremental_rebuild: bool,
}

/// Whether a compile's block is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keep {
    /// Store the block.
    Store,
    /// Store nothing, for this reason.
    Not(NotStored),
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
    /// Run rustc with the wrapper's changes (B-9). `keep`: whether to store the block.
    /// `compare`: compare the outputs with the block found (B-1, B-16).
    Compile {
        /// Whether to store the block.
        keep: Keep,
        /// Whether to compare the outputs with the block found.
        compare: bool,
    },
}

/// Plans a compile (B-1, B-9, B-12, B-15, B-16). In `observe` nothing is reused and a library
/// whose block exists is compared with it. In `on` a found block is reused, except a library the
/// audit picked, which is compiled and compared, and a block whose messages don't serve this
/// call's form, which is compiled. A suspended key is never reused and not stored. A rebuild in
/// a warm target that finds no block stays incremental and runs as asked. Programs, build
/// scripts and proc macros are never compared: their bytes are not stable.
pub fn before(facts: &Before) -> Plan {
    let comparable = facts.class == Class::Library;
    let compile = |keep, compare| Plan::Compile { keep, compare };
    match (facts.mode, facts.lookup) {
        (Mode::Off, _) => Plan::PassThrough(PassedThrough::Off),
        (_, Lookup::KeyFailed) => Plan::AsAsked(NotStored::Key),
        (_, Lookup::StoreFailed) => Plan::AsAsked(NotStored::StoreRead),
        (_, Lookup::Missing | Lookup::Suspended | Lookup::Unstorable(_))
            if facts.incremental_rebuild =>
        {
            Plan::PassThrough(PassedThrough::Incremental)
        }
        (_, Lookup::Suspended) => compile(Keep::Not(NotStored::Suspended), false),
        (_, Lookup::Unstorable(why)) => compile(Keep::Not(why), false),
        (_, Lookup::Missing) => compile(Keep::Store, false),
        (Mode::Observe, Lookup::Found { .. }) => compile(Keep::Store, comparable),
        (Mode::On, Lookup::Found { audit, serves_form }) => {
            if (audit && comparable) || !serves_form {
                compile(Keep::Store, comparable)
            } else {
                Plan::Reuse
            }
        }
    }
}

/// The plan when putting a block in place failed before any message was written (B-12, B-19):
/// rustc runs as asked and nothing is stored.
pub const fn restore_failed() -> Plan {
    Plan::AsAsked(NotStored::Restore)
}

/// rustc's codes for a crate it could not load (B-15): only these, never an error in the
/// dependent's own code, suspend a reused block.
pub const CRATE_LOADING: [&str; 7] = [
    "E0460", "E0461", "E0462", "E0463", "E0464", "E0514", "E0786",
];

/// Whether a failed compile's error codes say it could not load a crate (B-15).
pub fn could_not_load<'a>(codes: impl IntoIterator<Item = &'a str>) -> bool {
    codes.into_iter().any(|code| CRATE_LOADING.contains(&code))
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
/// the block (the key is suspended); a plan that keeps nothing; low free space; the store's
/// answer.
pub fn after(plan: Plan, facts: &Facts) -> Verdict {
    let outcome = match plan {
        Plan::PassThrough(why) => Outcome::PassedThrough(why),
        Plan::AsAsked(why) => Outcome::BuiltNotStored(why),
        Plan::Reuse => match (facts.rustc, facts.error) {
            (Rustc::NotRun, _) => Outcome::Reused,
            (_, Some(why)) => Outcome::BuiltNotStored(why),
            (_, None) => Outcome::BuiltNotStored(NotStored::Restore),
        },
        Plan::Compile { keep, compare } => {
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
            } else if let Keep::Not(why) = keep {
                Outcome::BuiltNotStored(why)
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
    const LOOKUPS: [Lookup; 10] = [
        Lookup::KeyFailed,
        Lookup::StoreFailed,
        Lookup::Suspended,
        Lookup::Unstorable(NotStored::ScriptOutputMissing),
        Lookup::Missing,
        Lookup::Found {
            audit: false,
            serves_form: true,
        },
        Lookup::Found {
            audit: true,
            serves_form: true,
        },
        Lookup::Found {
            audit: false,
            serves_form: false,
        },
        Lookup::Found {
            audit: true,
            serves_form: false,
        },
        Lookup::Unstorable(NotStored::DebugFiles),
    ];

    const fn compile(keep: Keep, compare: bool) -> Plan {
        Plan::Compile { keep, compare }
    }

    /// The plan the rules give, written out case by case.
    fn expected(mode: Mode, class: Class, lookup: Lookup, rebuild: bool) -> Plan {
        let lib = class == Class::Library;
        if mode == Mode::Off {
            return Plan::PassThrough(PassedThrough::Off);
        }
        match lookup {
            Lookup::KeyFailed => Plan::AsAsked(NotStored::Key),
            Lookup::StoreFailed => Plan::AsAsked(NotStored::StoreRead),
            Lookup::Missing | Lookup::Suspended | Lookup::Unstorable(_) if rebuild => {
                Plan::PassThrough(PassedThrough::Incremental)
            }
            Lookup::Suspended => compile(Keep::Not(NotStored::Suspended), false),
            Lookup::Unstorable(why) => compile(Keep::Not(why), false),
            Lookup::Missing => compile(Keep::Store, false),
            Lookup::Found { .. } if mode == Mode::Observe => compile(Keep::Store, lib),
            Lookup::Found {
                serves_form: false, ..
            } => compile(Keep::Store, lib),
            Lookup::Found { audit: true, .. } if lib => compile(Keep::Store, true),
            Lookup::Found { .. } => Plan::Reuse,
        }
    }

    // covers: B-1, B-9, B-12, B-15, B-16, B-19
    #[test]
    fn before_over_every_combination() {
        let mut checked = 0;
        for mode in MODES {
            for lookup in LOOKUPS {
                for class in Class::ALL {
                    for rebuild in [false, true] {
                        let facts = Before {
                            mode,
                            class,
                            lookup,
                            incremental_rebuild: rebuild,
                        };
                        assert_eq!(
                            before(&facts),
                            expected(mode, class, lookup, rebuild),
                            "{facts:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 10 * 4 * 2);
    }

    // covers: B-1, B-15
    #[test]
    fn observe_never_reuses_and_a_suspended_key_is_never_reused_or_stored() {
        for class in Class::ALL {
            for rebuild in [false, true] {
                for lookup in LOOKUPS {
                    let facts = Before {
                        mode: Mode::Observe,
                        class,
                        lookup,
                        incremental_rebuild: rebuild,
                    };
                    assert_ne!(before(&facts), Plan::Reuse);
                }
                for mode in MODES {
                    let plan = before(&Before {
                        mode,
                        class,
                        lookup: Lookup::Suspended,
                        incremental_rebuild: rebuild,
                    });
                    assert!(
                        !matches!(
                            plan,
                            Plan::Reuse
                                | Plan::Compile {
                                    keep: Keep::Store,
                                    ..
                                }
                        ),
                        "{mode:?} {class:?}"
                    );
                }
            }
        }
    }

    // covers: B-9
    #[test]
    fn an_edit_in_a_warm_target_stays_incremental_and_a_first_build_is_stored() {
        let first = Before {
            mode: Mode::On,
            class: Class::Library,
            lookup: Lookup::Missing,
            incremental_rebuild: false,
        };
        assert_eq!(before(&first), compile(Keep::Store, false));
        let rebuild = Before {
            incremental_rebuild: true,
            ..first
        };
        assert_eq!(
            before(&rebuild),
            Plan::PassThrough(PassedThrough::Incremental)
        );
        let found = Before {
            lookup: Lookup::Found {
                audit: false,
                serves_form: true,
            },
            ..rebuild
        };
        assert_eq!(before(&found), Plan::Reuse);
    }

    // covers: B-15
    #[test]
    fn only_crate_loading_errors_reject_a_reused_block() {
        for code in CRATE_LOADING {
            assert!(could_not_load([code]));
        }
        assert!(!could_not_load(["E0425", "E0308"]));
        assert!(!could_not_load([]));
        assert!(could_not_load(["E0425", "E0463"]));
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
            keep: Option<Keep>,
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
                        keep: k,
                        compare: c,
                    },
                    Plan::Compile { keep, compare },
                ) => k.is_none_or(|k| k == keep) && c.is_none_or(|c| c == compare),
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
            keep: None,
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
                    keep: None,
                    compare: Some(true),
                },
                rustc: BUILT,
                compared: Some(Compared::Differed),
                expect: Expect::Suspends(Outcome::BuiltNotStored(NotStored::Differed)),
                ..ROW
            },
            Row {
                plan: PlanIs::Compile {
                    keep: Some(Keep::Not(NotStored::Suspended)),
                    compare: None,
                },
                rustc: BUILT,
                expect: not_stored(NotStored::Suspended),
                ..ROW
            },
            Row {
                plan: PlanIs::Compile {
                    keep: Some(Keep::Not(NotStored::ScriptOutputMissing)),
                    compare: None,
                },
                rustc: BUILT,
                expect: not_stored(NotStored::ScriptOutputMissing),
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
            compile(Keep::Store, true),
            compile(Keep::Store, false),
            compile(Keep::Not(NotStored::Suspended), false),
            compile(Keep::Not(NotStored::Suspended), true),
            compile(Keep::Not(NotStored::ScriptOutputMissing), false),
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
        assert_eq!(checked, 9 * 3 * 2 * 2 * 3 * 4 * 3);
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
            after(compile(Keep::Store, false), &stored).outcome,
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
        assert!(after(compile(Keep::Store, true), &differed).suspend);
        assert!(!after(compile(Keep::Store, false), &differed).suspend);
        let changed = Facts {
            changed: true,
            ..differed
        };
        assert_eq!(
            after(compile(Keep::Store, true), &changed),
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
            after(compile(Keep::Store, true), &same).outcome,
            Outcome::BuiltAndStored
        );
    }
}

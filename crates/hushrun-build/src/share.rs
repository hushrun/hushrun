//! What a run does with its blocks (B-25, B-33): reuse on this machine, and sharing through a
//! backend, as two modes. One function, [`what_a_run_does`], decides both from who runs, the
//! person's two settings and what the org said. Nothing stores the answer: every caller asks it
//! again from the same facts (the session as `hushrun run` begins, the relay once `open`
//! answers, `hushrun build status`), so no two places can disagree.
//!
//! The local mode is B-1's, unchanged. The shared mode is the org's (`observe` or `on`, set by an
//! admin and returned by `open`), which a person may lower for their machine with `hushrun build
//! share` or `HUSHRUN_BUILD_SHARE`, and never raise. Sharing is never above the local mode: with
//! local `observe`, sharing observes too.

use serde::{Deserialize, Serialize};

use crate::model::Mode;

/// Who a run is, as the rules for sharing tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// A person's machine. `signed_in`: a backend and a credential for it (a login or a personal
    /// token), so `open` can be asked.
    Person { signed_in: bool },
    /// A CI job. `id_token`: the job can mint an ID token for the backend (GitHub Actions).
    /// `agent_token`: an agent token is the job's credential, as in CI outside GitHub Actions,
    /// where it reads the org's blocks and uploads none (B-31).
    Ci { id_token: bool, agent_token: bool },
    /// An agent token. `sandbox`: a machine with no person's settings (a cloud sandbox); else a
    /// person's machine where the agent runs, whose local mode applies.
    Agent { sandbox: bool },
}

/// What the org said about sharing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Org {
    /// No backend, no `Hushrun-Build: on` on the run's results lookup, or `open` said no.
    NotSharing,
    /// The org shares. Its mode, once `open` has answered; `None` before.
    Shares(Option<Mode>),
}

/// Why sharing is off for a run. Each is a word for `hushrun build status` and the run's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Off {
    /// The local mode is `off`: nothing is reused or stored here, so nothing is shared.
    LocalOff,
    /// No backend is named, or nothing here signs in to it.
    SignedOut,
    /// The org doesn't share build blocks.
    OrgDoesntShare,
    /// The person turned sharing off on this machine.
    OwnSetting,
    /// A CI job that can't mint an ID token.
    NoIdToken,
}

impl Off {
    /// Why, as a line ends with it.
    pub fn reason(self) -> &'static str {
        match self {
            Off::LocalOff => "build reuse is off on this machine",
            Off::SignedOut => "no backend is signed in to",
            Off::OrgDoesntShare => "the org doesn't share build blocks",
            Off::OwnSetting => "sharing is off on this machine (hushrun build share)",
            Off::NoIdToken => "the job can't mint an ID token",
        }
    }
}

/// What sharing does in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    /// Nothing goes to or comes from a backend, for this reason.
    Off(Off),
    /// Upload what this run builds, download and compare, never reuse a download.
    Observe,
    /// Upload what this run builds, and reuse downloads.
    On,
}

impl Sharing {
    /// The word `hushrun build status` writes.
    pub fn word(self) -> &'static str {
        match self {
            Sharing::Off(_) => "off",
            Sharing::Observe => "observe",
            Sharing::On => "on",
        }
    }
}

/// What a run does: whether it reuses here (B-1's mode), and what it shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Does {
    /// Reuse on this machine: B-1's mode for the run.
    pub local: Mode,
    /// Sharing through the backend. Before `open` answers it is the most the run may share; the
    /// org's mode can only lower it.
    pub sharing: Sharing,
    /// Whether the run may upload what it builds: with sharing on or observing, but never a CI
    /// job without an ID token (B-31). The policy `open` answers decides the rest.
    pub uploads: bool,
}

/// The lower of two modes: `off` below `observe` below `on`.
pub fn lowest(a: Mode, b: Mode) -> Mode {
    let rank = |mode: Mode| match mode {
        Mode::Off => 0,
        Mode::Observe => 1,
        Mode::On => 2,
    };
    if rank(a) <= rank(b) { a } else { b }
}

fn sharing_of(mode: Mode, why_off: Off) -> Sharing {
    match mode {
        Mode::Off => Sharing::Off(why_off),
        Mode::Observe => Sharing::Observe,
        Mode::On => Sharing::On,
    }
}

/// The one function for what a run does (B-25, B-33):
///
/// | Who | Local reuse | Sharing |
/// |---|---|---|
/// | A person, local mode `off` | off | off |
/// | A person signed in, the org shares, own setting not `off` | the local mode | the lowest of the org's mode, the own setting and the local mode |
/// | A person otherwise | the local mode | off |
/// | CI with an ID token, the org shares | on, in the job's own store | the org's mode |
/// | CI with an agent token and no ID token, the org shares | on, in the job's own store | the org's mode, reading only |
/// | CI otherwise | off | off |
/// | An agent token, the org shares | the machine's local mode, or on in a sandbox | the org's mode, lowered on a person's machine as for the person |
/// | An agent token otherwise | the machine's local mode, or off in a sandbox | off |
///
/// `own` is the person's share setting (B-33), `local` B-1's mode, both for this command.
pub fn what_a_run_does(caller: Caller, local: Mode, own: Mode, org: Org) -> Does {
    let (local, sharing) = modes(caller, local, own, org);
    let reads_only = matches!(
        caller,
        Caller::Ci {
            id_token: false,
            ..
        }
    );
    Does {
        local,
        sharing,
        uploads: !reads_only && !matches!(sharing, Sharing::Off(_)),
    }
}

fn modes(caller: Caller, local: Mode, own: Mode, org: Org) -> (Mode, Sharing) {
    // Before `open` answers, the org's mode is taken as `on`, the most it can be.
    let org_mode = match org {
        Org::NotSharing => None,
        Org::Shares(mode) => Some(mode.unwrap_or(Mode::On)),
    };
    match caller {
        Caller::Person { signed_in } => {
            if local == Mode::Off {
                return (local, Sharing::Off(Off::LocalOff));
            }
            let sharing = match (signed_in, org_mode) {
                (false, _) => Sharing::Off(Off::SignedOut),
                (true, None) => Sharing::Off(Off::OrgDoesntShare),
                (true, Some(_)) if own == Mode::Off => Sharing::Off(Off::OwnSetting),
                (true, Some(org)) => sharing_of(lowest(lowest(org, own), local), Off::OwnSetting),
            };
            (local, sharing)
        }
        Caller::Ci {
            id_token,
            agent_token,
        } => match (id_token || agent_token, org_mode) {
            (true, Some(org)) => (Mode::On, sharing_of(org, Off::OrgDoesntShare)),
            (false, Some(_)) => (Mode::Off, Sharing::Off(Off::NoIdToken)),
            (_, None) => (Mode::Off, Sharing::Off(Off::OrgDoesntShare)),
        },
        Caller::Agent { sandbox } => {
            let local = if sandbox {
                match org_mode {
                    Some(_) => Mode::On,
                    None => Mode::Off,
                }
            } else {
                local
            };
            let sharing = match org_mode {
                None => Sharing::Off(Off::OrgDoesntShare),
                Some(_) if local == Mode::Off => Sharing::Off(Off::LocalOff),
                Some(org) if sandbox => sharing_of(org, Off::OrgDoesntShare),
                Some(_) if own == Mode::Off => Sharing::Off(Off::OwnSetting),
                Some(org) => sharing_of(lowest(lowest(org, own), local), Off::OwnSetting),
            };
            (local, sharing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [Mode; 3] = [Mode::Off, Mode::Observe, Mode::On];

    fn orgs() -> Vec<Org> {
        let mut orgs = vec![Org::NotSharing, Org::Shares(None)];
        orgs.extend(MODES.map(|m| Org::Shares(Some(m))));
        orgs
    }

    // covers: B-25, B-33
    #[test]
    fn sharing_is_never_above_the_local_mode_the_own_setting_or_the_org() {
        let rank = |s: Sharing| match s {
            Sharing::Off(_) => Mode::Off,
            Sharing::Observe => Mode::Observe,
            Sharing::On => Mode::On,
        };
        for signed_in in [false, true] {
            for local in MODES {
                for own in MODES {
                    for org in orgs() {
                        let does = what_a_run_does(Caller::Person { signed_in }, local, own, org);
                        assert_eq!(does.local, local, "a person's local reuse is B-1's mode");
                        let shared = rank(does.sharing);
                        assert_eq!(lowest(shared, local), shared);
                        assert_eq!(lowest(shared, own), shared);
                        if let Org::Shares(Some(org)) = org {
                            assert_eq!(lowest(shared, org), shared);
                        }
                        if !signed_in || org == Org::NotSharing {
                            assert!(matches!(does.sharing, Sharing::Off(_)));
                        }
                    }
                }
            }
        }
    }

    // covers: B-33
    #[test]
    fn observing_the_shared_store_keeps_local_reuse() {
        let does = what_a_run_does(
            Caller::Person { signed_in: true },
            Mode::On,
            Mode::On,
            Org::Shares(Some(Mode::Observe)),
        );
        assert_eq!(does.local, Mode::On);
        assert_eq!(does.sharing, Sharing::Observe);
        // The person lowers it; they can't raise it.
        let lowered = what_a_run_does(
            Caller::Person { signed_in: true },
            Mode::On,
            Mode::Observe,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(lowered.sharing, Sharing::Observe);
        let off = what_a_run_does(
            Caller::Person { signed_in: true },
            Mode::On,
            Mode::Off,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(off.sharing, Sharing::Off(Off::OwnSetting));
        let local_observe = what_a_run_does(
            Caller::Person { signed_in: true },
            Mode::Observe,
            Mode::On,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(local_observe.sharing, Sharing::Observe);
    }

    // covers: B-25
    #[test]
    fn ci_and_agents_follow_the_table() {
        let ci = |id_token, agent_token, org| {
            what_a_run_does(
                Caller::Ci {
                    id_token,
                    agent_token,
                },
                Mode::Off,
                Mode::On,
                org,
            )
        };
        for agent_token in [false, true] {
            assert_eq!(
                ci(true, agent_token, Org::Shares(Some(Mode::Observe))),
                Does {
                    local: Mode::On,
                    sharing: Sharing::Observe,
                    uploads: true,
                }
            );
            assert_eq!(ci(true, agent_token, Org::NotSharing).local, Mode::Off);
        }
        assert_eq!(
            ci(false, false, Org::Shares(Some(Mode::On))).local,
            Mode::Off
        );
        // Outside GitHub Actions an agent token reads the org's blocks into the job's store, and
        // uploads none.
        assert_eq!(
            ci(false, true, Org::Shares(Some(Mode::On))),
            Does {
                local: Mode::On,
                sharing: Sharing::On,
                uploads: false,
            }
        );
        let sandbox = what_a_run_does(
            Caller::Agent { sandbox: true },
            Mode::Off,
            Mode::Off,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(
            sandbox,
            Does {
                local: Mode::On,
                sharing: Sharing::On,
                uploads: true,
            }
        );
        // On a person's machine, the agent gets the machine's local mode, and the person's
        // setting lowers what it shares.
        let laptop = what_a_run_does(
            Caller::Agent { sandbox: false },
            Mode::Off,
            Mode::On,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(laptop.local, Mode::Off);
        assert!(matches!(laptop.sharing, Sharing::Off(_)));
        let lowered = what_a_run_does(
            Caller::Agent { sandbox: false },
            Mode::On,
            Mode::Observe,
            Org::Shares(Some(Mode::On)),
        );
        assert_eq!(lowered.sharing, Sharing::Observe);
    }
}

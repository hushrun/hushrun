//! What a build tool's cache does with the store and the relay whatever its protocol (B-80,
//! B-81, B-174, B-175): asks the relay for a download the check's manifest names, asks whether a
//! block may be reused (B-34), refuses a block that fails the protocol's checks, records a
//! download's first reuse, and logs. The protocol says what a block is and how it is checked.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_core::blocks::Hex32;

use crate::model::{Kind, Mode};
use crate::origin::Origin;
use crate::relay::{self, Answer, Problem, Request};
use crate::session::{Current, Entry};
use crate::store::Store;

/// The longest a question waits on a relay that says nothing (ADR 0144): a relay working on an
/// answer says so every heartbeat, so this bounds a frozen or gone relay only, and every question
/// waits the same, whatever its unit costs.
pub const RELAY_WAIT: Duration = relay::WAIT;

/// One check's session, as a tool's cache of kind `kind` reaches it.
pub struct Shared {
    pub current: Current,
    pub kind: Kind,
    /// The relay failed or timed out once: nothing more is asked of it this run.
    relay_down: AtomicBool,
    /// Keys whose first reuse of a download here was counted, so it is counted once.
    first_reuses: Mutex<HashSet<Digest>>,
}

impl Shared {
    pub fn new(current: Current, kind: Kind) -> Shared {
        Shared {
            current,
            kind,
            relay_down: AtomicBool::new(false),
            first_reuses: Mutex::new(HashSet::new()),
        }
    }

    /// The download of `key`, only where the run shares with sharing on (the relay says) and the
    /// check's manifest of the cache's kind names it (B-80, B-174): whether it is in the store now.
    pub fn fetch(&self, store: &Store, key: &Digest) -> bool {
        if self.current.mode != Mode::On {
            return false;
        }
        let asked = Request::ToolFetch {
            kind: self.kind,
            key: *key,
            local_ms: store.step_cost(key),
        };
        matches!(
            self.ask(&asked, RELAY_WAIT),
            Some(Answer::Ready { key: got }) if got == *key
        )
    }

    /// Whether the block under `key`, whose record says `origin` and whose outputs are
    /// `outputs`, may be reused here (B-34): `Current::may_reuse`, the question to the relay
    /// being the cache's vouch.
    pub fn vouched(
        &self,
        key: &Digest,
        origin: Option<&Origin>,
        outputs: Hex32,
        at: SystemTime,
    ) -> bool {
        let vouch = Request::ToolVouch {
            kind: self.kind,
            key: *key,
            outputs,
        };
        self.current
            .may_reuse(origin, epoch_ms(at), vouch, &mut |_, request| {
                self.ask(request, RELAY_WAIT)
            })
    }

    /// B-78, B-172: a block that isn't a canonical entry of its key is never served. One that
    /// was `downloaded` is reported to the scope its record names, and its key suspended for the
    /// kept days, as every adapter's refused downloads are; then it goes, and `entry` is logged
    /// with a note. `what` is one such block in words: "Gradle entry".
    pub fn refuse(
        &self,
        store: &Store,
        key: &Digest,
        downloaded: bool,
        what: &str,
        entry: Entry,
        at: SystemTime,
    ) {
        if downloaded {
            let _ = self.ask(
                &Request::ToolReport {
                    kind: self.kind,
                    key: *key,
                    problem: Problem::Rejected,
                },
                RELAY_WAIT,
            );
            let until = at + Duration::from_secs(u64::from(self.current.keep_days) * 86_400);
            let _ = store.suspend_until(
                key,
                &format!("a downloaded {what} failed its checks"),
                until,
                at,
            );
        }
        let _ = store.discard(key);
        self.log(Entry {
            note: Some(format!(
                "a {what} in the store was refused: it failed its checks"
            )),
            ..entry
        });
    }

    /// The block under `key` is served: used now, and, when it was only downloaded and this is
    /// its first reuse here, its record says so (B-34).
    pub fn reused(&self, store: &Store, key: &Digest, origin: Option<&Origin>, at: SystemTime) {
        let _ = store.used(key, at);
        let downloaded = origin.is_some_and(Origin::downloaded_only);
        let first_download = downloaded
            && origin.is_some_and(|o| !o.reused)
            && self.first_reuses.lock().is_ok_and(|mut s| s.insert(*key));
        if first_download {
            let _ = store.update_origin(key, |origin| {
                let mut origin = origin?;
                (!origin.reused).then(|| {
                    origin.reused = true;
                    origin
                })
            });
        }
    }

    /// B-32, B-81, B-175: stores `files` under `key`, with `description`, as a block this run
    /// built: in place of one only downloaded, never of one built here. Whether it is stored.
    pub fn store_built(
        &self,
        store: &Store,
        key: &Digest,
        description: &serde_json::Value,
        files: &[crate::store::NewFile],
        at: SystemTime,
    ) -> bool {
        let share = self.current.share.relay();
        let run = share
            .map(|share| share.run.clone())
            .or_else(|| {
                self.current
                    .folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let origin = Origin::built_here(&run, share.map(|share| share.by.clone()));
        store
            .replace(key, description, files, &origin, at, |there, _| {
                there.is_some_and(Origin::downloaded_only)
            })
            .is_ok_and(|replaced| replaced == crate::store::Replaced::Stored)
    }

    // --- the relay and the log ---------------------------------------------------------

    /// Asks the run's relay within `wait`: no relay, or one call that failed or timed out this
    /// run, and nothing more is asked (B-26).
    pub fn ask(&self, request: &Request, wait: Duration) -> Option<Answer> {
        if self.relay_down.load(Ordering::SeqCst) {
            return None;
        }
        let share = self.current.share.relay()?;
        let answer = relay::ask(&share.socket, request, wait);
        if answer.is_none() {
            self.relay_down.store(true, Ordering::SeqCst);
        }
        answer
    }

    /// Tells the run's relay, as [`Shared::ask`] asks.
    pub fn tell(&self, request: &Request) {
        if self.relay_down.load(Ordering::SeqCst) {
            return;
        }
        if let Some(share) = self.current.share.relay()
            && !relay::tell(&share.socket, request, RELAY_WAIT)
        {
            self.relay_down.store(true, Ordering::SeqCst);
        }
    }

    pub fn log(&self, entry: Entry) {
        self.current.log(&entry);
    }

    /// The session's folder.
    pub fn folder(&self) -> &Path {
        &self.current.folder
    }
}

pub fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

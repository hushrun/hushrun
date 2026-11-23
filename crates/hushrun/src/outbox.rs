//! \[ADR 0106\] The results outbox (C-53): a result a server couldn't be reached to record waits in
//! `<cache dir>/outbox/<key>.json` until a later run, `hushrun ready` or `hushrun flush` reaches it,
//! and while the server is out of reach, lookups read it, so a check this machine passed skips.
//!
//! Each entry has one state, decided by [`decide`] and nowhere else:
//!
//! | From | To | When |
//! |---|---|---|
//! | none | `pending` | a result's request didn't reach its server |
//! | `pending` | `sent` | the server took it (2xx) |
//! | `pending` | `refused` | the server refused it for good (4xx but 408 and 429) |
//! | `pending` | `expired` | 7 days passed, by this machine's clock when read |
//!
//! `sent`, `refused` and `expired` stay as they are. An entry is written whole, through a
//! temporary file and a rename, and every read-change-write holds `outbox.lock`, so two runs at
//! once neither lose an entry nor send one twice; a flush that can't take the lock leaves it to
//! the run that has it. A newer result of the same key replaces an entry still pending: the server
//! answers the newest anyway. Sending again is safe: the entry's `id` is its body's SHA-256, and
//! the server keeps one result of a key per body.
//!
//! Never in CI: a runner's cache is shared with no one, and whoever could write a cache CI restores
//! could have a job send what they wrote with its own credential (ADR 0067). An entry is sent only
//! to the server it was for, with the same kind of credential that wrote it.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use hushrun_core::digest::Digest;

use crate::results::RunResult;

/// The schema of an entry.
pub const SCHEMA: &str = "hushrun.outbox/v1";

/// How long a pending entry waits before it is given up: 7 days.
pub const KEPT_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// An entry's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pending,
    Sent,
    Refused,
    Expired,
}

/// What a try to send an entry found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The server took it.
    Accepted,
    /// The server refused it for good.
    Refused,
    /// It didn't reach the server, or the server failed (5xx, 408, 429).
    Unreachable,
}

/// The one function that decides an entry's state: what it was, when it was written, the time
/// now, and what a try to send it found, if one was made.
pub fn decide(state: State, created_at: i64, now: i64, outcome: Option<Outcome>) -> State {
    match (state, outcome) {
        (State::Pending, Some(Outcome::Accepted)) => State::Sent,
        (State::Pending, Some(Outcome::Refused)) => State::Refused,
        (State::Pending, _) if now - created_at >= KEPT_MS => State::Expired,
        (state, _) => state,
    }
}

/// One entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub schema: String,
    pub state: State,
    /// The run key, hex.
    pub key: String,
    /// The SHA-256 of the result's body, hex: what makes sending it again the same result.
    pub id: String,
    /// The server's org URL it is for.
    pub base: String,
    /// How the credential that wrote it is named (`Credential::named`): only the same kind sends it.
    pub credential: String,
    pub result: RunResult,
    /// When it was written, in milliseconds since the epoch.
    pub created_at: i64,
    #[serde(default)]
    pub attempts: u32,
    /// Why the last try didn't send it, in the server's words: never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When it was sent, refused or given up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<i64>,
}

/// `<cache dir>/outbox`.
pub fn directory() -> Option<PathBuf> {
    crate::context::cache_dir().map(|cache| cache.join("outbox"))
}

/// Milliseconds since the epoch, now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Holds `outbox.lock` in `dir` while it lives.
pub struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    /// Waits for the lock.
    pub fn take(dir: &Path) -> io::Result<Lock> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("outbox.lock"))?;
        if let Err(err) = file.lock() {
            tracing::debug!("can't lock the outbox: {err}");
        }
        Ok(Lock(file))
    }

    /// The lock, when nothing else holds it.
    pub fn try_take(dir: &Path) -> io::Result<Option<Lock>> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("outbox.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Lock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(err)) => {
                tracing::debug!("can't lock the outbox: {err}");
                Ok(Some(Lock(file)))
            }
        }
    }
}

fn path_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

/// The entry of `key`, when there is one that reads.
pub fn read(dir: &Path, key: &str) -> Option<Entry> {
    let bytes = std::fs::read(path_of(dir, key)).ok()?;
    serde_json::from_slice::<Entry>(&bytes)
        .ok()
        .filter(|entry| entry.schema == SCHEMA && entry.key == key)
}

/// Writes `entry` whole: a temporary file and a rename. The caller holds the lock.
pub fn write(dir: &Path, entry: &Entry) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{}-{}.tmp", entry.key, std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut file = crate::credentials::create_private(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(entry)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temp, path_of(dir, &entry.key))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Keeps `result` of `key` for `base`, pending, with the lock held: a newer result replaces an
/// older one still pending, and the same result (same body) is kept once.
pub fn keep(
    dir: &Path,
    key: &Digest,
    base: &str,
    credential: &str,
    result: &RunResult,
) -> io::Result<()> {
    let body = serde_json::to_vec(result)?;
    let id = Digest::of(&body).hex();
    let _lock = Lock::take(dir)?;
    if let Some(existing) = read(dir, &key.hex())
        && existing.id == id
        && existing.base == base
    {
        return Ok(());
    }
    write(
        dir,
        &Entry {
            schema: SCHEMA.to_string(),
            state: State::Pending,
            key: key.hex(),
            id,
            base: base.to_string(),
            credential: credential.to_string(),
            result: result.clone(),
            created_at: now_ms(),
            attempts: 0,
            last_error: None,
            settled_at: None,
        },
    )
}

/// Every entry that reads, in no order.
pub fn entries(dir: &Path) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read_dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let key = name.strip_suffix(".json")?.to_string();
            (!key.starts_with('.')).then(|| read(dir, &key)).flatten()
        })
        .collect()
}

/// \[C-53\] A pass or fail of `key` this machine kept for `base`, pending or sent, that counts by
/// `counts`: what a lookup answers while the server is out of reach.
pub fn local(
    dir: &Path,
    key: &Digest,
    base: &str,
    counts: &dyn Fn(&RunResult) -> bool,
) -> Option<RunResult> {
    let entry = read(dir, &key.hex())?;
    let state = decide(entry.state, entry.created_at, now_ms(), None);
    (entry.base == base && matches!(state, State::Pending | State::Sent) && counts(&entry.result))
        .then_some(entry.result)
}

/// What a flush did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Flushed {
    pub sent: usize,
    pub refused: usize,
    pub expired: usize,
    pub pending: usize,
}

/// Sends the entries pending for `base` with `credential`, each with `send`, which says what the
/// try found; settles each by [`decide`], with the lock held throughout. Entries for another server
/// or credential stay as they are. `None` when another flush holds the lock.
pub fn flush(
    dir: &Path,
    base: &str,
    credential: &str,
    send: &mut dyn FnMut(&Entry) -> (Outcome, Option<String>),
) -> io::Result<Option<Flushed>> {
    let Some(_lock) = Lock::try_take(dir)? else {
        return Ok(None);
    };
    let mut flushed = Flushed::default();
    let now = now_ms();
    // A server out of reach is out of reach for the rest: the others wait for the next flush.
    let mut down = false;
    for mut entry in entries(dir) {
        let before = entry.state;
        let ours = entry.base == base && entry.credential == credential;
        let mut state = decide(entry.state, entry.created_at, now, None);
        if state == State::Pending && ours && !down {
            let (outcome, why) = send(&entry);
            entry.attempts += 1;
            entry.last_error = why;
            state = decide(state, entry.created_at, now, Some(outcome));
            down = outcome == Outcome::Unreachable;
            if down {
                entry.state = state;
                write(dir, &entry)?;
            }
        }
        if state != before {
            entry.state = state;
            entry.settled_at = Some(now);
            write(dir, &entry)?;
        }
        match state {
            State::Sent if before != State::Sent => flushed.sent += 1,
            State::Refused if before != State::Refused => flushed.refused += 1,
            State::Expired if before != State::Expired => flushed.expired += 1,
            State::Pending if ours => flushed.pending += 1,
            _ => {}
        }
    }
    Ok(Some(flushed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> RunResult {
        serde_json::from_value(serde_json::json!({
            "schema": "hushrun.run-result/v0",
            "check": "unit",
            "result": "pass",
            "exitCode": 0,
            "command": ["true"],
            "cwd": "",
            "scope": "default",
            "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "startedAt": "2026-11-23T10:00:00.000Z",
            "durationMs": 10,
            "provider": "local",
            "tool": "hushrun 0.20.0",
        }))
        .unwrap()
    }

    // covers: C-53
    #[test]
    fn one_function_decides_every_state() {
        let day = 24 * 60 * 60 * 1000;
        use Outcome as O;
        use State as S;
        assert_eq!(decide(S::Pending, 0, day, Some(O::Accepted)), S::Sent);
        assert_eq!(decide(S::Pending, 0, day, Some(O::Refused)), S::Refused);
        assert_eq!(decide(S::Pending, 0, day, Some(O::Unreachable)), S::Pending);
        assert_eq!(decide(S::Pending, 0, day, None), S::Pending);
        assert_eq!(decide(S::Pending, 0, 7 * day, None), S::Expired);
        assert_eq!(
            decide(S::Pending, 0, 7 * day, Some(O::Unreachable)),
            S::Expired
        );
        // Settled stays settled, whatever a later try says.
        for state in [S::Sent, S::Refused, S::Expired] {
            for outcome in [
                None,
                Some(O::Accepted),
                Some(O::Refused),
                Some(O::Unreachable),
            ] {
                assert_eq!(decide(state, 0, 30 * day, outcome), state);
            }
        }
    }

    // covers: C-53
    #[test]
    fn a_flush_sends_each_entry_once_and_only_to_its_server() {
        let dir = tempfile::tempdir().unwrap();
        let key = Digest::of(b"one");
        keep(dir.path(), &key, "https://a/o/acme", "token", &result()).unwrap();
        // The same result again is the same entry.
        keep(dir.path(), &key, "https://a/o/acme", "token", &result()).unwrap();
        let other = Digest::of(b"two");
        keep(dir.path(), &other, "https://b/o/acme", "token", &result()).unwrap();
        assert_eq!(entries(dir.path()).len(), 2);

        // Another credential sends nothing.
        let mut tries = 0;
        let flushed = flush(dir.path(), "https://a/o/acme", "login", &mut |_| {
            tries += 1;
            (Outcome::Accepted, None)
        })
        .unwrap()
        .unwrap();
        assert_eq!((tries, flushed.sent), (0, 0));

        // Out of reach: it stays pending, counted.
        let flushed = flush(dir.path(), "https://a/o/acme", "token", &mut |_| {
            (Outcome::Unreachable, Some("connection refused".into()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(flushed.pending, 1);
        assert_eq!(read(dir.path(), &key.hex()).unwrap().attempts, 1);

        let mut sent = Vec::new();
        let flushed = flush(dir.path(), "https://a/o/acme", "token", &mut |entry| {
            sent.push(entry.key.clone());
            (Outcome::Accepted, None)
        })
        .unwrap()
        .unwrap();
        assert_eq!(sent, [key.hex()]);
        assert_eq!(flushed.sent, 1);
        assert_eq!(read(dir.path(), &key.hex()).unwrap().state, State::Sent);
        // Sent once: a later flush doesn't send it again; the other server's entry waits.
        let mut again = 0;
        flush(dir.path(), "https://a/o/acme", "token", &mut |_| {
            again += 1;
            (Outcome::Accepted, None)
        })
        .unwrap();
        assert_eq!(again, 0);
        assert_eq!(
            read(dir.path(), &other.hex()).unwrap().state,
            State::Pending
        );
        // A lookup out of reach answers it.
        assert!(local(dir.path(), &key, "https://a/o/acme", &|_| true).is_some());
        assert!(local(dir.path(), &key, "https://b/o/acme", &|_| true).is_none());
    }

    // covers: C-53
    #[test]
    fn a_week_old_entry_expires_and_is_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let key = Digest::of(b"old");
        keep(dir.path(), &key, "https://a/o/acme", "token", &result()).unwrap();
        let mut entry = read(dir.path(), &key.hex()).unwrap();
        entry.created_at -= KEPT_MS;
        write(dir.path(), &entry).unwrap();
        let mut tries = 0;
        let flushed = flush(dir.path(), "https://a/o/acme", "token", &mut |_| {
            tries += 1;
            (Outcome::Accepted, None)
        })
        .unwrap()
        .unwrap();
        assert_eq!((tries, flushed.expired), (0, 1));
        assert_eq!(read(dir.path(), &key.hex()).unwrap().state, State::Expired);
        assert!(local(dir.path(), &key, "https://a/o/acme", &|_| true).is_none());
    }

    // covers: C-53
    #[test]
    fn runs_keeping_results_at_once_lose_none() {
        let dir = tempfile::tempdir().unwrap();
        let threads: Vec<_> = (0..16)
            .map(|n| {
                let dir = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    let key = Digest::of(format!("{n}").as_bytes());
                    keep(&dir, &key, "https://a/o/acme", "token", &result()).unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(entries(dir.path()).len(), 16);
    }
}

// --- What `hushrun fetch` saved -----------------------------------------------------------------

/// \[ADR 0106\] How long what `hushrun fetch` saved is read: 24 hours (C-54).
pub const FETCHED_MS: i64 = 24 * 60 * 60 * 1000;

/// One result `hushrun fetch` saved, sealed by its SHA-256 over the server, the key and the result.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Fetched {
    schema: String,
    base: String,
    key: String,
    result: RunResult,
    fetched_at: i64,
    seal: String,
}

const FETCHED_SCHEMA: &str = "hushrun.fetched/v1";

fn seal(base: &str, key: &str, result: &RunResult) -> String {
    let body = serde_json::to_vec(result).unwrap_or_default();
    let mut text = format!("{FETCHED_SCHEMA}\n{base}\n{key}\n").into_bytes();
    text.extend_from_slice(&body);
    Digest::of(&text).hex()
}

/// `<cache dir>/fetched`.
pub fn fetched_directory() -> Option<PathBuf> {
    crate::context::cache_dir().map(|cache| cache.join("fetched"))
}

/// Saves `result`, the server `base`'s answer for `key`, whole.
pub fn save_fetched(dir: &Path, base: &str, key: &Digest, result: &RunResult) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let entry = Fetched {
        schema: FETCHED_SCHEMA.to_string(),
        base: base.to_string(),
        key: key.hex(),
        result: result.clone(),
        fetched_at: now_ms(),
        seal: seal(base, &key.hex(), result),
    };
    let temp = dir.join(format!(".{}-{}.tmp", key.hex(), std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut file = crate::credentials::create_private(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(&entry)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, dir.join(format!("{}.json", key.hex())))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// \[C-54\] What `hushrun fetch` saved for `key` from `base` within 24 hours, its seal whole, that
/// counts by `counts`.
pub fn fetched(
    dir: &Path,
    key: &Digest,
    base: &str,
    counts: &dyn Fn(&RunResult) -> bool,
) -> Option<RunResult> {
    let bytes = std::fs::read(dir.join(format!("{}.json", key.hex()))).ok()?;
    let entry: Fetched = serde_json::from_slice(&bytes).ok()?;
    let good = entry.schema == FETCHED_SCHEMA
        && entry.base == base
        && entry.key == key.hex()
        && entry.seal == seal(base, &entry.key, &entry.result)
        && now_ms() - entry.fetched_at < FETCHED_MS
        && entry.fetched_at <= now_ms() + 60_000;
    (good && counts(&entry.result)).then_some(entry.result)
}

#[cfg(test)]
mod fetched_tests {
    use super::*;

    // covers: C-54
    #[test]
    fn what_fetch_saved_is_read_for_its_server_for_a_day_and_whole() {
        let dir = tempfile::tempdir().unwrap();
        let key = Digest::of(b"k");
        let result: RunResult = serde_json::from_value(serde_json::json!({
            "schema": "hushrun.run-result/v0", "check": "unit", "result": "pass", "exitCode": 0,
            "command": ["true"], "cwd": "", "scope": "default",
            "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "startedAt": "2026-11-23T10:00:00.000Z", "durationMs": 10, "provider": "local",
            "tool": "hushrun 0.20.0",
        }))
        .unwrap();
        save_fetched(dir.path(), "https://a/o/acme", &key, &result).unwrap();
        assert!(fetched(dir.path(), &key, "https://a/o/acme", &|_| true).is_some());
        assert!(fetched(dir.path(), &key, "https://b/o/acme", &|_| true).is_none());
        // Edited, it doesn't read.
        let path = dir.path().join(format!("{}.json", key.hex()));
        let mut entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        entry["result"]["result"] = serde_json::json!("pass ");
        entry["result"]["durationMs"] = serde_json::json!(11);
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        assert!(fetched(dir.path(), &key, "https://a/o/acme", &|_| true).is_none());
        // A day old, it doesn't either.
        save_fetched(dir.path(), "https://a/o/acme", &key, &result).unwrap();
        let mut entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        entry["fetched_at"] = serde_json::json!(now_ms() - FETCHED_MS);
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        assert!(fetched(dir.path(), &key, "https://a/o/acme", &|_| true).is_none());
    }
}

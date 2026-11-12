//! \[ADR 0072\] This machine's store of what test files read: one SQLite file per repository,
//! `<cache dir>/units/<repository id>.v2.sqlite`, in WAL mode, opened by any number of hushrun
//! processes at once (C-36). \[ADR 0077\] Schema 2 adds each test file's tests; a `v1` file is left
//! for an older hushrun, and read by none.
//!
//! It holds the map and what explains it, and nothing that decides:
//!
//! - **the map** of each check, by its id ([`super::store::check_id`]): each test file that ran
//!   here, the files it read that its key holds, each with its kind and content hash then, what
//!   the adapter couldn't follow, the directories it listed; and the check's rest set, sources and
//!   \[ADR 0071\] Vitest's list of test files, of its last run;
//! - **the edges** adapters saw (a file imports, opens or reads another), which `hushrun why`
//!   walks to show a path, and no key ever reads;
//! - **the audit**: each test file that would have been reused and failed;
//! - \[C-38\] **hints known**: the content id of each hint this machine sent or received, by its
//!   address and system, so that the same hint isn't sent twice;
//! - \[ADR 0077\] **tests**: for a check with `tests.unit: test`, each test of a test file that ran
//!   here, by its id, with its last state and duration and what wasn't followed inside it; and, for
//!   `reads: test`, a list it read, one row per distinct list, which tests share (C-39).
//!
//! \[C-38\] A test file's row is `seen here`, what it read when it ran here, or `shared`, a list
//! another machine saw it read whose key had a pass that counted here, which reused it; the next
//! run of the test file here replaces it.
//!
//! A map is a hint: it chooses which files a key hashes, and a wrong one makes a key no run
//! recorded, which runs the test file (RFC 0003 §3.4). So a store that is missing, busy, broken or
//! of another schema reads as an empty map: every test file runs, and the run says so once when
//! the store is there and can't be read. A run that writes one it can't read replaces it.
//!
//! Every write is one transaction, `BEGIN IMMEDIATE`, after a busy wait of at most
//! [`BUSY_TIMEOUT`]: a run replaces its test files' rows, the check's sets and the edges it saw,
//! and forgets a check no run wrote for [`KEEP_DAYS`]. Readers that must write nothing
//! (`hushrun why`, `hushrun ready`, ADR 0061) open it without creating it and query only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension as _, Transaction, params};
use serde_json::json;

use hushrun_core::digest::Digest;
use hushrun_core::units::{KeyBase, NotFollowed, State};

use super::store::{Disagreement, Map, Mapped, Origin, TestListRow, TestRow};

/// The schema this hushrun reads and writes, in the file's name and its `user_version`. A
/// hushrun with another schema keeps another file.
pub const SCHEMA: i64 = 2;
/// `PRAGMA application_id`: "hush".
const APPLICATION_ID: i64 = 0x6875_7368;
/// How long a write waits for another hushrun's transaction.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// \[ADR 0067\] A check no run wrote for this long is forgotten.
pub const KEEP_DAYS: i64 = 30;
/// How many disagreements the audit keeps of a check.
const MOST_DISAGREEMENTS: i64 = 200;
/// \[C-38\] How many hints known a check keeps.
const MOST_HINTS_KNOWN: i64 = 200;

/// The tables, in a fresh store. `STRICT`: a value of another type is refused.
const TABLES: &str = "
CREATE TABLE path (
    id   INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
) STRICT;

-- A file's kind and content hash, as a source manifest writes them (D-4): one row per version
-- of a file some row needs.
CREATE TABLE version (
    id       INTEGER PRIMARY KEY,
    path_ref INTEGER NOT NULL REFERENCES path (id),
    entry    TEXT NOT NULL CHECK (entry GLOB '? [0-9a-f]*' AND length(entry) = 66),
    UNIQUE (path_ref, entry)
) STRICT;

-- One row per check id (C-33), with what it is, for `hushrun why`.
CREATE TABLE checks (
    id         INTEGER PRIMARY KEY,
    check_id   TEXT NOT NULL UNIQUE CHECK (length(check_id) = 64),
    name       TEXT NOT NULL CHECK (name <> ''),
    command    TEXT NOT NULL CHECK (json_valid(command) AND json_type(command) = 'array'),
    cwd        TEXT NOT NULL,
    scope      TEXT NOT NULL CHECK (scope <> ''),
    -- [ADR 0077] The adapter's word: 1 to 32 of a-z, 0-9 and -, so that a new adapter needs no
    -- new schema.
    adapter    TEXT NOT NULL CHECK (length(adapter) BETWEEN 1 AND 32
                                    AND adapter NOT GLOB '*[^a-z0-9-]*'),
    -- When a run last wrote it, in milliseconds since 1970, by this machine's clock.
    written_at INTEGER NOT NULL
) STRICT;

-- The check's last run: its rest set, its sources, and Vitest's list of test files.
CREATE TABLE check_rest (
    check_ref   INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    version_ref INTEGER NOT NULL REFERENCES version (id),
    PRIMARY KEY (check_ref, version_ref)
) STRICT, WITHOUT ROWID;
CREATE TABLE check_source (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (check_ref, path_ref)
) STRICT, WITHOUT ROWID;
CREATE TABLE check_listed (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (check_ref, path_ref)
) STRICT, WITHOUT ROWID;

-- Each test file of a check, as it last ran here, or [C-38] as another machine saw it read the
-- files of a pass that reused it here.
CREATE TABLE unit (
    id        INTEGER PRIMARY KEY,
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    ran_at    INTEGER NOT NULL,
    origin    TEXT NOT NULL CHECK (origin IN ('seen here', 'shared')),
    -- The system whose machine saw a shared list: one for `shared`, none for `seen here`.
    shared_os TEXT CHECK (shared_os IN ('linux', 'macos', 'windows')),
    UNIQUE (check_ref, path_ref),
    CHECK ((origin = 'seen here') = (shared_os IS NULL))
) STRICT;
-- What it read that its key holds, each file as it was then.
CREATE TABLE unit_read (
    unit_ref    INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    version_ref INTEGER NOT NULL REFERENCES version (id),
    PRIMARY KEY (unit_ref, version_ref)
) STRICT, WITHOUT ROWID;
CREATE INDEX unit_read_by_version ON unit_read (version_ref);
-- What the adapter couldn't follow while it ran, by its word: any makes it `not observed`.
CREATE TABLE unit_not_followed (
    unit_ref INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    why      TEXT NOT NULL CHECK (why <> ''),
    detail   TEXT NOT NULL,
    PRIMARY KEY (unit_ref, why, detail)
) STRICT, WITHOUT ROWID;
-- [ADR 0070] Directories it listed: a plan adds every file below each.
CREATE TABLE unit_dir (
    unit_ref INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    path_ref INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (unit_ref, path_ref)
) STRICT, WITHOUT ROWID;

-- Direct edges an adapter saw in a check's runs, for `hushrun why` to show a path. Never read
-- for a key.
CREATE TABLE edge (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    from_ref  INTEGER NOT NULL REFERENCES path (id),
    to_ref    INTEGER NOT NULL REFERENCES path (id),
    kind      TEXT NOT NULL CHECK (kind IN ('import', 'open', 'read')),
    PRIMARY KEY (check_ref, from_ref, to_ref, kind)
) STRICT, WITHOUT ROWID;
CREATE INDEX edge_to ON edge (to_ref);

-- [C-38] Each hint this machine sent or received for a check, by its address and system: one it
-- already knows isn't sent again. The newest 200 of a check.
CREATE TABLE hint_known (
    check_ref  INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    address    TEXT NOT NULL CHECK (length(address) = 64),
    os         TEXT NOT NULL CHECK (os IN ('linux', 'macos', 'windows')),
    content_id TEXT NOT NULL CHECK (length(content_id) = 64),
    how        TEXT NOT NULL CHECK (how IN ('sent', 'received')),
    at         INTEGER NOT NULL,
    PRIMARY KEY (check_ref, address, os, content_id)
) STRICT, WITHOUT ROWID;

-- [ADR 0077] What a test read with `reads: test`: one row per distinct list, which any number of
-- tests share, by the SHA-256 of its paths and their versions.
CREATE TABLE test_list (
    id     INTEGER PRIMARY KEY,
    digest TEXT NOT NULL UNIQUE CHECK (length(digest) = 64)
) STRICT;
CREATE TABLE test_list_read (
    list_ref    INTEGER NOT NULL REFERENCES test_list (id) ON DELETE CASCADE,
    version_ref INTEGER NOT NULL REFERENCES version (id),
    PRIMARY KEY (list_ref, version_ref)
) STRICT, WITHOUT ROWID;
CREATE TABLE test_list_dir (
    list_ref INTEGER NOT NULL REFERENCES test_list (id) ON DELETE CASCADE,
    path_ref INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (list_ref, path_ref)
) STRICT, WITHOUT ROWID;

-- [ADR 0077] Each test of a test file, as it last ran here (C-39): its id, its last state (a test
-- reused keeps the row of the run it stands on), its duration, and with `reads: test` the list it
-- read; none with `reads: file`, where it read what its test file read.
CREATE TABLE test (
    id          INTEGER PRIMARY KEY,
    unit_ref    INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    test_id     TEXT NOT NULL CHECK (test_id <> ''),
    state       TEXT NOT NULL CHECK (state IN ('passed', 'failed', 'not-observed', 'not-run')),
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    list_ref    INTEGER REFERENCES test_list (id),
    -- [C-37] Why a test the run decided `not-run` kept no result, as its adapter said it.
    not_kept    TEXT CHECK (not_kept IS NULL OR (state = 'not-run' AND not_kept <> '')),
    UNIQUE (unit_ref, test_id)
) STRICT;
CREATE INDEX test_by_list ON test (list_ref);
-- What the adapter couldn't follow inside the test's own window.
CREATE TABLE test_not_followed (
    test_ref INTEGER NOT NULL REFERENCES test (id) ON DELETE CASCADE,
    why      TEXT NOT NULL CHECK (why <> ''),
    detail   TEXT NOT NULL,
    PRIMARY KEY (test_ref, why, detail)
) STRICT, WITHOUT ROWID;

-- [C-34] Each test file that would have been reused and failed: the newest 200 of a check.
CREATE TABLE disagreement (
    id        INTEGER PRIMARY KEY,
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    unit      TEXT NOT NULL,
    key       TEXT NOT NULL CHECK (length(key) = 64),
    run_key   TEXT NOT NULL CHECK (length(run_key) = 64),
    at        TEXT NOT NULL
) STRICT;
";

/// Where a repository's store is: `<cache dir>/units/<id>.v2.sqlite`, the id the SHA-256 of the
/// JCS form of `{"schema": "hushrun.unit-store-id/v1", "repository"}`, the repository as the run
/// names it (C-10), lowercase, `""` for none.
pub fn path(cache: &Path, repository: Option<&str>) -> PathBuf {
    let value = json!({
        "schema": "hushrun.unit-store-id/v1",
        "repository": repository.unwrap_or_default().to_lowercase(),
    });
    let id = hushrun_core::jcs::digest(&value)
        .unwrap_or_else(|_| Digest::of(value.to_string().as_bytes()));
    cache
        .join("units")
        .join(format!("{}.v{SCHEMA}.sqlite", id.hex()))
}

/// Whether a file of the cache's `units` directory is a store of this schema, which prunes
/// itself and which the directory's sweep leaves alone.
pub fn is_store_file(name: &str) -> bool {
    let suffix = format!(".v{SCHEMA}.sqlite");
    ["", "-wal", "-shm", ".lock"]
        .iter()
        .any(|more| name.ends_with(&format!("{suffix}{more}")))
}

/// Why a store couldn't be read: what the run says, once, as `every test file runs: <why>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// Another hushrun held it past [`BUSY_TIMEOUT`].
    Busy,
    /// It isn't a store of this schema, or SQLite says it is damaged: what it said.
    Broken(String),
}

impl Unread {
    /// The words after `every test file runs: `.
    pub fn why(&self) -> String {
        match self {
            Unread::Busy => format!(
                "this machine's record of what its test files read was busy for {}s",
                BUSY_TIMEOUT.as_secs()
            ),
            Unread::Broken(why) => format!(
                "this machine's record of what its test files read can't be read ({why}), so the next run here starts it again"
            ),
        }
    }
}

fn unread(err: &rusqlite::Error) -> Unread {
    match err.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Unread::Busy,
        _ => Unread::Broken(first_line(&err.to_string())),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// A store opened to read: `Ok(None)` when there is none.
fn open_to_read(path: &Path) -> Result<Option<Connection>, Unread> {
    if !path.is_file() {
        return Ok(None);
    }
    // Read-write without creating, querying only: a read-only connection would leave WAL's files
    // behind, and this one removes them as it closes when it is the last.
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| unread(&e))?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| unread(&e))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|e| unread(&e))?;
    // One that another hushrun is making right now has neither yet: none, so far.
    if ids(&conn)? == (0, 0) {
        return Ok(None);
    }
    check_schema(&conn)?;
    Ok(Some(conn))
}

/// A database's application id and user version.
fn ids(conn: &Connection) -> Result<(i64, i64), Unread> {
    conn.query_row(
        "SELECT application_id, user_version FROM pragma_application_id, pragma_user_version",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|e| unread(&e))
}

/// Whether `conn` is a store of this schema: hushrun's application id and [`SCHEMA`], and \[C-36\]
/// its tables as this build makes them, so that a store an older build of the same schema made (a
/// test table without a column read now) reads as one that can't be read, and is set aside.
fn check_schema(conn: &Connection) -> Result<(), Unread> {
    let (app, version) = ids(conn)?;
    if app != APPLICATION_ID || version != SCHEMA {
        return Err(Unread::Broken(format!(
            "it isn't hushrun's store of schema {SCHEMA}"
        )));
    }
    if tables_of(conn).map_err(|e| unread(&e))? != *this_builds_tables() {
        return Err(Unread::Broken(
            "its tables aren't the ones this hushrun makes".into(),
        ));
    }
    Ok(())
}

/// The `CREATE TABLE` statements of a database's tables, as SQLite keeps them.
fn tables_of(conn: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT sql FROM sqlite_master WHERE type = 'table'")?;
    stmt.query_map([], |row| row.get::<_, String>(0))?.collect()
}

/// The tables [`TABLES`] makes, made once in memory.
fn this_builds_tables() -> &'static BTreeSet<String> {
    static MADE: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
    MADE.get_or_init(|| {
        Connection::open_in_memory()
            .and_then(|conn| {
                conn.execute_batch(TABLES)?;
                tables_of(&conn)
            })
            .unwrap_or_default()
    })
}

/// \[C-36\] The one place a store is written: `write` with the store opened to write, made when
/// there is none. A store that can't be opened or written for anything but "busy" (another schema,
/// damaged, a table of an older build of this schema) is set aside ([`set_aside`]) and made anew,
/// once, so the line that says the next run starts it again is true. `Err` when even that fails,
/// or another hushrun held it too long.
fn writing<T>(
    path: &Path,
    write: impl Fn(&mut Connection) -> Result<T, Unread>,
) -> Result<T, Unread> {
    let attempt = || open_writable(path).and_then(|mut conn| write(&mut conn));
    match attempt() {
        Err(Unread::Broken(why)) => {
            set_aside(path, &why);
            attempt()
        }
        other => other,
    }
}

/// \[C-36\] Sets a store that can't be read aside, `<file>.unread` beside it (one kept, the
/// newest), with its WAL's files gone, so that the next write makes it anew.
fn set_aside(path: &Path, why: &str) {
    tracing::debug!("{} is set aside and made anew: {why}", path.display());
    let mut aside = path.as_os_str().to_os_string();
    aside.push(".unread");
    if std::fs::rename(path, PathBuf::from(aside)).is_err() {
        let _ = std::fs::remove_file(path);
    }
    remove(path);
}

fn open_writable(path: &Path) -> Result<Connection, Unread> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Unread::Broken(e.to_string()))?;
    }
    let conn = Connection::open(path).map_err(|e| unread(&e))?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| unread(&e))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| unread(&e))?;
    let (app, version) = ids(&conn)?;
    if app == APPLICATION_ID && version == SCHEMA {
        check_schema(&conn)?;
        return Ok(conn);
    }
    if app != 0 || version != 0 {
        return Err(Unread::Broken(format!(
            "it isn't hushrun's store of schema {SCHEMA}"
        )));
    }
    // A new file: WAL, then the tables and the version in one transaction, so that a store with a
    // version always has its tables. One hushrun makes it, holding a lock file beside it; another
    // that waited finds it made.
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))
        .map_err(|e| Unread::Broken(e.to_string()))?;
    lock.lock().map_err(|e| Unread::Broken(e.to_string()))?;
    let (app, version) = ids(&conn)?;
    if app == APPLICATION_ID && version == SCHEMA {
        return Ok(conn);
    }
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| unread(&e))?;
    let mut conn = conn;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| unread(&e))?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| unread(&e))?;
    if version == 0 {
        tx.execute_batch(TABLES).map_err(|e| unread(&e))?;
        tx.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {SCHEMA};"
        ))
        .map_err(|e| unread(&e))?;
    }
    tx.commit().map_err(|e| unread(&e))?;
    check_schema(&conn)?;
    Ok(conn)
}

/// \[C-36\] The tables of schema 1, in an order their references allow: schema 2 holds them with
/// the same columns, beside its tests' tables, and its adapter column takes any word where schema
/// 1's took four, so every row of schema 1 is a row of schema 2.
const TABLES_OF_SCHEMA_1: &[&str] = &[
    "path",
    "version",
    "checks",
    "check_rest",
    "check_source",
    "check_listed",
    "unit",
    "unit_read",
    "unit_not_followed",
    "unit_dir",
    "edge",
    "hint_known",
    "disagreement",
];

/// \[C-36\] The store of schema 1 an older hushrun keeps beside `path`: `<id>.v1.sqlite`.
pub fn older(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(&format!(".v{SCHEMA}.sqlite"))?;
    Some(path.with_file_name(format!("{stem}.v1.sqlite")))
}

/// \[C-36\] Makes this schema's store at `path` from the store of schema 1 beside it, when there
/// is none of this schema and that one is hushrun's of schema 1: its rows are copied into a new
/// file in one transaction, whole or not at all, which takes `path` only when no other hushrun
/// made one meanwhile. The older file is copied first and left as it is, for an older hushrun.
/// The older file's name when it did; `None` when there was nothing to do, or it couldn't be done,
/// and then the run goes on as with no store. Readers that write nothing never call it.
pub fn upgrade(path: &Path) -> Option<String> {
    if path.exists() {
        return None;
    }
    let old = older(path).filter(|old| old.is_file())?;
    match import(&old, path) {
        Ok(()) => old.file_name().map(|n| n.to_string_lossy().into_owned()),
        Err(why) => {
            tracing::debug!("{} wasn't imported: {why}", old.display());
            None
        }
    }
}

fn import(old: &Path, path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("a store has a directory")?;
    let work = tempfile::Builder::new()
        .prefix(".import-")
        .tempdir_in(dir)
        .map_err(|err| err.to_string())?;
    // A copy, with its WAL, which opening it recovers: the older file is never opened.
    let copy = work.path().join("old.sqlite");
    std::fs::copy(old, &copy).map_err(|err| err.to_string())?;
    let mut wal = old.as_os_str().to_os_string();
    wal.push("-wal");
    if Path::new(&wal).is_file() {
        std::fs::copy(&wal, work.path().join("old.sqlite-wal")).map_err(|err| err.to_string())?;
    }
    let made = work.path().join("new.sqlite");
    {
        let mut conn = Connection::open(&made).map_err(|e| e.to_string())?;
        conn.execute("ATTACH DATABASE ?1 AS old", [copy.to_string_lossy()])
            .map_err(|e| e.to_string())?;
        let pragma = |name: &str| -> Result<i64, String> {
            conn.query_row(&format!("PRAGMA old.{name}"), [], |row| row.get(0))
                .map_err(|e| e.to_string())
        };
        let ids = (pragma("application_id")?, pragma("user_version")?);
        if ids != (APPLICATION_ID, 1) {
            return Err("it isn't hushrun's store of schema 1".into());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(TABLES).map_err(|e| e.to_string())?;
        for table in TABLES_OF_SCHEMA_1 {
            let columns: Vec<String> = tx
                .prepare(&format!("PRAGMA old.table_info({table})"))
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>("name"))?
                        .collect::<rusqlite::Result<Vec<String>>>()
                })
                .map_err(|e| e.to_string())?;
            if columns.is_empty() {
                return Err(format!("it has no table {table}"));
            }
            let columns = columns.join(", ");
            tx.execute_batch(&format!(
                "INSERT INTO main.{table} ({columns}) SELECT {columns} FROM old.{table};"
            ))
            .map_err(|e| e.to_string())?;
        }
        tx.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {SCHEMA};"
        ))
        .map_err(|e| e.to_string())?;
        let broken: Option<String> = tx
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(table) = broken {
            return Err(format!("a row of {table} names one that isn't there"));
        }
        tx.commit().map_err(|e| e.to_string())?;
        conn.execute_batch("DETACH DATABASE old; PRAGMA journal_mode = WAL;")
            .map_err(|e| e.to_string())?;
    }
    // Whole, closed: it takes the store's name only if none is there yet.
    std::fs::hard_link(&made, path).map_err(|err| err.to_string())
}

/// Removes a store and its WAL's files.
fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

/// \[C-32\] The adapter that last reported the check `check_id`'s test files in the store at
/// `path`, by its word: `None` when the store doesn't hold the check, or can't be read. With no
/// store of this schema, the one of schema 1 an older hushrun keeps beside it, read only and never
/// imported: `hushrun ready` and `hushrun why`, which write nothing, choose as the run, which
/// imports first, does.
pub fn adapter_of(path: &Path, check_id: &Digest) -> Option<String> {
    if !path.exists()
        && let Some(old) = older(path).filter(|old| old.is_file())
    {
        let conn = Connection::open_with_flags(
            &old,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        if ids(&conn).ok()? != (APPLICATION_ID, 1) {
            return None;
        }
        return conn
            .query_row(
                "SELECT adapter FROM checks WHERE check_id = ?1",
                [check_id.hex()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()?;
    }
    let conn = open_to_read(path).ok()??;
    conn.query_row(
        "SELECT adapter FROM checks WHERE check_id = ?1",
        [check_id.hex()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()?
}

/// What reading a check's map came to: the map, empty when there is none; and why the store
/// couldn't be read, when it is there and couldn't be.
#[derive(Debug, Default)]
pub struct Loaded {
    pub map: Map,
    pub unread: Option<Unread>,
}

/// The map of the check `check_id` in the store at `path`.
pub fn load(path: &Path, check_id: &Digest) -> Loaded {
    let read = || -> Result<Map, Unread> {
        let Some(conn) = open_to_read(path)? else {
            return Ok(Map::default());
        };
        read_map(&conn, check_id).map_err(|e| unread(&e))
    };
    match read() {
        Ok(map) => Loaded { map, unread: None },
        Err(why) => Loaded {
            map: Map::default(),
            unread: Some(why),
        },
    }
}

fn read_map(conn: &Connection, check_id: &Digest) -> rusqlite::Result<Map> {
    let Some(check) = conn
        .query_row(
            "SELECT id FROM checks WHERE check_id = ?1",
            [check_id.hex()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(Map::default());
    };
    let mut map = Map {
        reported: true,
        ..Map::default()
    };
    let mut names: HashMap<i64, String> = HashMap::new();
    let mut units = conn.prepare(
        "SELECT u.id, p.path, u.origin, u.shared_os FROM unit u JOIN path p ON p.id = u.path_ref
         WHERE u.check_ref = ?1",
    )?;
    for row in units.query_map([check], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })? {
        let (id, name, origin, os): (i64, String, String, Option<String>) = row?;
        let mapped = Mapped {
            origin: Origin::from_row(&origin, os.as_deref()).unwrap_or_default(),
            ..Mapped::default()
        };
        map.units.insert(name.clone(), mapped);
        names.insert(id, name);
    }
    let mut reads = conn.prepare(
        "SELECT r.unit_ref, p.path, v.entry FROM unit_read r
         JOIN unit u ON u.id = r.unit_ref
         JOIN version v ON v.id = r.version_ref
         JOIN path p ON p.id = v.path_ref
         WHERE u.check_ref = ?1",
    )?;
    for row in reads.query_map([check], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))? {
        let (unit, path, entry): (i64, String, String) = row?;
        if let Some(mapped) = names.get(&unit).and_then(|n| map.units.get_mut(n)) {
            mapped.reads.insert(path, entry);
        }
    }
    let mut not_followed = conn.prepare(
        "SELECT n.unit_ref, n.why, n.detail FROM unit_not_followed n
         JOIN unit u ON u.id = n.unit_ref WHERE u.check_ref = ?1",
    )?;
    for row in not_followed.query_map([check], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))? {
        let (unit, why, detail): (i64, String, String) = row?;
        if let Some(mapped) = names.get(&unit).and_then(|n| map.units.get_mut(n)) {
            mapped.not_followed.insert(NotFollowed { why, detail });
        }
    }
    let mut dirs = conn.prepare(
        "SELECT d.unit_ref, p.path FROM unit_dir d JOIN unit u ON u.id = d.unit_ref
         JOIN path p ON p.id = d.path_ref WHERE u.check_ref = ?1",
    )?;
    for row in dirs.query_map([check], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (unit, dir): (i64, String) = row?;
        if let Some(mapped) = names.get(&unit).and_then(|n| map.units.get_mut(n)) {
            mapped.dirs.insert(dir);
        }
    }
    // \[ADR 0077\] Each test file's tests, as they last ran here.
    let mut tests = conn.prepare(
        "SELECT t.id, t.unit_ref, t.test_id, t.state, t.duration_ms, t.list_ref, t.not_kept
         FROM test t JOIN unit u ON u.id = t.unit_ref WHERE u.check_ref = ?1",
    )?;
    let mut test_names: HashMap<i64, (i64, String)> = HashMap::new();
    let mut lists: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
    for row in tests.query_map([check], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    })? {
        #[allow(clippy::type_complexity, reason = "one row's columns")]
        let (id, unit, test, state, duration, list, not_kept): (
            i64,
            i64,
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
        ) = row?;
        if let Some(list) = list {
            lists.entry(list).or_default().push((unit, test.clone()));
        }
        let Some(state) = TestRow::state_from_word(&state) else {
            continue;
        };
        if let Some(mapped) = names.get(&unit).and_then(|n| map.units.get_mut(n)) {
            mapped.tests.insert(
                test.clone(),
                TestRow {
                    state,
                    duration_ms: u64::try_from(duration).unwrap_or_default(),
                    not_followed: BTreeSet::new(),
                    not_kept: not_kept.filter(|_| state == State::NotRun),
                    list: None,
                },
            );
            test_names.insert(id, (unit, test));
        }
    }
    let mut test_not_followed = conn.prepare(
        "SELECT n.test_ref, n.why, n.detail FROM test_not_followed n
         JOIN test t ON t.id = n.test_ref JOIN unit u ON u.id = t.unit_ref
         WHERE u.check_ref = ?1",
    )?;
    for row in
        test_not_followed.query_map([check], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
    {
        let (test, why, detail): (i64, String, String) = row?;
        if let Some((unit, id)) = test_names.get(&test)
            && let Some(row) = names
                .get(unit)
                .and_then(|n| map.units.get_mut(n))
                .and_then(|m| m.tests.get_mut(id))
        {
            row.not_followed.insert(NotFollowed { why, detail });
        }
    }
    // \[ADR 0078\] Each test's own list, a set tests share.
    let mut found: HashMap<i64, TestListRow> = HashMap::new();
    let mut list_reads = conn.prepare(
        "SELECT DISTINCT r.list_ref, p.path, v.entry FROM test_list_read r
         JOIN test t ON t.list_ref = r.list_ref JOIN unit u ON u.id = t.unit_ref
         JOIN version v ON v.id = r.version_ref JOIN path p ON p.id = v.path_ref
         WHERE u.check_ref = ?1",
    )?;
    for row in list_reads.query_map([check], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))? {
        let (list, path, entry): (i64, String, String) = row?;
        found.entry(list).or_default().reads.insert(path, entry);
    }
    let mut list_dirs = conn.prepare(
        "SELECT DISTINCT d.list_ref, p.path FROM test_list_dir d
         JOIN test t ON t.list_ref = d.list_ref JOIN unit u ON u.id = t.unit_ref
         JOIN path p ON p.id = d.path_ref WHERE u.check_ref = ?1",
    )?;
    for row in list_dirs.query_map([check], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (list, dir): (i64, String) = row?;
        found.entry(list).or_default().dirs.insert(dir);
    }
    for (list, tests) in lists {
        let row = found.remove(&list).unwrap_or_default();
        for (unit, test) in tests {
            if let Some(t) = names
                .get(&unit)
                .and_then(|n| map.units.get_mut(n))
                .and_then(|m| m.tests.get_mut(&test))
            {
                t.list = Some(row.clone());
            }
        }
    }
    let mut rest = conn.prepare(
        "SELECT p.path, v.entry FROM check_rest c JOIN version v ON v.id = c.version_ref
         JOIN path p ON p.id = v.path_ref WHERE c.check_ref = ?1",
    )?;
    for row in rest.query_map([check], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (path, entry): (String, String) = row?;
        map.rest.insert(path, entry);
    }
    for (table, into) in [
        ("check_source", &mut map.sources),
        ("check_listed", &mut map.unit_files),
    ] {
        let mut paths = conn.prepare(&format!(
            "SELECT p.path FROM {table} c JOIN path p ON p.id = c.path_ref WHERE c.check_ref = ?1"
        ))?;
        for row in paths.query_map([check], |row| row.get::<_, String>(0))? {
            into.insert(row?);
        }
    }
    Ok(map)
}

/// What an edge between two files is: one imports, opens or reads the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// It imported the other (pytest, Vitest), or its package imports the other's (Go).
    Import,
    /// It opened the other while a test file ran.
    Open,
    /// A test file read the other while it ran, through no edge of its own: its collection, a
    /// `conftest.py` above it, a module code outside the repository loaded.
    Read,
}

impl EdgeKind {
    pub fn word(self) -> &'static str {
        match self {
            EdgeKind::Import => "import",
            EdgeKind::Open => "open",
            EdgeKind::Read => "read",
        }
    }

    pub fn from_word(word: &str) -> Option<EdgeKind> {
        [EdgeKind::Import, EdgeKind::Open, EdgeKind::Read]
            .into_iter()
            .find(|k| k.word() == word)
    }
}

/// One edge: `from` imports, opens or reads `to`, both from the repository's root.
pub type Edge = (String, String, EdgeKind);

/// What a run that recorded writes into the store, for its check (C-33).
pub struct Write<'a> {
    pub check_id: Digest,
    pub base: &'a KeyBase,
    /// \[ADR 0077\] The adapter's word ([`super::adapter::Adapter::word`]).
    pub adapter: &'static str,
    /// Each test file that ran, with what the map keeps of it.
    pub seen: &'a BTreeMap<String, Mapped>,
    /// The scope's files now: a test file the map has whose file is gone is forgotten.
    pub tree: &'a hushrun_core::units::Tree,
    pub rest: &'a BTreeMap<String, String>,
    pub sources: &'a BTreeSet<String>,
    /// \[ADR 0071\] Vitest's list of test files, when this run's adapter gave one.
    pub unit_files: Option<&'a BTreeSet<String>>,
    /// The edges this run saw.
    pub edges: &'a BTreeSet<Edge>,
    pub disagreements: &'a [Disagreement],
    /// \[C-38\] The hints this run received.
    pub hints: &'a [HintKnown],
    /// This run's clock, in milliseconds since 1970.
    pub now_ms: i64,
}

/// \[C-38\] A hint this machine sent or received, where it is: its content id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintKnown {
    pub address: Digest,
    pub os: hushrun_core::hints::Os,
    pub content_id: Digest,
    pub how: HintHow,
}

/// \[C-38\] How this machine knows a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintHow {
    Sent,
    Received,
}

impl HintHow {
    fn word(self) -> &'static str {
        match self {
            HintHow::Sent => "sent",
            HintHow::Received => "received",
        }
    }
}

/// Writes what a run learned into the store at `path`, in one transaction: each test file it saw
/// replaces its rows, the others stay while their file is in the scope; the check's sets are this
/// run's; the edges from every file a test file that ran read are this run's; its disagreements
/// join the audit; and a check no run wrote for [`KEEP_DAYS`] goes. `Err` when nothing was
/// written, and why.
pub fn write(path: &Path, w: &Write<'_>) -> Result<(), Unread> {
    write_waiting(path, w, BUSY_TIMEOUT)
}

fn write_waiting(path: &Path, w: &Write<'_>, busy: Duration) -> Result<(), Unread> {
    writing(path, |conn| {
        conn.busy_timeout(busy).map_err(|e| unread(&e))?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| unread(&e))?;
        write_in(&tx, w).map_err(|e| unread(&e))?;
        tx.commit().map_err(|e| unread(&e))?;
        checkpoint(conn);
        Ok(())
    })?;
    Ok(())
}

/// Moves what a write committed into the one file, when no reader holds the WAL: what a cache of
/// the file alone keeps (CI's, C-15, RFC 0003 §3.11). Every write ends with it, so a run that ends
/// leaves the store whole in `<id>.v2.sqlite`. One that can't now is done by a later write, or the
/// last connection's close.
fn checkpoint(conn: &Connection) {
    if let Err(err) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
        tracing::debug!("the store's WAL wasn't checkpointed: {err}");
    }
}

/// Path and version ids, made as needed within one transaction.
struct Ids<'t> {
    tx: &'t Transaction<'t>,
    paths: HashMap<String, i64>,
}

impl Ids<'_> {
    fn path(&mut self, path: &str) -> rusqlite::Result<i64> {
        if let Some(id) = self.paths.get(path) {
            return Ok(*id);
        }
        let id = match self
            .tx
            .query_row("SELECT id FROM path WHERE path = ?1", [path], |r| r.get(0))
            .optional()?
        {
            Some(id) => id,
            None => {
                self.tx
                    .execute("INSERT INTO path (path) VALUES (?1)", [path])?;
                self.tx.last_insert_rowid()
            }
        };
        self.paths.insert(path.to_string(), id);
        Ok(id)
    }

    /// \[ADR 0078\] The row of a test's list, made with its reads and directories when no test
    /// has it yet.
    fn list(&mut self, list: &TestListRow) -> rusqlite::Result<i64> {
        let digest = list.digest().hex();
        if let Some(id) = self
            .tx
            .query_row(
                "SELECT id FROM test_list WHERE digest = ?1",
                [&digest],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.tx
            .execute("INSERT INTO test_list (digest) VALUES (?1)", [&digest])?;
        let id = self.tx.last_insert_rowid();
        for (path, entry) in &list.reads {
            let version = self.version(path, entry)?;
            self.tx.execute(
                "INSERT OR IGNORE INTO test_list_read (list_ref, version_ref) VALUES (?1, ?2)",
                params![id, version],
            )?;
        }
        for dir in &list.dirs {
            let dir = self.path(dir)?;
            self.tx.execute(
                "INSERT OR IGNORE INTO test_list_dir (list_ref, path_ref) VALUES (?1, ?2)",
                params![id, dir],
            )?;
        }
        Ok(id)
    }

    fn version(&mut self, path: &str, entry: &str) -> rusqlite::Result<i64> {
        let path_ref = self.path(path)?;
        if let Some(id) = self
            .tx
            .query_row(
                "SELECT id FROM version WHERE path_ref = ?1 AND entry = ?2",
                params![path_ref, entry],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.tx.execute(
            "INSERT INTO version (path_ref, entry) VALUES (?1, ?2)",
            params![path_ref, entry],
        )?;
        Ok(self.tx.last_insert_rowid())
    }
}

fn write_in(tx: &Transaction<'_>, w: &Write<'_>) -> rusqlite::Result<()> {
    let cutoff = w.now_ms - KEEP_DAYS * 86_400_000;
    let pruned = tx.execute("DELETE FROM checks WHERE written_at < ?1", [cutoff])?;
    let command = serde_json::to_string(&w.base.command).unwrap_or_else(|_| "[]".into());
    let check: i64 = tx.query_row(
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (check_id) DO UPDATE SET name = excluded.name, command = excluded.command,
             cwd = excluded.cwd, scope = excluded.scope, adapter = excluded.adapter,
             written_at = excluded.written_at
         RETURNING id",
        params![
            w.check_id.hex(),
            w.base.check.as_str(),
            command,
            w.base.cwd,
            w.base.scope.as_str(),
            w.adapter,
            w.now_ms,
        ],
        |row| row.get(0),
    )?;
    let mut ids = Ids {
        tx,
        paths: HashMap::new(),
    };
    // Test files whose file left the scope go; those this run saw are made again.
    let known: Vec<(i64, String)> = {
        let mut units = tx.prepare(
            "SELECT u.id, p.path FROM unit u JOIN path p ON p.id = u.path_ref
             WHERE u.check_ref = ?1",
        )?;
        units
            .query_map([check], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, name) in &known {
        if !still_there(w.tree, name) || w.seen.contains_key(name) {
            tx.execute("DELETE FROM unit WHERE id = ?1", [id])?;
        }
    }
    for (name, mapped) in w.seen {
        let path_ref = ids.path(name)?;
        tx.execute(
            "INSERT INTO unit (check_ref, path_ref, ran_at, origin, shared_os)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                check,
                path_ref,
                w.now_ms,
                mapped.origin.word(),
                mapped.origin.system().map(|os| os.word())
            ],
        )?;
        let unit = tx.last_insert_rowid();
        for (read, entry) in &mapped.reads {
            let version = ids.version(read, entry)?;
            tx.execute(
                "INSERT OR IGNORE INTO unit_read (unit_ref, version_ref) VALUES (?1, ?2)",
                params![unit, version],
            )?;
        }
        for n in &mapped.not_followed {
            tx.execute(
                "INSERT OR IGNORE INTO unit_not_followed (unit_ref, why, detail)
                 VALUES (?1, ?2, ?3)",
                params![unit, n.why, n.detail],
            )?;
        }
        for dir in &mapped.dirs {
            let dir = ids.path(dir)?;
            tx.execute(
                "INSERT OR IGNORE INTO unit_dir (unit_ref, path_ref) VALUES (?1, ?2)",
                params![unit, dir],
            )?;
        }
        // \[ADR 0077\] Its tests; \[ADR 0078\] with `reads: test`, each with its own list, a row
        // tests with the same list share.
        for (id, row) in &mapped.tests {
            let list = match &row.list {
                Some(list) => Some(ids.list(list)?),
                None => None,
            };
            tx.execute(
                "INSERT INTO test (unit_ref, test_id, state, duration_ms, list_ref, not_kept)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    unit,
                    id,
                    row.state.word(),
                    i64::try_from(row.duration_ms).unwrap_or(i64::MAX),
                    list,
                    row.not_kept
                        .as_deref()
                        .filter(|why| row.state == State::NotRun && !why.is_empty())
                ],
            )?;
            let test = tx.last_insert_rowid();
            for n in &row.not_followed {
                tx.execute(
                    "INSERT OR IGNORE INTO test_not_followed (test_ref, why, detail)
                     VALUES (?1, ?2, ?3)",
                    params![test, n.why, n.detail],
                )?;
            }
        }
    }
    tx.execute("DELETE FROM check_rest WHERE check_ref = ?1", [check])?;
    for (path, entry) in w.rest {
        let version = ids.version(path, entry)?;
        tx.execute(
            "INSERT INTO check_rest (check_ref, version_ref) VALUES (?1, ?2)",
            params![check, version],
        )?;
    }
    tx.execute("DELETE FROM check_source WHERE check_ref = ?1", [check])?;
    for path in w.sources {
        let path = ids.path(path)?;
        tx.execute(
            "INSERT INTO check_source (check_ref, path_ref) VALUES (?1, ?2)",
            params![check, path],
        )?;
    }
    if let Some(files) = w.unit_files {
        tx.execute("DELETE FROM check_listed WHERE check_ref = ?1", [check])?;
        for path in files {
            let path = ids.path(path)?;
            tx.execute(
                "INSERT INTO check_listed (check_ref, path_ref) VALUES (?1, ?2)",
                params![check, path],
            )?;
        }
    }
    // The edges from every file a test file that ran read, the test files included, are this
    // run's: a module's imports are seen whole when it loads. The others stay as they were seen.
    let mut loaded: BTreeSet<&str> = BTreeSet::new();
    for (name, mapped) in w.seen {
        loaded.insert(name);
        loaded.extend(mapped.reads.keys().map(String::as_str));
        loaded.extend(mapped.dirs.iter().map(String::as_str));
    }
    loaded.extend(w.edges.iter().map(|(from, _, _)| from.as_str()));
    for from in loaded {
        let from = ids.path(from)?;
        tx.execute(
            "DELETE FROM edge WHERE check_ref = ?1 AND from_ref = ?2",
            params![check, from],
        )?;
    }
    for (from, to, kind) in w.edges {
        let from = ids.path(from)?;
        let to = ids.path(to)?;
        tx.execute(
            "INSERT OR IGNORE INTO edge (check_ref, from_ref, to_ref, kind) VALUES (?1, ?2, ?3, ?4)",
            params![check, from, to, kind.word()],
        )?;
    }
    for d in w.disagreements {
        tx.execute(
            "INSERT INTO disagreement (check_ref, unit, key, run_key, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![check, d.unit, d.key.hex(), d.run_key.hex(), d.at.to_string()],
        )?;
    }
    if !w.disagreements.is_empty() {
        tx.execute(
            "DELETE FROM disagreement WHERE check_ref = ?1 AND id NOT IN
                 (SELECT id FROM disagreement WHERE check_ref = ?1 ORDER BY id DESC LIMIT ?2)",
            params![check, MOST_DISAGREEMENTS],
        )?;
    }
    note_in(tx, check, w.hints, w.now_ms)?;
    // What no row needs any more: versions of files that changed, and the paths of files gone.
    let _ = pruned;
    tx.execute_batch(
        "DELETE FROM test_list WHERE NOT EXISTS (SELECT 1 FROM test t WHERE t.list_ref = test_list.id);
         DELETE FROM version WHERE NOT EXISTS (SELECT 1 FROM unit_read r WHERE r.version_ref = version.id)
             AND NOT EXISTS (SELECT 1 FROM check_rest c WHERE c.version_ref = version.id)
             AND NOT EXISTS (SELECT 1 FROM test_list_read t WHERE t.version_ref = version.id);
         DELETE FROM path WHERE NOT EXISTS (SELECT 1 FROM version v WHERE v.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM test_list_dir t WHERE t.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM unit u WHERE u.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM unit_dir d WHERE d.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM check_source c WHERE c.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM check_listed c WHERE c.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.from_ref = path.id OR e.to_ref = path.id);",
    )?;
    Ok(())
}

/// \[C-38\] Notes each hint of `hints` as known for the check `check`, and keeps its newest
/// [`MOST_HINTS_KNOWN`].
fn note_in(
    tx: &Transaction<'_>,
    check: i64,
    hints: &[HintKnown],
    now_ms: i64,
) -> rusqlite::Result<()> {
    for hint in hints {
        tx.execute(
            "INSERT INTO hint_known (check_ref, address, os, content_id, how, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (check_ref, address, os, content_id) DO UPDATE SET at = excluded.at",
            params![
                check,
                hint.address.hex(),
                hint.os.word(),
                hint.content_id.hex(),
                hint.how.word(),
                now_ms
            ],
        )?;
    }
    if !hints.is_empty() {
        tx.execute(
            "DELETE FROM hint_known WHERE check_ref = ?1 AND content_id NOT IN
                 (SELECT content_id FROM hint_known WHERE check_ref = ?1 ORDER BY at DESC LIMIT ?2)",
            params![check, MOST_HINTS_KNOWN],
        )?;
    }
    Ok(())
}

/// \[C-32\] Forgets the check `check_id`: its row, with the adapter word it names, and its map,
/// in one transaction. Nothing when the store doesn't hold it.
pub fn forget_check(path: &Path, check_id: &Digest) -> Result<(), Unread> {
    if !path.is_file() {
        return Ok(());
    }
    writing(path, |conn| {
        conn.execute("PRAGMA foreign_keys = ON", [])
            .map_err(|e| unread(&e))?;
        conn.execute("DELETE FROM checks WHERE check_id = ?1", [check_id.hex()])
            .map_err(|e| unread(&e))?;
        checkpoint(conn);
        Ok(())
    })
}

/// \[C-38\] Notes `hints` as known for the check `check_id`, in one transaction: a hint sent.
/// Nothing when the store doesn't hold the check, whose run wrote nothing.
pub fn note_hints(
    path: &Path,
    check_id: &Digest,
    hints: &[HintKnown],
    now_ms: i64,
) -> Result<(), Unread> {
    writing(path, |conn| {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| unread(&e))?;
        let check: Option<i64> = tx
            .query_row(
                "SELECT id FROM checks WHERE check_id = ?1",
                [check_id.hex()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| unread(&e))?;
        if let Some(check) = check {
            note_in(&tx, check, hints, now_ms).map_err(|e| unread(&e))?;
        }
        tx.commit().map_err(|e| unread(&e))?;
        checkpoint(conn);
        Ok(())
    })
}

/// \[C-38\] Whether this machine sent or received the hint `content_id` at `address` for `os`, for
/// the check `check_id`. A store that can't be read knows none.
pub fn hint_known(
    path: &Path,
    check_id: &Digest,
    address: &Digest,
    os: hushrun_core::hints::Os,
    content_id: &Digest,
) -> bool {
    let Ok(Some(conn)) = open_to_read(path) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM hint_known h JOIN checks c ON c.id = h.check_ref
         WHERE c.check_id = ?1 AND h.address = ?2 AND h.os = ?3 AND h.content_id = ?4",
        params![check_id.hex(), address.hex(), os.word(), content_id.hex()],
        |_| Ok(()),
    )
    .optional()
    .is_ok_and(|found| found.is_some())
}

/// Whether a test file the map has is still in the scope: its file, or \[ADR 0070\] for a Go
/// package, named by its directory, some file below it (`.` the root's).
fn still_there(tree: &hushrun_core::units::Tree, unit: &str) -> bool {
    if tree.contains(unit) {
        return true;
    }
    if unit == "." {
        return tree.paths().next().is_some();
    }
    let prefix = format!("{unit}/");
    tree.paths().any(|path| path.starts_with(&prefix))
}

/// A check the store holds, as `hushrun why` finds it by name: what its key fields were.
#[derive(Debug, Clone)]
pub struct CheckRow {
    pub command: Vec<String>,
    pub cwd: String,
    pub scope: String,
}

/// The checks named `name` in the store at `path`, the newest written first: `Ok` empty when
/// there is no store or none.
pub fn checks_named(path: &Path, name: &str) -> Result<Vec<CheckRow>, Unread> {
    let Some(conn) = open_to_read(path)? else {
        return Ok(Vec::new());
    };
    let rows = (|| -> rusqlite::Result<Vec<CheckRow>> {
        let mut query = conn.prepare(
            "SELECT command, cwd, scope FROM checks
             WHERE name = ?1 ORDER BY written_at DESC, id DESC",
        )?;
        let rows = query.query_map([name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (command, cwd, scope) = row?;
            let Ok(command) = serde_json::from_str::<Vec<String>>(&command) else {
                continue;
            };
            out.push(CheckRow {
                command,
                cwd,
                scope,
            });
        }
        Ok(out)
    })();
    rows.map_err(|e| unread(&e))
}

/// The edges the store holds for the check `check_id`.
pub fn edges(path: &Path, check_id: &Digest) -> Result<BTreeSet<Edge>, Unread> {
    let Some(conn) = open_to_read(path)? else {
        return Ok(BTreeSet::new());
    };
    let rows = (|| -> rusqlite::Result<BTreeSet<Edge>> {
        let mut query = conn.prepare(
            "SELECT f.path, t.path, e.kind FROM edge e
             JOIN checks c ON c.id = e.check_ref
             JOIN path f ON f.id = e.from_ref JOIN path t ON t.id = e.to_ref
             WHERE c.check_id = ?1",
        )?;
        let rows = query.query_map([check_id.hex()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = BTreeSet::new();
        for row in rows {
            let (from, to, kind) = row?;
            if let Some(kind) = EdgeKind::from_word(&kind) {
                out.insert((from, to, kind));
            }
        }
        Ok(out)
    })();
    rows.map_err(|e| unread(&e))
}

/// The disagreements the audit holds of the check `check_id`, oldest first.
#[cfg(test)]
pub fn disagreements(path: &Path, check_id: &Digest) -> Result<Vec<Disagreement>, Unread> {
    let Some(conn) = open_to_read(path)? else {
        return Ok(Vec::new());
    };
    let rows = (|| -> rusqlite::Result<Vec<Disagreement>> {
        let mut query = conn.prepare(
            "SELECT d.unit, d.key, d.run_key, d.at FROM disagreement d
             JOIN checks c ON c.id = d.check_ref WHERE c.check_id = ?1 ORDER BY d.id",
        )?;
        let rows = query.query_map([check_id.hex()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (unit, key, run_key, at) = row?;
            let (Ok(key), Ok(run_key), Ok(at)) = (
                Digest::from_hex(&key),
                Digest::from_hex(&run_key),
                hushrun_core::time::Time::parse(&at),
            ) else {
                continue;
            };
            out.push(Disagreement {
                unit,
                key,
                run_key,
                at,
            });
        }
        Ok(out)
    })();
    rows.map_err(|e| unread(&e))
}

#[cfg(test)]
mod tests;

//! \[ADR 0072\] This machine's store of what test files read: one SQLite file per repository,
//! `<cache dir>/units/<repository id>.v1.sqlite`, in WAL mode, opened by any number of hushrun
//! processes at once (C-36).
//!
//! It holds the map and what explains it, and nothing that decides:
//!
//! - **the map** of each check, by its id ([`super::store::check_id`]): each test file that ran
//!   here, the files it read that its key holds, each with its kind and content hash then, what
//!   the adapter couldn't follow, the directories it listed; and the check's rest set, sources and
//!   \[ADR 0071\] Vitest's list of test files, of its last run;
//! - **the edges** adapters saw (a file imports, opens or reads another), which `hushrun why`
//!   walks to show a path, and no key ever reads;
//! - **the audit**: each test file that would have been reused and failed.
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
use hushrun_core::units::{KeyBase, NotFollowed};

use super::store::{Disagreement, Map, Mapped};

/// The schema this hushrun reads and writes, in the file's name and its `user_version`. A
/// hushrun with another schema keeps another file.
pub const SCHEMA: i64 = 1;
/// `PRAGMA application_id`: "hush".
const APPLICATION_ID: i64 = 0x6875_7368;
/// How long a write waits for another hushrun's transaction.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// \[ADR 0067\] A check no run wrote for this long is forgotten.
pub const KEEP_DAYS: i64 = 30;
/// How many disagreements the audit keeps of a check.
const MOST_DISAGREEMENTS: i64 = 200;

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
    adapter    TEXT NOT NULL CHECK (adapter IN ('pytest', 'cargo', 'go', 'vitest')),
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

-- Each test file of a check, as it last ran here.
CREATE TABLE unit (
    id        INTEGER PRIMARY KEY,
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    ran_at    INTEGER NOT NULL,
    UNIQUE (check_ref, path_ref)
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

/// Where a repository's store is: `<cache dir>/units/<id>.v1.sqlite`, the id the SHA-256 of the
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

/// Whether `conn` is a store of this schema: hushrun's application id and [`SCHEMA`].
fn check_schema(conn: &Connection) -> Result<(), Unread> {
    let (app, version) = ids(conn)?;
    if app != APPLICATION_ID || version != SCHEMA {
        return Err(Unread::Broken(format!(
            "it isn't hushrun's store of schema {SCHEMA}"
        )));
    }
    Ok(())
}

/// A store opened to write, made when there is none, and made again when the one there can't be
/// read: `Err` when even that fails, or another hushrun held it too long.
fn open_to_write(path: &Path) -> Result<Connection, Unread> {
    match open_writable(path) {
        Err(Unread::Broken(why)) => {
            tracing::debug!("{} is replaced: {why}", path.display());
            remove(path);
            open_writable(path)
        }
        other => other,
    }
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

/// Removes a store and its WAL's files.
fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
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
        "SELECT u.id, p.path FROM unit u JOIN path p ON p.id = u.path_ref WHERE u.check_ref = ?1",
    )?;
    for row in units.query_map([check], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (id, name): (i64, String) = row?;
        map.units.insert(name.clone(), Mapped::default());
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

/// The adapter a check's test files are observed with, as the store names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterWord {
    Pytest,
    Cargo,
    Go,
    Vitest,
}

impl AdapterWord {
    pub fn word(self) -> &'static str {
        match self {
            AdapterWord::Pytest => "pytest",
            AdapterWord::Cargo => "cargo",
            AdapterWord::Go => "go",
            AdapterWord::Vitest => "vitest",
        }
    }
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
    pub adapter: AdapterWord,
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
    /// This run's clock, in milliseconds since 1970.
    pub now_ms: i64,
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
    let mut conn = open_to_write(path)?;
    conn.busy_timeout(busy).map_err(|e| unread(&e))?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| unread(&e))?;
    write_in(&tx, w).map_err(|e| unread(&e))?;
    tx.commit().map_err(|e| unread(&e))
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
            w.adapter.word(),
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
            "INSERT INTO unit (check_ref, path_ref, ran_at) VALUES (?1, ?2, ?3)",
            params![check, path_ref, w.now_ms],
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
    // What no row needs any more: versions of files that changed, and the paths of files gone.
    let _ = pruned;
    tx.execute_batch(
        "DELETE FROM version WHERE NOT EXISTS (SELECT 1 FROM unit_read r WHERE r.version_ref = version.id)
             AND NOT EXISTS (SELECT 1 FROM check_rest c WHERE c.version_ref = version.id);
         DELETE FROM path WHERE NOT EXISTS (SELECT 1 FROM version v WHERE v.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM unit u WHERE u.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM unit_dir d WHERE d.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM check_source c WHERE c.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM check_listed c WHERE c.path_ref = path.id)
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.from_ref = path.id OR e.to_ref = path.id);",
    )?;
    Ok(())
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

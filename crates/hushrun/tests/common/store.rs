//! \[ADR 0072\] What the tests of test files read of this machine's store, as hushrun left it: a
//! check's map, as JSON, in the shape the map files had before it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};

/// The stores in `cache`, one per repository.
#[allow(dead_code)]
pub fn stores(cache: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(cache.join("units"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".v2.sqlite"))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The map of the one check the one store in `cache` holds: `units` (each with `reads`, and
/// `notFollowed` and `dirs` when it has any), `rest`, `sources` and `unitFiles`.
#[allow(dead_code)]
pub fn map(cache: &Path) -> Value {
    let stores = stores(cache);
    assert_eq!(stores.len(), 1, "one store in {}", cache.display());
    let conn = Connection::open_with_flags(&stores[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("the store opens");
    let checks: i64 = conn
        .query_row("SELECT count(*) FROM checks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(checks, 1, "one check in the store");
    let rows = |sql: &str| -> Vec<(String, String)> {
        let mut query = conn.prepare(sql).unwrap();
        query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let mut units = Map::new();
    for (unit, _) in
        rows("SELECT p.path, '' FROM unit u JOIN path p ON p.id = u.path_ref ORDER BY p.path")
    {
        units.insert(unit, json!({"reads": []}));
    }
    for (unit, read) in rows(
        "SELECT up.path, rp.path FROM unit_read r JOIN unit u ON u.id = r.unit_ref
         JOIN path up ON up.id = u.path_ref JOIN version v ON v.id = r.version_ref
         JOIN path rp ON rp.id = v.path_ref ORDER BY rp.path",
    ) {
        units[&unit]["reads"]
            .as_array_mut()
            .unwrap()
            .push(json!(read));
    }
    for (unit, why) in rows(
        "SELECT DISTINCT p.path, n.why FROM unit_not_followed n JOIN unit u ON u.id = n.unit_ref
         JOIN path p ON p.id = u.path_ref ORDER BY n.why",
    ) {
        let entry = units[&unit].as_object_mut().unwrap();
        entry
            .entry("notFollowed")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!(why));
    }
    for (unit, dir) in rows(
        "SELECT up.path, dp.path FROM unit_dir d JOIN unit u ON u.id = d.unit_ref
         JOIN path up ON up.id = u.path_ref JOIN path dp ON dp.id = d.path_ref ORDER BY dp.path",
    ) {
        let entry = units[&unit].as_object_mut().unwrap();
        entry
            .entry("dirs")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!(dir));
    }
    // \[ADR 0077\] Each test of a test file, by id, with its last state, when it has any.
    let mut tests = conn
        .prepare(
            "SELECT p.path, t.test_id, t.state FROM test t JOIN unit u ON u.id = t.unit_ref
             JOIN path p ON p.id = u.path_ref ORDER BY t.test_id",
        )
        .unwrap();
    let found: Vec<(String, String, String)> = tests
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (unit, test, state) in found {
        let entry = units[&unit].as_object_mut().unwrap();
        entry
            .entry("tests")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap()
            .insert(test, json!(state));
    }
    let rest: Map<String, Value> = rows(
        "SELECT p.path, v.entry FROM check_rest c JOIN version v ON v.id = c.version_ref
         JOIN path p ON p.id = v.path_ref",
    )
    .into_iter()
    .map(|(p, e)| (p, json!(e)))
    .collect();
    let list = |table: &str| -> Vec<String> {
        rows(&format!(
            "SELECT p.path, '' FROM {table} c JOIN path p ON p.id = c.path_ref ORDER BY p.path"
        ))
        .into_iter()
        .map(|(p, _)| p)
        .collect()
    };
    json!({
        "units": units,
        "rest": rest,
        "sources": list("check_source"),
        "unitFiles": list("check_listed"),
    })
}

/// The test files of the disagreements the one store in `cache` holds, oldest first.
#[allow(dead_code)]
pub fn disagreements(cache: &Path) -> Vec<String> {
    let stores = stores(cache);
    assert_eq!(stores.len(), 1, "one store in {}", cache.display());
    let conn = Connection::open_with_flags(&stores[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("the store opens");
    let mut query = conn
        .prepare("SELECT unit FROM disagreement ORDER BY id")
        .unwrap();
    query
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// \[C-38\] Each test file of the one store in `cache`, with where its row came from: `seen here`
/// or `shared`.
#[allow(dead_code)]
pub fn origins(cache: &Path) -> std::collections::BTreeMap<String, String> {
    let stores = stores(cache);
    assert_eq!(stores.len(), 1, "one store in {}", cache.display());
    let conn = Connection::open_with_flags(&stores[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("the store opens");
    let mut query = conn
        .prepare("SELECT p.path, u.origin FROM unit u JOIN path p ON p.id = u.path_ref")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

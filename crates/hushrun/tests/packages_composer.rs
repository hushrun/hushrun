//! `hushrun packages` with Composer (K-10): a store baked by `composer install --download-only`
//! holds the archives of Composer's files cache; `hushrun packages exec -- composer install` seeds
//! an empty cache with the archives `composer.lock` names now, and Composer installs `vendor/` with
//! nothing to download, the same `vendor/` an install from the network writes. A local server
//! stands in for the registry, so the tests need no network. The real Composer and PHP, as CI's
//! runner has them (`HUSHRUN_TEST_COMPOSER` names Composer, else `composer`); a machine without
//! them passes with a line. A run gets nothing from a Composer part, so no key changes.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/phpunit.rs"]
mod php;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use cli::Cli as _;
use repo::Repo;

const CLEAR: &[&str] = &[
    "HUSHRUN_PACKAGES_LOWER",
    "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    "HUSHRUN_PACKAGES",
    "COMPOSER",
    "COMPOSER_HOME",
    "COMPOSER_CACHE_DIR",
    "COMPOSER_CACHE_READ_ONLY",
    "COMPOSER_AUTH",
    "PHP_INI_SCAN_DIR",
];

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn composer_program() -> String {
    std::env::var("HUSHRUN_TEST_COMPOSER").unwrap_or_else(|_| "composer".to_string())
}

fn composer() -> bool {
    let skip = std::env::var("HUSHRUN_TEST_PHP").as_deref() == Ok("skip");
    let works = !skip
        && Command::new(composer_program())
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
    if !works {
        if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") && !skip {
            panic!("Composer is missing, and HUSHRUN_TEST_TOOLS=required");
        }
        eprintln!("note: skipping: no Composer (HUSHRUN_TEST_TOOLS=required makes this fail)");
    }
    works
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A registry nobody else reaches: it serves `files` by name over HTTP on 127.0.0.1, counts the
/// requests it answers, and refuses every request once `down` is set.
struct Registry {
    port: u16,
    served: Arc<AtomicUsize>,
    down: Arc<AtomicBool>,
}

impl Registry {
    fn start(files: BTreeMap<String, Vec<u8>>) -> Registry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let down = Arc::new(AtomicBool::new(false));
        let (count, refuse) = (served.clone(), down.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                // The headers, to the empty line.
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                    header.clear();
                }
                let name = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .trim_start_matches('/');
                let mut stream = &stream;
                match files.get(name).filter(|_| !refuse.load(Ordering::SeqCst)) {
                    Some(bytes) => {
                        count.fetch_add(1, Ordering::SeqCst);
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-tar\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = stream.write_all(bytes);
                    }
                    None => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                    }
                }
            }
        });
        Registry { port, served, down }
    }

    fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/{name}", self.port)
    }
}

/// A package's archive, as `tar` writes one: `composer.json` and one class.
fn archive(dir: &Path, name: &str) -> Vec<u8> {
    let src = dir.join(format!("src-{name}"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("composer.json"),
        format!("{{\"name\":\"acme/{name}\",\"version\":\"1.0.0\"}}\n"),
    )
    .unwrap();
    std::fs::write(
        src.join("Thing.php"),
        format!("<?php\nnamespace Acme\\{name};\nfinal class Thing {{}}\n"),
    )
    .unwrap();
    let to = dir.join(format!("{name}.tar"));
    let made = Command::new("tar")
        .arg("-cf")
        .arg(&to)
        .arg("-C")
        .arg(&src)
        .args(["composer.json", "Thing.php"])
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::read(to).unwrap()
}

fn locked(name: &str, url: &str) -> String {
    format!(
        r#"{{"name": "acme/{name}", "version": "1.0.0", "dist": {{"type": "tar", "url": "{url}", "shasum": ""}}, "type": "library", "autoload": {{"psr-4": {{"Acme\\{name}\\": ""}}}}}}"#
    )
}

fn lock(packages: &[String]) -> String {
    format!(
        "{{\n\"content-hash\": \"0\",\n\"packages\": [{}],\n\"packages-dev\": [],\n\"aliases\": [],\n\"minimum-stability\": \"stable\",\n\"stability-flags\": {{}},\n\"prefer-stable\": false,\n\"prefer-lowest\": false,\n\"platform\": {{}},\n\"platform-dev\": {{}},\n\"plugin-api-version\": \"2.6.0\"\n}}\n",
        packages.join(",\n")
    )
}

/// The project's own script touches `canary` after an install: a bake with `--no-scripts` runs
/// it not, a person's `composer install` does.
const COMPOSER_JSON: &str = r#"{"require": {"acme/a": "1.0.0", "acme/b": "1.0.0"}, "config": {"secure-http": false}, "scripts": {"post-install-cmd": ["@php -r \"touch('canary');\""]}}
"#;

/// Every file under `dir`, by its path from `dir`, with its bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

// covers: K-6, K-7, K-10
#[test]
fn a_store_baked_by_composer_installs_vendor_from_an_empty_cache_with_no_registry() {
    if !composer() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let registry = Registry::start(BTreeMap::from([
        ("a.tar".to_string(), archive(work.path(), "a")),
        ("b.tar".to_string(), archive(work.path(), "b")),
    ]));
    let lock_text = lock(&[
        locked("a", &registry.url("a.tar")),
        locked("b", &registry.url("b.tar")),
    ]);
    let r = Repo::new(&[
        file("composer.json", COMPOSER_JSON),
        file("composer.lock", &lock_text),
        file(".gitignore", "vendor\n"),
    ])
    .clear(CLEAR)
    .give("HUSHRUN_COMPOSER", composer_program());
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args(["--", &composer_program()])
        .args([
            "install",
            "--download-only",
            "--no-scripts",
            "--no-plugins",
            "--no-interaction",
        ])
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(baked.status.success(), "{said}");
    assert!(said.contains("packages: composer: 2 files"), "{said}");
    assert!(
        !r.repo.join("vendor").exists(),
        "--download-only writes no vendor"
    );
    assert!(!r.repo.join("canary").exists(), "a bake runs no script");
    let rows = std::fs::read_to_string(store.join("rows/composer.jsonl")).unwrap();
    assert!(rows.contains("\"package\":\"acme/a\""), "{rows}");
    assert!(!rows.contains(".htaccess"), "{rows}");

    // The reference: the same lock installed from the network.
    let composer_install = |cache: &Path| {
        let mut cmd = r.command(&composer_program());
        cmd.args(["install", "--no-interaction"])
            .env("COMPOSER_CACHE_DIR", cache);
        cmd.output().unwrap()
    };
    let network = composer_install(&r.root.join("network"));
    assert!(network.status.success(), "{}", out(&network));
    let from_network = tree(&r.repo.join("vendor"));
    assert!(from_network.contains_key("acme/a/Thing.php"));
    assert!(
        r.repo.join("canary").exists(),
        "the person's install runs its scripts"
    );

    // The registry goes away. An unsigned store with no say-so is untrusted, and read not at all.
    registry.down.store(true, Ordering::SeqCst);
    let served = registry.served.load(Ordering::SeqCst);
    let install = |cache: &Path, lower: &Path, say_so: bool| {
        let _ = std::fs::remove_dir_all(r.repo.join("vendor"));
        let mut cmd = r.hushrun();
        cmd.args(["packages", "exec", "--", &composer_program()])
            .args(["install", "--no-interaction"])
            .env("COMPOSER_CACHE_DIR", cache)
            .env("HUSHRUN_PACKAGES_LOWER", lower);
        if say_so {
            cmd.env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1");
        }
        cmd.output().unwrap()
    };
    let untrusted = install(&r.root.join("empty0"), &store, false);
    assert!(!untrusted.status.success(), "{}", out(&untrusted));
    assert!(!out(&untrusted).contains("seeded"), "{}", out(&untrusted));

    // With the say-so: seeded, installed, nothing downloaded, and the same vendor/.
    let first = install(&r.root.join("empty"), &store, true);
    let said = out(&first);
    assert!(first.status.success(), "{said}");
    assert!(
        said.contains("packages: composer: seeded 2 files"),
        "{said}"
    );
    assert_eq!(registry.served.load(Ordering::SeqCst), served);
    assert_eq!(
        tree(&r.repo.join("vendor")),
        from_network,
        "a seeded install writes the vendor/ the network's does"
    );
    // The second seed into the same cache costs one stat.
    let again = install(&r.root.join("empty"), &store, true);
    assert!(out(&again).contains("seeded already"), "{}", out(&again));
    // Without the store the same install fails: the store is what it read.
    assert!(
        !install(&r.root.join("empty2"), Path::new(""), true)
            .status
            .success()
    );

    // The lock changed by one package (another URL, so another archive): the archive it no
    // longer names is left out, and no stamp is written.
    r.write(
        "composer.lock",
        &lock(&[
            locked("a", &registry.url("a.tar")),
            locked("b", &registry.url("b.tar?v=2")),
        ]),
    );
    let changed = install(&r.root.join("empty3"), &store, true);
    let said = out(&changed);
    assert!(said.contains("seeded 1 files"), "{said}");
    assert!(
        said.contains("left out 1 files no lockfile here names now"),
        "{said}"
    );
    r.write("composer.lock", &lock_text);

    // One byte of one archive flipped: the seed catches it, and Composer, with no registry,
    // fails.
    let archive_row = rows
        .lines()
        .next()
        .and_then(|l| l.split("\"path\":\"").nth(1))
        .and_then(|l| l.split('"').next())
        .unwrap()
        .to_string();
    let at = store.join("parts/composer").join(&archive_row);
    let mut bytes = std::fs::read(&at).unwrap();
    bytes[10] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let tampered = install(&r.root.join("empty4"), &store, true);
    assert!(
        out(&tampered).contains("files whose bytes changed since the bake"),
        "{}",
        out(&tampered)
    );
    assert!(!tampered.status.success(), "{}", out(&tampered));
}

/// A store with a `composer` part harvested from a hand-made cache: no Composer needed.
fn harvested(r: &Repo) -> PathBuf {
    let cache = r.root.join("composer-cache");
    let dir = cache.join("files/acme/a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{}.tar", "0".repeat(40))), "archive").unwrap();
    std::fs::write(cache.join(".htaccess"), "deny from all").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg(format!("--harvest=composer={}", cache.display()))
        .output()
        .unwrap();
    assert!(baked.status.success(), "{}", out(&baked));
    assert!(
        out(&baked).contains("packages: composer: 1 files"),
        "{}",
        out(&baked)
    );
    store
}

// covers: K-10
// The keys of a Composer part: everything COMMAND sees in a run is the same with and without the
// store, `hushrun run` seeds nothing, and a PHPUnit check (whose adapter sets PHP_INI_SCAN_DIR
// and reads vendor/) run with the store is skipped without it.
#[test]
fn a_composer_part_gives_a_run_nothing_and_changes_no_key() {
    let lock_text = lock(&[locked("a", "https://example.com/a.tar")]);
    let r = Repo::new(&[file("composer.lock", &lock_text)]).clear(CLEAR);
    let store = harvested(&r);
    let cache = r.root.join("seeded");
    let env_of = |lower: &Path| {
        let ran = r
            .hushrun()
            .args(["run", "e", "--force", "--", "sh", "-c", "env | sort"])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("COMPOSER_CACHE_DIR", &cache)
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", out(&ran));
        String::from_utf8_lossy(&ran.stdout)
            .lines()
            .filter(|l| {
                !l.starts_with("HUSHRUN_PACKAGES_LOWER=") && !l.contains("/build/sessions/")
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let (with, without) = (env_of(&store), env_of(Path::new("")));
    let differ: Vec<&String> = with
        .iter()
        .filter(|l| !without.contains(l))
        .chain(without.iter().filter(|l| !with.contains(l)))
        .collect();
    assert!(
        differ.is_empty(),
        "the store changed what COMMAND sees: {differ:?}"
    );
    assert!(!cache.exists(), "`hushrun run` never seeds");
    for runner in php::phpunits() {
        let mut files = php::shop("{mode: reuse, unit: test}");
        files.push(file("composer.lock", &lock_text));
        let r = php::repo(&runner, &files).clear(CLEAR);
        let store = harvested(&r);
        let run = ["run", "unit", "--", "vendor/bin/phpunit"];
        let with = r.run(
            &run,
            &[
                ("HUSHRUN_PACKAGES_LOWER", store.to_str().unwrap()),
                ("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"),
            ],
        );
        assert_eq!(with.code, 0, "{}\n{}", with.stdout, with.stderr);
        let without = r.run(&run, &[("HUSHRUN_PACKAGES_LOWER", "")]);
        assert!(
            without.stderr.contains("unit skipped"),
            "{}: {}",
            runner.name(),
            without.stderr
        );
    }
}

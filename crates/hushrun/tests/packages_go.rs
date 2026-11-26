//! `hushrun packages` with Go (K-6, K-10): a store baked by the check's own `go mod download` is
//! served to Go as a registry, `GOPROXY=file://<part>/download,` then the proxies Go would use,
//! and an empty module cache builds offline from it. Go checks each module against `go.sum`. The
//! module the fixture needs is made here, as a module proxy lays it out, so nothing is
//! downloaded. Needs Go: it says so and passes where Go is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/go.rs"]
mod go;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;

use cli::Cli as _;
use go::{file, has_go, tools};

/// The module the fixture depends on.
const DEP: &str = "example.com/dep";

/// A zip of `files`, stored, as Go's module zips may be.
fn zip(files: &[(&str, &str)]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for b in bytes {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, text) in files {
        let (name, data) = (name.as_bytes(), text.as_bytes());
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            for v in [20u16, 0, 0, 0, 0x21] {
                h.extend(v.to_le_bytes());
            }
            h.extend(crc.to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                for v in [0u16, 0, 0] {
                    h.extend(v.to_le_bytes());
                }
                h.extend(0u32.to_le_bytes());
                h.extend(offset.to_le_bytes());
            }
            h.extend(name);
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(data);
        central.extend(header(0x0201_4b50, true));
    }
    let (at, len) = (out.len() as u32, central.len() as u32);
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    for v in [0u16, 0, files.len() as u16, files.len() as u16] {
        out.extend(v.to_le_bytes());
    }
    out.extend(len.to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// A module proxy's folder at `dir` that serves [`DEP`] v1.0.0, whose `Two` returns `two`.
fn origin(dir: &Path, two: char) -> Vec<u8> {
    let at = dir.join(DEP).join("@v");
    std::fs::create_dir_all(&at).unwrap();
    let module = format!("module {DEP}\n\ngo 1.21\n");
    let source = format!("package dep\n\nfunc Two() int {{ return {two} }}\n");
    let zipped = zip(&[
        (&format!("{DEP}@v1.0.0/go.mod"), &module),
        (&format!("{DEP}@v1.0.0/dep.go"), &source),
    ]);
    std::fs::write(at.join("v1.0.0.mod"), &module).unwrap();
    std::fs::write(at.join("v1.0.0.zip"), &zipped).unwrap();
    std::fs::write(at.join("v1.0.0.info"), "{\"Version\":\"v1.0.0\"}\n").unwrap();
    zipped
}

// covers: K-6, K-10
#[test]
fn a_store_baked_by_go_mod_download_is_go_s_first_proxy_and_go_sum_checks_it() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = go::repo(&[
        file(
            "go.mod",
            &format!("module example.com/shop\n\ngo 1.21\n\nrequire {DEP} v1.0.0\n"),
        ),
        file(
            "shop_test.go",
            &format!("package shop\n\nimport (\n\t\"testing\"\n\n\t\"{DEP}\"\n)\n\nfunc TestTwo(t *testing.T) {{\n\tif dep.Two() != 2 {{\n\t\tt.Fatal(\"two\")\n\t}}\n}}\n"),
        ),
    ])
    .clear(&[
        "GOPROXY",
        "GOSUMDB",
        "GONOSUMDB",
        "GOPRIVATE",
        "GONOPROXY",
        "GOMODCACHE",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
    ]);
    let proxy = r.root.join("origin");
    origin(&proxy, '2');
    let from_origin = format!("file://{}", proxy.display());
    // go.sum, as Go writes it from the module it fetched.
    let tidy = r
        .command("go")
        .args(["mod", "tidy"])
        .env("GOPROXY", &from_origin)
        .env("GOSUMDB", "off")
        .env("GOMODCACHE", r.root.join("tidy-cache"))
        .output()
        .unwrap();
    assert!(
        tidy.status.success(),
        "{}",
        String::from_utf8_lossy(&tidy.stderr)
    );
    hushrun_testkit::git::commit_all(&r.repo, "go.sum");

    // The bake runs the fetch with a module cache of its own.
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args(["--", "go", "mod", "download"])
        .env("GOPROXY", &from_origin)
        .env("GOSUMDB", "off")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout).into_owned();
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: go: "), "{said}");
    assert!(said.contains("(go.mod, go.sum)"), "{said}");
    let at = store.join("parts/go/download").join(DEP).join("@v");
    assert!(at.join("v1.0.0.zip").is_file() && at.join("v1.0.0.mod").is_file());
    for left in ["list", "v1.0.0.lock", "v1.0.0.ziphash"] {
        assert!(!at.join(left).exists(), "{left} is Go's own note");
    }
    assert!(!store.join("parts/go/download/sumdb").exists());
    let rows = std::fs::read_to_string(store.join("rows/go.jsonl")).unwrap();
    assert!(
        rows.contains(&format!("\"package\":\"{DEP}@v1.0.0\"")),
        "{rows}"
    );
    assert!(rows.contains("\"locked\":\"h1:"), "{rows}");
    // The fetch's module cache, whose folders Go makes read-only, is gone.
    let left: Vec<String> = std::fs::read_dir(&r.root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.') && !n.ends_with(".lock"))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // Read: an empty module cache, no GOPROXY of the person's, the origin gone.
    std::fs::remove_dir_all(&proxy).unwrap();
    let cache = r.root.join("modcache");
    let run = |name: &str, store: &Path, cache: &Path| {
        let mut cmd = r.hushrun();
        cmd.args(["run", name, "--", "go", "test", "./..."])
            .env("GOMODCACHE", cache)
            .env("HUSHRUN_PACKAGES_LOWER", store);
        cmd.output().unwrap()
    };
    let first = run("t", &store, &cache);
    let err = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(first.status.success(), "{err}");
    assert!(err.contains("t passed"), "{err}");
    assert!(cache.join(format!("{DEP}@v1.0.0/dep.go")).is_file());
    // covers: K-10
    // The check's key is the same with and without the store: the run without it skips.
    let second = run("t", Path::new(""), &cache);
    let err = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(err.contains("t skipped"), "{err}");
    // Without the store, and with no proxy to fall back to, the same build fails.
    let without = r
        .hushrun()
        .args(["run", "u", "--", "go", "test", "./..."])
        .env("GOMODCACHE", r.root.join("modcache-2"))
        .env("GOPROXY", "off")
        .env("HUSHRUN_PACKAGES_LOWER", "")
        .output()
        .unwrap();
    assert!(!without.status.success());

    // `env` prints GOPROXY with the part first, then Go's own proxies.
    let env = |vars: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store);
        for (n, v) in vars {
            cmd.env(n, v);
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (out, _) = env(&[]);
    assert_eq!(
        out,
        format!(
            "export GOPROXY='file://{}/parts/go/download,https://proxy.golang.org,direct'\n",
            store.display()
        )
    );
    // A GOPROXY of the person's, or a module Go would take unchecked: left alone.
    let (out, err) = env(&[("GOPROXY", "https://goproxy.example.com")]);
    assert_eq!(out, "");
    assert!(
        err.contains("packages: go: left alone: GOPROXY is set already"),
        "{err}"
    );
    let (out, err) = env(&[("GOPRIVATE", "example.com")]);
    assert_eq!(out, "");
    assert!(
        err.contains("packages: go: left alone: GOPRIVATE is set, so Go takes some modules go.sum doesn't name unchecked"),
        "{err}"
    );

    // A module changed in the store, the same size: go.sum refuses it.
    let changed = origin(&r.root.join("changed"), '3');
    std::fs::write(at.join("v1.0.0.zip"), changed).unwrap();
    let tampered = run("v", &store, &r.root.join("modcache-3"));
    let err = String::from_utf8_lossy(&tampered.stderr).into_owned();
    let out = String::from_utf8_lossy(&tampered.stdout).into_owned();
    assert!(!tampered.status.success(), "{err}");
    assert!(
        format!("{out}{err}").contains("SECURITY ERROR"),
        "{out}{err}"
    );
}

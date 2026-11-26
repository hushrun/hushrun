//! `hushrun packages` with NuGet (K-6, K-10): a store harvested from a filled global packages
//! folder is read by `dotnet test` offline with an empty one, as a fallback folder through
//! `NUGET_FALLBACK_PACKAGES`, and the check's key is the same with and without it. Needs the .NET
//! SDK (`HUSHRUN_TEST_DOTNET`); its packages come from `HUSHRUN_TEST_DOTNET_PACKAGES`, filled from
//! nuget.org the first time, as `e2e_dotnet.rs`'s do.

#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/dotnet.rs"]
mod dotnet_fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};

use hushrun_testkit::git;

use cli::Cli as _;
use dotnet_fixtures::{dotnet, packages, project, run_dotnet};

/// Every file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

// covers: K-6, K-10
#[test]
fn a_store_harvested_from_a_global_packages_folder_is_read_offline_and_keys_nothing() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework).clear(&[
        "NUGET_FALLBACK_PACKAGES",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
    ]);
    // Online, the fixture's packages fill the tests' global packages folder.
    let (code, said) = run_dotnet(&r, &dotnet, &["restore"]);
    assert_eq!(code, 0, "{said}");
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("nuget={}", packages().display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout);
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: nuget: "), "{said}");
    assert!(
        said.contains("nuget.config, src/Calc/Calc.csproj"),
        "{said}"
    );
    assert!(
        store
            .join("parts/nuget/packages/xunit/2.9.2/.nupkg.metadata")
            .is_file(),
        "NuGet's mark is kept"
    );
    // Offline from here: no source at all.
    r.write(
        "nuget.config",
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<configuration>\n  <packageSources>\n    <clear />\n  </packageSources>\n</configuration>\n",
    );
    git::commit_all(&r.repo, "offline");
    let empty = r.root.join("empty-nuget");
    let empty_str = empty.to_str().unwrap().to_string();
    let clean = || {
        let _ = std::fs::remove_dir_all(&empty);
        for project in [
            "src/Calc",
            "tests/Calc.XTests",
            "tests/Calc.NTests",
            "tests/Calc.MTests",
        ] {
            let _ = std::fs::remove_dir_all(r.repo.join(project).join("obj"));
            let _ = std::fs::remove_dir_all(r.repo.join(project).join("bin"));
        }
    };
    // covers: K-10
    // In `hushrun run`, with an empty global packages folder: the tests pass from the store, and
    // nothing is copied.
    clean();
    let first = r.run(
        &["run", "unit", "--", "dotnet", "test"],
        &[
            ("NUGET_PACKAGES", &empty_str),
            ("HUSHRUN_PACKAGES_LOWER", store.to_str().unwrap()),
            ("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"),
        ],
    );
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    let copied: Vec<_> = all_files(&empty)
        .into_iter()
        .filter(|f| f.to_string_lossy().ends_with(".nupkg"))
        .collect();
    assert!(
        copied.is_empty(),
        "read in place, nothing copied: {copied:?}"
    );
    // The keys: a unit's key holds what its test project compiles, never a package's file, nor
    // `obj/project.assets.json`, which names each package's folder, the store's included.
    let map = r.map();
    let units = map["units"].as_object().expect("units in the map");
    assert!(!units.is_empty(), "{map}");
    for (unit, at) in units {
        for read in at["reads"].as_array().into_iter().flatten() {
            let read = read.as_str().unwrap_or_default();
            assert!(
                !(read.contains("obj/")
                    || read.contains("project.assets.json")
                    || read.contains(".nupkg")
                    || read.contains("parts/nuget")),
                "{unit} reads {read}"
            );
        }
    }
    // And the same check without the store skips. `obj/project.assets.json`, which names the
    // store's folder, is in no key.
    let second = r.run(
        &["run", "unit", "--", "dotnet", "test"],
        &[
            ("NUGET_PACKAGES", &empty_str),
            ("HUSHRUN_PACKAGES_LOWER", ""),
        ],
    );
    let said = format!("{}{}", second.stdout, second.stderr);
    assert_eq!(second.code, 0, "{said}");
    assert!(said.contains("unit skipped"), "{said}");
    // Without the store the same offline restore fails: the store is what it read.
    clean();
    let restore = |lower: &Path| {
        r.hushrun()
            .args(["packages", "exec", "--", "dotnet", "restore"])
            .env("NUGET_PACKAGES", &empty)
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .output()
            .unwrap()
    };
    assert!(!restore(Path::new("")).status.success());
    assert!(restore(&store).status.success());
    // One package more, the network on: NuGet downloads only what the store lacks.
    r.write(
        "nuget.config",
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<configuration>\n  <packageSources>\n    <clear />\n    <add key=\"nuget.org\" value=\"https://api.nuget.org/v3/index.json\" />\n  </packageSources>\n</configuration>\n",
    );
    r.write(
        "src/Calc/Calc.csproj",
        &format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"Humanizer.Core\" Version=\"2.14.1\" />\n  </ItemGroup>\n</Project>\n"
        ),
    );
    clean();
    let online = restore(&store);
    assert!(
        online.status.success(),
        "{}",
        String::from_utf8_lossy(&online.stdout)
    );
    assert!(
        empty
            .join("humanizer.core/2.14.1/.nupkg.metadata")
            .is_file()
    );
    assert!(
        !empty.join("xunit").exists(),
        "what the store holds isn't downloaded"
    );
    // One byte flipped in a file of the part, the same size: NuGet reads it without a word (it
    // hashes nothing it reads from a fallback folder), and `hushrun packages verify` finds it.
    let dll = all_files(&store.join("parts/nuget/packages"))
        .into_iter()
        .find(|f| f.extension().is_some_and(|e| e == "dll"))
        .expect("a dll in the part");
    let mut bytes = std::fs::read(&dll).unwrap();
    bytes[100] ^= 0xff;
    make_writable(&dll);
    std::fs::write(&dll, &bytes).unwrap();
    clean();
    assert!(restore(&store).status.success(), "NuGet doesn't check");
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(!verify.status.success());
    let said = String::from_utf8_lossy(&verify.stderr);
    let name = dll.file_name().unwrap().to_string_lossy().into_owned();
    assert!(said.contains(&name), "{said}");
    bytes[100] ^= 0xff;
    std::fs::write(&dll, &bytes).unwrap();
    // `env` prints the variable, after the person's own folders.
    let env = |theirs: Option<&str>| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store)
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1");
        if let Some(theirs) = theirs {
            cmd.env("NUGET_FALLBACK_PACKAGES", theirs);
        }
        String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned()
    };
    let part = store.join("parts/nuget/packages");
    assert_eq!(
        env(None),
        format!("export NUGET_FALLBACK_PACKAGES='{}'\n", part.display())
    );
    assert_eq!(
        env(Some("/mine")),
        format!(
            "export NUGET_FALLBACK_PACKAGES='/mine;{}'\n",
            part.display()
        )
    );
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

/// A file a write may change: the bake writes a part's files read-only.
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o200)).unwrap();
}

/// A repository with one project and a harvested store of one package, signed by `key` when
/// given, and the store's folder.
fn harvested(key: Option<&Path>) -> (repo::Repo, PathBuf) {
    let r = repo::Repo::new(&[(
        "Calc.csproj".to_string(),
        "<Project Sdk=\"Microsoft.NET.Sdk\" />\n".to_string(),
    )])
    .clear(&[
        "NUGET_FALLBACK_PACKAGES",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
    ]);
    let cache = r.root.join("nuget");
    let file = cache.join("n/1.0.0/n.nuspec");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"<package/>").unwrap();
    std::fs::write(cache.join("n/1.0.0/.nupkg.metadata"), b"{}").unwrap();
    let mut public = None;
    if let Some(key) = key {
        let made = r
            .hushrun()
            .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
            .arg(key)
            .output()
            .unwrap();
        assert!(made.status.success());
        let out = r
            .hushrun()
            .args(["keys", "public", "--key"])
            .arg(key)
            .output()
            .unwrap();
        public = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("public key: ").map(str::to_string));
    }
    if let Some(public) = public {
        r.write(
            "hushrun.yaml",
            &format!(
                "version: 1\nrepository: github.com/acme/calc\nbuild:\n  baked:\n    signers:\n      - key: {public}\nchecks:\n  test:\n    run: dotnet test\n"
            ),
        );
    }
    let store = r.root.join("packages");
    let mut bake = r.hushrun();
    bake.args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("nuget={}", cache.display()));
    if let Some(key) = key {
        bake.arg("--signing-key").arg(key);
    }
    let baked = bake.output().unwrap();
    assert!(
        baked.status.success(),
        "{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    (r, store)
}

// covers: K-10
#[test]
fn a_nuget_configs_fallback_folders_are_carried_first() {
    let (r, store) = harvested(None);
    r.write(
        "nuget.config",
        "<configuration>\n  <fallbackPackageFolders>\n    <add key=\"team\" value=\"/opt/team-packages\" />\n  </fallbackPackageFolders>\n</configuration>\n",
    );
    let env = || {
        let out = r
            .hushrun()
            .args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store)
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (out, _) = env();
    assert_eq!(
        out,
        format!(
            "export NUGET_FALLBACK_PACKAGES='/opt/team-packages;{}'\n",
            store.join("parts/nuget/packages").display()
        )
    );
    // A list hushrun can't read exactly is never dropped: the part is left alone, with a line.
    r.write(
        "nuget.config",
        "<configuration><fallbackPackageFolders><add key=\"team\" value=\"%TEAM%\" /></fallbackPackageFolders></configuration>\n",
    );
    let (out, err) = env();
    assert_eq!(out, "");
    assert!(err.contains("packages: nuget: left alone: "), "{err}");
}

// covers: K-7, K-8
#[test]
fn a_byte_changed_in_a_signed_nuget_part_keeps_it_out_in_ci() {
    let key_dir = tempfile::tempdir().unwrap();
    let key = key_dir.path().join("key.pem");
    let (r, store) = harvested(Some(&key));
    let env = || {
        let out = r
            .hushrun()
            .args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store)
            .env("CI", "true")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (out, _) = env();
    assert!(
        out.contains("NUGET_FALLBACK_PACKAGES="),
        "a signed store is read in CI: {out}"
    );
    // The same size, one byte changed: a run that may write what others trust hashes it first.
    let nuspec = store.join("parts/nuget/packages/n/1.0.0/n.nuspec");
    make_writable(&nuspec);
    std::fs::write(&nuspec, b"<packagE/>").unwrap();
    let (out, err) = env();
    assert_eq!(out, "", "{err}");
    assert!(err.contains("nuget"), "{err}");
}

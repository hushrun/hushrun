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
    // The keys: the same check without the store skips. `obj/project.assets.json`, which names the
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

// covers: K-10
#[test]
fn a_nuget_config_that_names_fallback_folders_keeps_the_part_out() {
    let Some((dotnet, framework)) = dotnet() else {
        return;
    };
    let r = project(&dotnet, &framework).clear(&["NUGET_FALLBACK_PACKAGES"]);
    let cache = r.root.join("nuget");
    let file = cache.join("n/1.0.0/n.nuspec");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"<package/>").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("nuget={}", cache.display()))
        .output()
        .unwrap();
    assert!(
        baked.status.success(),
        "{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    r.write(
        "nuget.config",
        "<configuration>\n  <fallbackPackageFolders>\n    <add key=\"team\" value=\"/opt/team-packages\" />\n  </fallbackPackageFolders>\n</configuration>\n",
    );
    let out = r
        .hushrun()
        .args(["packages", "env"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("packages: nuget: left alone: ")
            && said.contains("names fallback folders, which NUGET_FALLBACK_PACKAGES would replace"),
        "{said}"
    );
}

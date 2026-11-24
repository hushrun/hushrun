use super::*;

fn tree(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&str> = paths.to_vec();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).expect("a manifest")
}

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn word(command: &str) -> Option<Result<&'static str, String>> {
    recognized(&words(command), Path::new(".")).map(|found| found.map(|a| a.word()))
}

// covers: C-114
#[test]
fn mix_test_is_exunits_and_what_chooses_from_earlier_runs_is_not_offered() {
    for command in [
        "mix test",
        "mix test --trace",
        "mix test test/a_test.exs:12",
        "mix --no-compile test",
        "/opt/elixir/bin/mix test --cover",
        "env MIX_ENV=test mix test",
        "mix.bat test",
    ] {
        assert_eq!(word(command), Some(Ok(WORD)), "{command}");
    }
    for command in [
        "mix compile",
        "mix do test",
        "mix ci",
        "iex -S mix test",
        "elixir test.exs",
        "mix",
    ] {
        assert_eq!(word(command), None, "{command}");
    }
    for (command, why) in [
        ("mix test --stale", "--stale"),
        ("mix test --failed", "--failed"),
        ("mix test --listen-on-stdin", "--listen-on-stdin"),
        ("mix test --formatter JUnitFormatter", "--formatter"),
        ("mix test --formatter=JUnitFormatter", "--formatter"),
    ] {
        let Some(Err(said)) = word(command) else {
            panic!("{command}");
        };
        assert!(said.contains(why), "{command}: {said}");
    }
}

// covers: C-114
#[test]
fn the_module_loads_beside_what_the_environment_says() {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let vars = variables(
        Path::new("/c/adapters/exunit-1"),
        Some("/opt/libs".into()),
        Some("+fnu".into()),
    );
    assert_eq!(vars[0].0, "ERL_LIBS");
    assert_eq!(
        vars[0].1,
        OsString::from(format!("/c/adapters/exunit-1{sep}/opt/libs"))
    );
    assert_eq!(vars[1].0, "ELIXIR_ERL_OPTIONS");
    assert_eq!(vars[1].1, OsString::from(format!("+fnu {START}")));
    let bare = variables(Path::new("/c/a"), Some("".into()), None);
    assert_eq!(bare[0].1, OsString::from("/c/a"));
    assert_eq!(bare[1].1, OsString::from(START));
    // The `elixir` script splits ELIXIR_ERL_OPTIONS on blanks and expands globs, unquoted: the
    // start holds neither past its first word.
    let (flag, expression) = START.split_once(' ').unwrap();
    assert_eq!(flag, "-eval");
    assert!(
        !expression.contains([' ', '\t', '*', '?', '[']),
        "{expression}"
    );
    // A start that fails, even where the module can't load, is caught: the VM goes on.
    assert!(expression.starts_with("catch("), "{expression}");
}

// covers: C-114
#[test]
fn the_unit_rule_is_mixs_default_below_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("mix.exs", "defmodule A.MixProject do end\n");
    write("apps/web/mix.exs", "defmodule Web.MixProject do end\n");
    write(
        "apps/odd/mix.exs",
        "def project, do: [test_paths: [\"spec\"]]\n",
    );
    let tree = tree(&[
        "mix.exs",
        "test/a_test.exs",
        "test/nested/b_test.exs",
        "test/test_helper.exs",
        "test/support/case.ex",
        "lib/a_test.exs",
        "apps/web/mix.exs",
        "apps/web/test/web_test.exs",
        "apps/odd/mix.exs",
        "apps/odd/test/odd_test.exs",
        "elsewhere/test/c_test.exs",
    ]);
    let rule = |path: &str| is_test_file(root, &tree, path);
    assert_eq!(rule("test/a_test.exs"), Some(true));
    assert_eq!(rule("test/nested/b_test.exs"), Some(true));
    assert_eq!(rule("apps/web/test/web_test.exs"), Some(true));
    assert_eq!(rule("test/test_helper.exs"), Some(false));
    assert_eq!(rule("test/support/case.ex"), Some(false));
    assert_eq!(rule("lib/a_test.exs"), Some(false));
    // The root's mix.exs is the project above it.
    assert_eq!(rule("elsewhere/test/c_test.exs"), Some(false));
    // A project that names its own test files: this version can't tell.
    assert_eq!(rule("apps/odd/test/odd_test.exs"), None);
    let sets = sets_of(&tree, root, &[]);
    assert_eq!(sets.of("test/a_test.exs"), Some(Set::Units));
    assert_eq!(sets.of("test/support/case.ex"), Some(Set::Sources));
    assert_eq!(sets.of("test/test_helper.exs"), Some(Set::Sources));
    assert_eq!(sets.of("mix.exs"), Some(Set::Sources));
}

#[test]
fn erlc_is_found_on_the_path_command_gets() {
    let dir = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "erlc.exe" } else { "erlc" };
    std::fs::write(dir.path().join(name), "").unwrap();
    let path = std::env::join_paths([Path::new("/nowhere"), dir.path()]).unwrap();
    assert_eq!(erlc(Some(path)), Some(dir.path().join(name)));
    assert_eq!(erlc(Some("/nowhere".into())), None);
    assert_eq!(erlc(None), None);
}

/// With an `erlc` here, the module compiles, once: the second install finds it.
#[test]
fn the_module_compiles_with_erlc_once() {
    let Some(erlc) = erlc(std::env::var_os("PATH")) else {
        eprintln!("note: skipping: no erlc on PATH");
        return;
    };
    let cache = tempfile::tempdir().unwrap();
    let lib = install(cache.path(), &erlc).unwrap();
    let beam = lib.join(MODULE).join("ebin").join("hushrun_exunit.beam");
    let first = std::fs::metadata(&beam).unwrap().modified().unwrap();
    assert_eq!(install(cache.path(), &erlc).unwrap(), lib);
    assert_eq!(std::fs::metadata(&beam).unwrap().modified().unwrap(), first);
}

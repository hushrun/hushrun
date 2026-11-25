use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn recognized_in(cwd: &Path, text: &str) -> Option<Result<Recognized, String>> {
    recognize(&words(text), cwd)
}

// covers: C-91
#[test]
fn one_run_of_phpunit_or_pest_is_recognized_however_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    for (command, pest) in [
        ("phpunit", false),
        ("vendor/bin/phpunit --testsuite unit", false),
        ("./vendor/bin/phpunit", false),
        ("php vendor/bin/phpunit", false),
        ("php -d memory_limit=-1 vendor/bin/phpunit", false),
        (
            "php8.3 -dmemory_limit=1G vendor/phpunit/phpunit/phpunit",
            false,
        ),
        ("vendor/bin/pest", true),
        ("php vendor/bin/pest --ci", true),
        ("pest --filter cart", true),
    ] {
        let found = recognized_in(cwd, command).unwrap().unwrap();
        assert_eq!(found.pest, pest, "{command}");
        assert!(!found.coverage, "{command}");
    }
    let found = recognized_in(cwd, "php vendor/bin/phpunit")
        .unwrap()
        .unwrap();
    assert_eq!(found.php.as_deref(), Some("php"));
    assert_eq!(found.vendor, Some(cwd.join("vendor")));
    for command in [
        "php artisan test",
        "phpstan analyse",
        "npm test",
        "php -r 1",
    ] {
        assert!(recognized_in(cwd, command).is_none(), "{command}");
    }
    for (command, why) in [
        ("php -n vendor/bin/phpunit", "php -n reads no ini file"),
        (
            "php -d auto_prepend_file=x.php vendor/bin/phpunit",
            "php -d auto_prepend_file replaces",
        ),
        (
            "vendor/bin/phpunit --list-tests",
            "isn't one run of the test files",
        ),
        ("vendor/bin/pest --parallel", "runs its tests in workers"),
        ("vendor/bin/pest -p", "runs its tests in workers"),
        ("vendor/bin/pest --dirty", "chooses test files from git"),
        ("vendor/bin/pest --watch", "reruns as files change"),
    ] {
        let err = recognized_in(cwd, command).unwrap().unwrap_err();
        assert!(err.contains(why), "{command}: {err}");
    }
    // PHPUnit has no `-p`: it is a value or an argument there, not Pest's parallel run.
    assert!(recognized_in(cwd, "vendor/bin/phpunit -p").unwrap().is_ok());
    for command in [
        "vendor/bin/phpunit --coverage-html out",
        "vendor/bin/phpunit --coverage-clover=c.xml",
        "vendor/bin/pest --coverage --min=80",
    ] {
        assert!(
            recognized_in(cwd, command).unwrap().unwrap().coverage,
            "{command}"
        );
    }
}

// covers: C-91
#[test]
fn a_composer_script_that_runs_phpunit_is_its_run() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    std::fs::write(
        cwd.join("composer.json"),
        r#"{"scripts": {
            "test": "phpunit --colors=always",
            "unit": ["@putenv XDEBUG_MODE=off", "@php vendor/bin/pest"],
            "ci": ["@lint", "@test"],
            "lint": "phpstan analyse",
            "cover": "XDEBUG_MODE=coverage phpunit --coverage-text",
            "both": ["phpunit", "pest"],
            "hook": "App\\Composer::run"
        }}"#,
    )
    .unwrap();
    let found = recognized_in(cwd, "composer test").unwrap().unwrap();
    assert!(!found.pest);
    let found = recognized_in(cwd, "composer run-script unit")
        .unwrap()
        .unwrap();
    assert!(found.pest);
    assert_eq!(found.php.as_deref(), Some("php"));
    assert!(recognized_in(cwd, "composer run ci").unwrap().is_ok());
    assert!(
        recognized_in(cwd, "composer --no-interaction ci")
            .unwrap()
            .is_ok()
    );
    assert!(
        recognized_in(cwd, "composer cover")
            .unwrap()
            .unwrap()
            .coverage
    );
    assert!(recognized_in(cwd, "composer both").unwrap().is_err());
    for command in [
        "composer lint",
        "composer hook",
        "composer install",
        "composer nothing",
    ] {
        assert!(recognized_in(cwd, command).is_none(), "{command}");
    }
    // `-d` names where composer.json is.
    let sub = cwd.join("app");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("composer.json"), r#"{"scripts": {"t": "pest"}}"#).unwrap();
    assert!(
        recognized_in(cwd, "composer -d app t")
            .unwrap()
            .unwrap()
            .pest
    );
    assert!(
        recognized_in(cwd, "composer --working-dir=app t")
            .unwrap()
            .unwrap()
            .pest
    );
    assert!(recognized_in(cwd, "composer t").is_none());
}

// covers: C-91
#[test]
fn only_the_phpunit_and_pest_majors_it_knows_load() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let installed = |packages: &str| {
        std::fs::create_dir_all(cwd.join("vendor/composer")).unwrap();
        std::fs::write(
            cwd.join("vendor/composer/installed.json"),
            format!(r#"{{"packages": [{packages}]}}"#),
        )
        .unwrap();
    };
    installed(r#"{"name": "phpunit/phpunit", "version": "12.5.45"}"#);
    assert!(
        recognized(&words("vendor/bin/phpunit"), cwd)
            .unwrap()
            .is_ok()
    );
    installed(r#"{"name": "phpunit/phpunit", "version": "9.6.39"}"#);
    let err = recognized(&words("vendor/bin/phpunit"), cwd)
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err,
        "this PHPUnit, 9.6.39, isn't one hushrun's adapter knows (10, 11, 12)"
    );
    installed(
        r#"{"name": "pestphp/pest", "version": "v4.1.0"}, {"name": "phpunit/phpunit", "version": "12.5.0"}"#,
    );
    assert!(recognized(&words("vendor/bin/pest"), cwd).unwrap().is_ok());
    installed(r#"{"name": "pestphp/pest", "version": "v2.36.0"}"#);
    assert!(recognized(&words("vendor/bin/pest"), cwd).unwrap().is_err());
}

#[test]
fn the_adapter_loads_beside_what_the_environment_says() {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let vars = variables(Path::new("/c/adapters/phpunit-1"), None, None);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].0, "PHP_INI_SCAN_DIR");
    // An empty first entry keeps PHP's own scan directory.
    assert_eq!(
        vars[0].1,
        OsString::from(format!("{sep}/c/adapters/phpunit-1"))
    );
    let vars = variables(
        Path::new("/c/a"),
        Some("/etc/php/conf.d".into()),
        Some("/srv/prepend.php".into()),
    );
    assert_eq!(
        vars[0].1,
        OsString::from(format!("/etc/php/conf.d{sep}/c/a"))
    );
    assert_eq!(
        vars[1],
        (
            "HUSHRUN_PHP_PREPEND".to_string(),
            OsString::from("/srv/prepend.php")
        )
    );
    let dir = tempfile::tempdir().unwrap();
    let installed = install(dir.path()).unwrap();
    let ini = std::fs::read_to_string(installed.join("zz-hushrun-phpunit.ini")).unwrap();
    assert!(ini.contains("auto_prepend_file"), "{ini}");
    assert!(ini.contains("hushrun_phpunit_prepend.php"), "{ini}");
    assert!(installed.join("hushrun_phpunit.php").is_file());
    assert_eq!(install(dir.path()).unwrap(), installed);
}

fn tree_of(paths: &[&str]) -> units::Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut paths = paths.to_vec();
    paths.sort();
    for path in paths {
        bytes.extend(format!("f {} {path}\0", "0".repeat(64)).as_bytes());
    }
    units::Tree::from_manifest(&bytes).unwrap()
}

fn facts(value: serde_json::Value) -> FactsFile {
    serde_json::from_value(value).unwrap()
}

// covers: C-90, C-92
#[test]
fn what_phpunit_wrote_comes_to_one_unit_each() {
    let tree = tree_of(&[
        "src/Cart.php",
        "src/Pricing.php",
        "tests/CartTest.php",
        "tests/GreetTest.php",
    ]);
    let file = facts(serde_json::json!({
        "schema": "hushrun.unit-facts/v1",
        "adapter": "phpunit",
        "failuresByUnit": true,
        "settings": {
            "testFiles": ["tests/CartTest.php", "tests/GreetTest.php"],
            "sequence": ["vendor/autoload.php", "tests/CartTest.php", "tests/GreetTest.php",
                         "src/Cart.php", "src/Pricing.php", "src/Greet.php"],
            "readTo": {"tests/CartTest.php": 5},
            "byName": ["tests/CartTest.php", "tests/GreetTest.php"]
        },
        "units": [
            {"unit": "tests/CartTest.php", "selected": 2, "finished": 2, "failed": true,
             "reads": ["tests/CartTest.php"],
             "tests": [
                {"id": "CartTest::testTotal", "finished": true, "failed": true},
                {"id": "CartTest::testEmpty", "finished": true}
             ]},
            {"unit": "tests/GreetTest.php", "selected": 0, "finished": 0, "deselected": true,
             "tests": [{"id": "GreetTest::testHello", "deselected": true}]}
        ]
    }));
    let nothing = || "nothing".to_string();
    let seen = observe(std::slice::from_ref(&file), &tree, false, &nothing).unwrap();
    assert!(seen.failures_by_unit);
    assert_eq!(seen.unit_files.len(), 2);
    let cart = &seen.units["tests/CartTest.php"];
    assert!(cart.facts.failed && cart.facts.complete);
    // What isn't a file of the scope (vendor) is no read.
    assert_eq!(
        cart.reads.iter().map(String::as_str).collect::<Vec<_>>(),
        ["src/Cart.php", "src/Pricing.php", "tests/CartTest.php"]
    );
    assert_eq!(cart.tests.as_ref().unwrap().len(), 2);
    assert!(seen.units["tests/GreetTest.php"].facts.deselected);
    // Another test file is read where `related` names it; what a unit `readTo` doesn't name
    // reads the whole sequence.
    let mut naming = file.clone();
    naming.settings.insert(
        "related".into(),
        serde_json::json!({"tests/CartTest.php": ["tests/GreetTest.php"]}),
    );
    naming.settings.remove("readTo");
    let seen = observe(&[naming], &tree, true, &nothing).unwrap();
    assert_eq!(
        seen.units["tests/CartTest.php"]
            .reads
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "src/Cart.php",
            "src/Pricing.php",
            "tests/CartTest.php",
            "tests/GreetTest.php"
        ]
    );

    // A run that failed with no test file failing: what failed no test explains.
    let mut quiet = file.clone();
    quiet.units[0].failed = false;
    for test in quiet.units[0].tests.as_mut().unwrap() {
        test.failed = false;
    }
    let seen = observe(&[quiet.clone()], &tree, false, &nothing).unwrap();
    assert!(!seen.failures_by_unit);
    let seen = observe(&[quiet], &tree, true, &nothing).unwrap();
    assert!(seen.failures_by_unit);

    assert_eq!(observe(&[], &tree, true, &nothing).unwrap_err(), "nothing");
    let mut broken = file.clone();
    broken.errors = vec!["hushrun's PHPUnit adapter failed: x".into()];
    assert!(
        observe(&[broken], &tree, true, &nothing)
            .unwrap_err()
            .contains("failed: x")
    );
    assert!(observe(&[file.clone(), file], &tree, true, &nothing).is_err());
}

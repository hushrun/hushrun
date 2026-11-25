use super::*;

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

// covers: C-110
#[test]
fn xcodebuild_test_is_the_adapters_and_what_goes_round_its_program_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let direct = recognize_command(&words("xcodebuild -scheme App test"), cwd);
    let by_path = recognize_command(&words("/usr/bin/xcodebuild test"), cwd);
    let xcrun = recognize_command(&words("xcrun xcodebuild test"), cwd);
    if cfg!(target_os = "macos") {
        assert_eq!(direct, Some(Ok(Some(words("-scheme App test")))));
    } else {
        assert_eq!(direct, Some(Err(NOT_MACOS.to_string())));
    }
    assert!(matches!(by_path, Some(Err(_))));
    assert!(matches!(xcrun, Some(Err(_))));
    assert!(matches!(
        recognize_command(&words("xcodebuild test-without-building"), cwd),
        Some(Err(_))
    ));
    // Not a run of the tests: not the adapter's.
    assert_eq!(recognize_command(&words("xcodebuild build"), cwd), None);
    assert_eq!(recognize_command(&words("xcodebuild -list"), cwd), None);
    assert_eq!(recognize_command(&words("swift test"), cwd), None);
    // A wrapper whose text starts xcodebuild's tests and no other runner.
    let wrapper = vec![
        "sh".to_string(),
        "-c".into(),
        "xcodebuild -scheme A test | xcpretty".into(),
    ];
    let found = recognize_command(&wrapper, cwd);
    if cfg!(target_os = "macos") {
        assert_eq!(found, Some(Ok(None)));
    } else {
        assert!(matches!(found, Some(Err(_))));
    }
    let both = vec![
        "sh".to_string(),
        "-c".into(),
        "swift test && xcodebuild test".into(),
    ];
    assert_eq!(recognize_command(&both, cwd), None);
}

// covers: C-110
#[test]
fn a_workspace_names_its_projects_and_packages_through_its_groups() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Packages/Kit")).unwrap();
    std::fs::write(dir.path().join("Packages/Kit/Package.swift"), "").unwrap();
    let ws = dir.path().join("App.xcworkspace");
    let data = r#"<?xml version="1.0" encoding="UTF-8"?>
<Workspace
   version = "1.0">
   <FileRef
      location = "group:App/App.xcodeproj">
   </FileRef>
   <Group
      location = "group:Libs"
      name = "Libs">
      <FileRef
         location = "group:Core.xcodeproj">
      </FileRef>
   </Group>
   <FileRef
      location = "container:Packages/Kit">
   </FileRef>
   <FileRef
      location = "group:README.md">
   </FileRef>
</Workspace>
"#;
    let (projects, packages) = workspace_refs(data, &ws);
    assert_eq!(
        projects,
        vec![
            dir.path().join("App/App.xcodeproj"),
            dir.path().join("Libs/Core.xcodeproj")
        ]
    );
    assert_eq!(packages, vec![dir.path().join("Packages/Kit")]);
}

// covers: C-110
#[test]
fn the_project_is_the_one_commands_folder_holds_unless_it_names_one() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    assert!(containers(&parse(&["test"]), cwd).is_err());
    std::fs::write(cwd.join("Package.swift"), "").unwrap();
    assert_eq!(
        containers(&parse(&["test"]), cwd),
        Ok(Container::Package(cwd.to_path_buf()))
    );
    std::fs::create_dir(cwd.join("App.xcodeproj")).unwrap();
    assert_eq!(
        containers(&parse(&["test"]), cwd),
        Ok(Container::Projects {
            projects: vec![cwd.join("App.xcodeproj")],
            packages: vec![]
        })
    );
    std::fs::create_dir(cwd.join("Other.xcodeproj")).unwrap();
    assert!(containers(&parse(&["test"]), cwd).is_err());
    assert_eq!(
        containers(&parse(&["-project", "Other.xcodeproj", "test"]), cwd),
        Ok(Container::Projects {
            projects: vec![cwd.join("Other.xcodeproj")],
            packages: vec![]
        })
    );
}

fn plan() -> PlanFile {
    let unit = |target: &str, container: &str| PlannedUnit {
        target: target.into(),
        container: container.into(),
    };
    PlanFile {
        schema: PLAN_SCHEMA.into(),
        units: [
            (
                "ios/App.xcodeproj/AppTests".to_string(),
                unit("AppTests", "ios/App.xcodeproj"),
            ),
            (
                "ios/App.xcodeproj/KitTests".to_string(),
                unit("KitTests", "ios/App.xcodeproj"),
            ),
            (
                "ios/Other.xcodeproj/OtherTests".to_string(),
                unit("OtherTests", "ios/Other.xcodeproj"),
            ),
        ]
        .into_iter()
        .collect(),
    }
}

fn run_file(reuse: &[&str], tests: &[(&str, &[&str])]) -> RunFile {
    serde_json::from_value(serde_json::json!({
        "schema": facts::RUN_SCHEMA,
        "root": "/work",
        "reuse": reuse,
        "unit": "test",
        "reuseTests": tests.iter().map(|(u, ids)| (u.to_string(), ids.to_vec())).collect::<BTreeMap<_, _>>(),
    }))
    .unwrap()
}

// covers: C-112
#[test]
fn only_units_of_the_projects_this_start_tests_are_left_out() {
    let run = run_file(
        &[
            "ios/App.xcodeproj/KitTests",
            "ios/Other.xcodeproj/OtherTests",
        ],
        &[(
            "ios/App.xcodeproj/AppTests",
            &["AppTests/testA()", "Suite/b()"],
        )],
    );
    let ours: BTreeSet<String> = ["ios/App.xcodeproj".to_string()].into_iter().collect();
    let left = leave_out(&run, &plan(), &ours);
    assert_eq!(
        left.args,
        vec![
            "-skip-testing:KitTests",
            "-skip-testing:AppTests/AppTests/testA()",
            "-skip-testing:AppTests/Suite/b()"
        ]
    );
    assert!(left.units.contains("ios/App.xcodeproj/KitTests"));
    assert!(!left.units.contains("ios/Other.xcodeproj/OtherTests"));
    // Nothing of a project this start doesn't test, nor of a unit the plan doesn't know.
    let none = leave_out(&run, &plan(), &BTreeSet::new());
    assert!(none.args.is_empty());
}

// covers: C-112
#[test]
fn a_target_name_two_projects_share_is_never_left_out() {
    let mut p = plan();
    p.units
        .get_mut("ios/Other.xcodeproj/OtherTests")
        .unwrap()
        .target = "KitTests".into();
    let run = run_file(&["ios/App.xcodeproj/KitTests"], &[]);
    let ours: BTreeSet<String> = ["ios/App.xcodeproj".to_string()].into_iter().collect();
    assert!(leave_out(&run, &p, &ours).args.is_empty());
}

fn results_of(json: &str) -> results::Results {
    results::read(json.as_bytes()).unwrap()
}

// covers: C-112, C-113
#[test]
fn each_test_of_the_bundle_is_told_and_those_left_out_are_deselected() {
    let results = results_of(include_str!(
        "../../../../../testdata/xcodebuild/test-results.json"
    ));
    let mut p = plan();
    p.units.insert(
        "ios/Lib.xcodeproj/LibTests".into(),
        PlannedUnit {
            target: "LibTests".into(),
            container: "ios/Lib.xcodeproj".into(),
        },
    );
    let left = LeftOut {
        units: ["ios/App.xcodeproj/KitTests".to_string()]
            .into_iter()
            .collect(),
        tests: [(
            "ios/Lib.xcodeproj/LibTests".to_string(),
            ["LibTests/testGone()".to_string()].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        args: vec!["-skip-testing:x".into()],
    };
    let tested: BTreeSet<String> = [
        "ios/Lib.xcodeproj".to_string(),
        "ios/App.xcodeproj".to_string(),
    ]
    .into_iter()
    .collect();
    let file = facts_of(&results, &left, Some(&p), &tested, Some(65), true);
    // A test failed, so the failure is told by unit.
    assert!(file.failures_by_unit);
    let lib = file
        .units
        .iter()
        .find(|u| u.unit == "ios/Lib.xcodeproj/LibTests")
        .unwrap();
    let tests = lib.tests.as_ref().unwrap();
    let get = |id: &str| tests.iter().find(|t| t.id == id).unwrap();
    assert!(get("LibTests/testLib()").finished && !get("LibTests/testLib()").failed);
    assert!(get("LibTests/testOther()").failed);
    assert!(get("LibTests/testSkipped()").skipped);
    assert!(get("LibTests/testGone()").deselected);
    assert!(lib.failed && !lib.deselected);
    assert_eq!(lib.selected, 4);
    let kit = file
        .units
        .iter()
        .find(|u| u.unit == "ios/App.xcodeproj/KitTests")
        .unwrap();
    assert!(kit.deselected);
    // A target the plan doesn't know isn't told.
    assert_eq!(file.units.len(), 2);
    // Nor one of a project this start didn't test.
    let only_app: BTreeSet<String> = ["ios/App.xcodeproj".to_string()].into_iter().collect();
    let file = facts_of(&results, &left, Some(&p), &only_app, Some(65), true);
    assert!(
        file.units
            .iter()
            .all(|u| u.unit != "ios/Lib.xcodeproj/LibTests")
    );
}

// covers: C-112
#[test]
fn a_run_that_failed_with_no_test_failing_keeps_no_pass() {
    let results = results_of(r#"{"testNodes": []}"#);
    let none = BTreeSet::new();
    let file = facts_of(
        &results,
        &LeftOut::default(),
        Some(&plan()),
        &none,
        Some(65),
        true,
    );
    assert!(!file.failures_by_unit);
    let passed = facts_of(
        &results,
        &LeftOut::default(),
        Some(&plan()),
        &none,
        Some(0),
        true,
    );
    assert!(passed.failures_by_unit);
}

// covers: C-110
#[test]
fn xcodes_version_is_read_from_its_first_line() {
    assert_eq!(xcode_major("Xcode 26.1\nBuild version 17B55\n"), Some(26));
    assert_eq!(xcode_major("Xcode 16.4\n"), Some(16));
    assert_eq!(xcode_major("nonsense"), None);
}

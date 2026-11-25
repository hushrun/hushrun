//! \[ADR 0121\] What an `xcodebuild` command line asks: its actions, the project or workspace and
//! scheme it names, and the options that decide whether hushrun can observe it (C-110).

/// The options that take the next word as their value.
const VALUE_OPTS: &[&str] = &[
    "-project",
    "-target",
    "-workspace",
    "-scheme",
    "-configuration",
    "-xcconfig",
    "-arch",
    "-sdk",
    "-toolchain",
    "-destination",
    "-destination-timeout",
    "-jobs",
    "-resultBundlePath",
    "-resultBundleVersion",
    "-resultStreamPath",
    "-derivedDataPath",
    "-archivePath",
    "-exportOptionsPlist",
    "-exportPath",
    "-xctestrun",
    "-testPlan",
    "-only-testing",
    "-skip-testing",
    "-only-test-configuration",
    "-skip-test-configuration",
    "-test-iterations",
    "-maximum-concurrent-test-device-destinations",
    "-maximum-concurrent-test-simulator-destinations",
    "-parallel-testing-enabled",
    "-parallel-testing-worker-count",
    "-maximum-parallel-testing-workers",
    "-test-timeouts-enabled",
    "-default-test-execution-time-allowance",
    "-maximum-test-execution-time-allowance",
    "-enableCodeCoverage",
    "-enableAddressSanitizer",
    "-enableThreadSanitizer",
    "-enableUndefinedBehaviorSanitizer",
    "-testLanguage",
    "-testRegion",
    "-collect-test-diagnostics",
    "-packageCachePath",
    "-clonedSourcePackagesDirPath",
    "-packageDependencySchedulerPolicy",
    "-scmProvider",
    "-authenticationKeyPath",
    "-authenticationKeyID",
    "-authenticationKeyIssuerID",
    "-localizationPath",
    "-exportLanguage",
    "-defaultPackageRegistryURL",
    "-packageAuthorizationProvider",
    "-test-enumeration-style",
    "-test-enumeration-format",
    "-test-enumeration-output-path",
    "-testProductsPath",
    "-runFirstLaunchTimeoutSeconds",
    "-showBuildSettingsForIndex",
    "-buildSettingsForIndex",
];

/// The actions `xcodebuild` takes as bare words.
const ACTIONS: &[&str] = &[
    "build",
    "build-for-testing",
    "analyze",
    "archive",
    "test",
    "test-without-building",
    "docbuild",
    "installsrc",
    "install",
    "clean",
];

/// Options that ask for something other than a run of the tests: such a start passes through.
const PASSTHROUGH: &[&str] = &[
    "-list",
    "-version",
    "-help",
    "-usage",
    "-showsdks",
    "-showBuildSettings",
    "-showdestinations",
    "-showTestPlans",
    "-enumerate-tests",
    "-exportArchive",
    "-exportNotarizedApp",
    "-exportLocalizations",
    "-importLocalizations",
    "-checkFirstLaunchStatus",
    "-runFirstLaunch",
    "-downloadAllPlatforms",
    "-downloadPlatform",
    "-downloadComponent",
    "-importPlatform",
    "-create-xcframework",
];

/// What one `xcodebuild` start asks, as hushrun reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The actions, in order.
    pub actions: Vec<String>,
    pub project: Option<String>,
    pub workspace: Option<String>,
    pub scheme: Option<String>,
    /// The result bundle COMMAND names, when it names one.
    pub result_bundle: Option<String>,
    /// COMMAND chooses tests itself (`-only-testing`, `-skip-testing`, a test plan).
    pub chooses: bool,
    /// It asks for no run of the tests: a listing, a version, its help. Started as it is.
    pub passthrough: bool,
    /// Why hushrun can't observe it, said once: nothing is left out or recorded.
    pub refused: Option<String>,
}

impl Parsed {
    /// Whether it runs tests: `test` among its actions.
    pub fn tests(&self) -> bool {
        self.actions
            .iter()
            .any(|a| a == "test" || a == "test-without-building")
    }
}

/// Reads `args`, what follows `xcodebuild`.
pub fn parse<S: AsRef<str>>(args: &[S]) -> Parsed {
    let mut out = Parsed::default();
    let mut i = 0;
    let refuse = |out: &mut Parsed, why: String| {
        out.refused.get_or_insert(why);
    };
    while i < args.len() {
        let word = args[i].as_ref();
        i += 1;
        let (name, inline) = match word.split_once(':') {
            Some((n, v)) if n.starts_with('-') => (n, Some(v.to_string())),
            _ => match word.split_once('=') {
                Some((n, v)) if n.starts_with('-') => (n, Some(v.to_string())),
                _ => (word, None),
            },
        };
        if ACTIONS.contains(&word) {
            out.actions.push(word.to_string());
            continue;
        }
        if PASSTHROUGH.contains(&name) {
            out.passthrough = true;
            continue;
        }
        let value = if VALUE_OPTS.contains(&name) {
            match inline {
                Some(v) => Some(v),
                None => {
                    let v = args.get(i).map(|v| v.as_ref().to_string());
                    i += 1;
                    v
                }
            }
        } else {
            None
        };
        match name {
            "-project" => out.project = value,
            "-workspace" => out.workspace = value,
            "-scheme" => out.scheme = value,
            "-resultBundlePath" => out.result_bundle = value,
            "-only-testing" | "-skip-testing" | "-testPlan" | "-only-test-configuration"
            | "-skip-test-configuration" => out.chooses = true,
            "-xctestrun" => refuse(
                &mut out,
                "xcodebuild -xctestrun runs test products an earlier build left, which may not be the files now".into(),
            ),
            "-test-iterations" | "-retry-tests-on-failure" | "-run-tests-until-failure" => refuse(
                &mut out,
                format!("xcodebuild {name} runs a test more than once, and a pass may have come after a failure"),
            ),
            _ => {}
        }
    }
    if out.actions.iter().any(|a| a == "test-without-building") {
        refuse(
            &mut out,
            "xcodebuild test-without-building runs the test products an earlier build left, which may not be the files now".into(),
        );
    }
    out
}

/// `-skip-testing:` values for the targets left out whole and the tests left out one by one,
/// each `Target/Suite/test()`; literal, as `xcodebuild` matches them.
pub fn skip_testing(targets: &[String], tests: &[(String, String)]) -> Vec<String> {
    targets
        .iter()
        .map(|t| format!("-skip-testing:{t}"))
        .chain(
            tests
                .iter()
                .map(|(target, id)| format!("-skip-testing:{target}/{id}")),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-110
    #[test]
    fn a_test_run_is_read_with_its_project_scheme_and_bundle() {
        let p = parse(&[
            "-project",
            "App.xcodeproj",
            "-scheme",
            "App",
            "-destination",
            "platform=iOS Simulator,name=iPhone 17",
            "test",
            "-resultBundlePath",
            "out/r.xcresult",
            "CODE_SIGNING_ALLOWED=NO",
        ]);
        assert_eq!(p.actions, vec!["test"]);
        assert_eq!(p.project.as_deref(), Some("App.xcodeproj"));
        assert_eq!(p.scheme.as_deref(), Some("App"));
        assert_eq!(p.result_bundle.as_deref(), Some("out/r.xcresult"));
        assert!(p.tests() && !p.chooses && !p.passthrough && p.refused.is_none());
    }

    // covers: C-110
    #[test]
    fn a_value_that_is_an_action_word_isnt_an_action() {
        let p = parse(&["-scheme", "test", "build"]);
        assert_eq!(p.actions, vec!["build"]);
        assert!(!p.tests());
    }

    // covers: C-110
    #[test]
    fn choosing_tests_listing_and_what_cant_be_observed_are_told_apart() {
        assert!(parse(&["test", "-only-testing:AppTests"]).chooses);
        assert!(parse(&["test", "-skip-testing", "AppTests/A"]).chooses);
        assert!(parse(&["test", "-testPlan", "Fast"]).chooses);
        assert!(parse(&["test", "-enumerate-tests"]).passthrough);
        assert!(parse(&["-list"]).passthrough);
        assert!(parse(&["test-without-building"]).refused.is_some());
        assert!(
            parse(&["test", "-xctestrun", "x.xctestrun"])
                .refused
                .is_some()
        );
        assert!(parse(&["test", "-test-iterations", "3"]).refused.is_some());
        assert!(
            parse(&["test", "-retry-tests-on-failure"])
                .refused
                .is_some()
        );
        assert!(
            parse(&["test", "-run-tests-until-failure"])
                .refused
                .is_some()
        );
        assert!(parse(&["build", "test"]).refused.is_none());
    }

    // covers: C-112
    #[test]
    fn what_is_left_out_is_named_as_xcodebuild_matches_it() {
        assert_eq!(
            skip_testing(
                &["LibTests".into()],
                &[("AppTests".into(), "AppTests/testA()".into())]
            ),
            vec![
                "-skip-testing:LibTests".to_string(),
                "-skip-testing:AppTests/AppTests/testA()".to_string()
            ]
        );
    }
}

//! \[ADR 0120\] Catch2 3, for a test program COMMAND starts itself: what its arguments and
//! environment let hushrun do, its listing, the test spec that leaves tests out, and its JUnit
//! file.
//!
//! Catch2 3 reads Bazel's test variables when `BAZEL_TEST` is set: `XML_OUTPUT_FILE` adds a JUnit
//! reporter beside the one COMMAND chose, and `TESTBRIDGE_TEST_ONLY` takes the place of the test
//! spec of the command line. Without `TEST_SHARD_INDEX`, `TEST_TOTAL_SHARDS` and
//! `TEST_SHARD_STATUS_FILE` it prints a warning for each, so hushrun sets all three: one shard of
//! one, the whole program. A test's id is its name.

use super::{Case, Outcome};

/// Options that take a value, as a word of their own when no `=` joins it.
const VALUED: &[&str] = &[
    "-o",
    "--out",
    "-r",
    "--reporter",
    "-n",
    "--name",
    "-d",
    "--durations",
    "-D",
    "--min-duration",
    "-f",
    "--input-file",
    "-c",
    "--section",
    "-w",
    "--warn",
    "--order",
    "--rng-seed",
    "--colour-mode",
    "--use-colour",
    "-v",
    "--verbosity",
    "--benchmark-samples",
    "--benchmark-resamples",
    "--benchmark-confidence-interval",
    "--benchmark-warmup-time",
    "--wait-for-keypress",
    "--shard-count",
    "--shard-index",
    "-x",
    "--abortx",
];

/// What COMMAND's own arguments let the adapter do: `Err` why it can't observe the program at
/// all, `Ok(Some)` why it observes it and leaves nothing out.
pub fn args(args: &[String]) -> Result<Option<String>, String> {
    let mut observe = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            "-c" | "--section" => {
                return Err(format!(
                    "COMMAND runs one section of a test ({arg}), which can't be told from the whole test"
                ));
            }
            "-l"
            | "--list-tests"
            | "--list-tags"
            | "--list-reporters"
            | "--list-listeners"
            | "--list-test-names-only"
            | "-h"
            | "-?"
            | "--help"
            | "--libidentify" => {
                return Err(format!("COMMAND runs no test ({arg})"));
            }
            "-f" | "--input-file" => {
                observe = Some(format!(
                    "COMMAND names its tests in a file ({arg}), which the TESTBRIDGE_TEST_ONLY hushrun would leave tests out with takes the place of"
                ));
            }
            "--shard-count" | "--shard-index" => {
                observe = Some(format!(
                    "COMMAND runs one shard of the program's tests ({arg}), and leaving any out would move the others between shards"
                ));
            }
            _ if !arg.starts_with('-') => {
                observe = Some(format!(
                    "COMMAND names its own tests ({arg}), which the TESTBRIDGE_TEST_ONLY hushrun would leave tests out with takes the place of"
                ));
            }
            _ => {}
        }
        i += 1 + usize::from(VALUED.contains(&arg));
    }
    Ok(observe)
}

/// The variables hushrun sets for Catch2, which COMMAND's environment must not have already.
pub const SET: &[&str] = &[
    "BAZEL_TEST",
    "XML_OUTPUT_FILE",
    "TESTBRIDGE_TEST_ONLY",
    "TEST_SHARD_INDEX",
    "TEST_TOTAL_SHARDS",
    "TEST_SHARD_STATUS_FILE",
];

/// What COMMAND's environment lets the adapter do, as [`args`] says it.
pub fn env(get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Result<Option<String>, String> {
    match SET.iter().find(|name| get(name).is_some()) {
        Some(name) => Err(format!(
            "{name} is set: the program runs under Bazel's test variables, which hushrun would set"
        )),
        None => Ok(None),
    }
}

/// COMMAND's arguments for a listing: without the reporters and output files it names, which a
/// listing would write.
pub fn listing_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let name = arg.split('=').next().unwrap_or(arg);
        let valued = VALUED.contains(&arg);
        if !matches!(name, "-o" | "--out" | "-r" | "--reporter") {
            out.push(args[i].clone());
            if valued && let Some(value) = args.get(i + 1) {
                out.push(value.clone());
            }
        }
        i += 1 + usize::from(valued);
    }
    out.extend(["--list-tests", "--reporter", "xml"].map(str::to_string));
    out
}

/// The names `--list-tests --reporter xml` listed, in order.
pub fn listing(stdout: &[u8]) -> Result<Vec<String>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let text = std::str::from_utf8(stdout).map_err(|_| "it isn't UTF-8")?;
    let start = text.find("<?xml").ok_or("it holds no XML")?;
    let mut reader = Reader::from_str(&text[start..]);
    reader.config_mut().check_end_names = true;
    let mut out = Vec::new();
    let mut depth_case = false;
    let mut in_name = false;
    let mut name = String::new();
    let mut matching = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("it doesn't parse: {err}"))?;
        match event {
            Event::Start(e) if e.local_name().as_ref() == "MatchingTests" => matching = true,
            Event::Empty(e) if e.local_name().as_ref() == "MatchingTests" => matching = true,
            Event::Start(e) if e.local_name().as_ref() == "TestCase" => depth_case = true,
            Event::End(e) if e.local_name().as_ref() == "TestCase" => depth_case = false,
            Event::Start(e) if depth_case && e.local_name().as_ref() == "Name" => {
                in_name = true;
                name.clear();
            }
            Event::Text(t) if in_name => name.push_str(&t.xml10_content()),
            Event::CData(t) if in_name => name.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_name => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
                        .ok_or("it names an entity XML doesn't define")?
                        .to_string(),
                };
                name.push_str(&resolved);
            }
            Event::End(e) if in_name && e.local_name().as_ref() == "Name" => {
                in_name = false;
                out.push(std::mem::take(&mut name));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !matching {
        return Err("it holds no <MatchingTests>".into());
    }
    Ok(out)
}

/// The `TESTBRIDGE_TEST_ONLY` that leaves out `names`: each excluded, quoted, with Catch2's own
/// characters escaped.
pub fn spec(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| {
            let mut out = String::from("~\"");
            for c in name.chars() {
                if matches!(c, '\\' | ',' | '"' | '[' | ']' | '~' | '*') {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a name can be named in a spec exactly: no line break, and no space at either end,
/// which Catch2 trims.
pub fn nameable(name: &str) -> bool {
    !name.contains(['\n', '\r']) && name.trim() == name && !name.is_empty()
}

/// What Catch2's JUnit file says of each leaf section it reported, in order, each `id` the
/// section's path (`test/section/…`); `listed` then maps them to tests. A `<failure>` or
/// `<error>` failed; `<skipped>` was skipped; any other passed. A test that made no assertion
/// and printed nothing has no case: it reads as not run.
pub fn cases(bytes: &[u8]) -> Result<Vec<Case>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8")?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut cases = Vec::new();
    let mut open: Option<Case> = None;
    let mut suites = 0;
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("it doesn't parse: {err}"))?;
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "testsuites" => {
                suites += 1;
            }
            Event::Start(e) if e.local_name().as_ref() == "testcase" => open = Some(case(&e)?),
            Event::Empty(e) if e.local_name().as_ref() == "testcase" => cases.push(case(&e)?),
            Event::Start(e) | Event::Empty(e) => {
                if let Some(case) = &mut open {
                    match e.local_name().as_ref() {
                        "failure" | "error" => case.outcome = Outcome::Failed,
                        "skipped" if case.outcome != Outcome::Failed => {
                            case.outcome = Outcome::Skipped;
                        }
                        _ => {}
                    }
                }
            }
            Event::End(e) if e.local_name().as_ref() == "testcase" => cases.extend(open.take()),
            Event::Eof => break,
            _ => {}
        }
    }
    if suites == 0 {
        return Err("it holds no <testsuites>".into());
    }
    Ok(cases)
}

fn case(e: &quick_xml::events::BytesStart<'_>) -> Result<Case, String> {
    Ok(Case {
        id: super::attribute(e, "name")?.unwrap_or_default(),
        outcome: Outcome::Passed,
        duration_ms: super::millis(super::attribute(e, "time")?.as_deref()),
    })
}

/// Each case's test: the longest listed name that is its path or starts it, followed by `/`.
pub fn test_of<'a>(case: &str, listed: &'a [String]) -> Option<&'a str> {
    listed
        .iter()
        .filter(|name| {
            case == name.as_str()
                || case
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|name| name.len())
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-107
    #[test]
    fn its_own_arguments_say_what_can_be_done() {
        assert_eq!(args(&words("-r console -o out.txt --order rand")), Ok(None));
        assert!(matches!(args(&words("[fast]")), Ok(Some(_))));
        assert!(matches!(args(&words("--shard-count 2")), Ok(Some(_))));
        assert!(args(&words("-c section")).is_err());
        assert!(args(&words("--list-tests")).is_err());
        assert_eq!(
            listing_args(&words("-r console -o out.txt --order rand -s")),
            words("--order rand -s --list-tests --reporter xml")
        );
        let set = |n: &str| (n == "BAZEL_TEST").then(|| "1".into());
        assert!(env(&set).is_err());
    }

    // covers: C-107
    #[test]
    fn the_spec_excludes_each_name_exactly() {
        assert_eq!(
            spec(&["alpha one", "beta, with \"q\" [x] ~ * \\"]),
            r#"~"alpha one" ~"beta\, with \"q\" \[x\] \~ \* \\""#
        );
        assert!(!nameable(" lead"));
        assert!(nameable("a b"));
    }

    // covers: C-107
    #[test]
    fn the_listing_and_the_junit_file_are_read() {
        let listed = br#"Randomness seeded to: 1
<?xml version="1.0" encoding="UTF-8"?>
<MatchingTests>
  <TestCase><Name>alpha &amp; one</Name><ClassName/><Tags/></TestCase>
  <TestCase><Name>gamma</Name><ClassName/><Tags/></TestCase>
  <TestCase><Name>gamma/x</Name><ClassName/><Tags/></TestCase>
</MatchingTests>"#;
        let names = listing(listed).unwrap();
        assert_eq!(names, ["alpha & one", "gamma", "gamma/x"]);
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="t_c" errors="0" failures="1" skipped="1" tests="4">
    <testcase classname="t_c.global" name="alpha &amp; one" time="0.001" status="run"/>
    <testcase classname="t_c.global" name="gamma/s1" time="0.000" status="run">
      <failure message="x" type="REQUIRE">at c.cpp:4</failure>
    </testcase>
    <testcase classname="t_c.global" name="gamma/x" time="0.000" status="run">
      <skipped type="SKIP">nope</skipped>
    </testcase>
  </testsuite>
</testsuites>"#;
        let got = cases(xml).unwrap();
        assert_eq!(got[0].outcome, Outcome::Passed);
        assert_eq!(got[1].outcome, Outcome::Failed);
        assert_eq!(got[2].outcome, Outcome::Skipped);
        assert_eq!(test_of("gamma/s1", &names), Some("gamma"));
        assert_eq!(test_of("gamma/x", &names), Some("gamma/x"));
        assert_eq!(test_of("alpha & one", &names), Some("alpha & one"));
        assert_eq!(test_of("gammas", &names), None);
    }
}

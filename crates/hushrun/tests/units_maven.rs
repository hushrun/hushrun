//! \[ADR 0088\] Test classes with Maven (C-47): a unit is a test class of a reactor's module, what
//! it read is every source below the roots of its module and of the modules it depends on, as
//! Maven's own model says, and reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- mvn …` would, through Maven's adapter's own type, `maven::Recognized`, and the code
//! every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`). Maven
//! itself runs as COMMAND, for real. What only the CLI shows of Maven (its lines, the run's last
//! line, the audit's suspension, `hushrun why`) is in `e2e_maven.rs`.
//!
//! The fixtures are generated reactors (`common/maven.rs`) that need Maven 3.9 or later
//! (`HUSHRUN_TEST_MAVEN`, the path of its `mvn`), a JDK, and a local repository that already holds
//! every plugin and library they use (`HUSHRUN_TEST_MAVEN_REPO`): Maven runs offline, reading that
//! repository as a tail below one of its own for each test, which it never writes to. With
//! `HUSHRUN_TEST_MAVEN_ONLINE=1` it runs online and fills that repository instead, which is how
//! CI fills it once before its test run. The tests say so and pass where Maven is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/maven.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::{BTreeMap, BTreeSet};

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, maven};

use fixtures::*;
use in_process::Ran;

/// What Maven's adapter answers that never changes in a run, for the code every adapter shares.
fn profile(adapter: &maven::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
        not_by_test: adapter.not_by_test(),
        not_watched: adapter.not_watched(),
    }
}

/// `hushrun run unit -- mvn … ARGS`, with `env` besides, in-process, through Maven's adapter
/// alone: the tests' Maven, offline ([`Reactor::mvn`]).
fn run(r: &Reactor, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let words = r.mvn(args);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    run_command(r, &words, env)
}

/// `hushrun run unit -- COMMAND`, with `env` besides, in-process, through Maven's adapter alone.
fn run_command(r: &Reactor, command: &[&str], env: &[(&str, &str)]) -> Ran {
    let words: Vec<String> = command.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Maven's adapter (`maven::recognized`), with no
    // trait object: what COMMAND is.
    let adapter = match maven::recognize(hushrun::units::command::without_env(&words)) {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", command, env).unobserved(&why),
        None => panic!("{command:?} doesn't start Maven"),
    };
    observe(r, command, env, &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order. Every call is on `maven::Recognized` itself, which the static map resolves to its
/// own `impl Adapter`; through a trait object or a generic it would reach every adapter
/// (CONTRIBUTING.md, "Which tests CI runs again"). It is `units_gradle.rs`'s, on Maven's type.
fn observe(r: &Reactor, args: &[&str], env: &[(&str, &str)], adapter: &maven::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // Maven's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

// covers: C-30, C-31, C-47
#[test]
fn maven_test_classes_read_their_modules_and_the_modules_they_depend_on() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("observe"), maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(
        first.line("unit: with `tests: reuse`"),
        "unit: with `tests: reuse`, all 6 test classes would have run: none had passed here yet on the files it read"
    );
    let map = r.map();
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit}: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    // A class reads every source below its module's roots and the main roots of the modules of
    // the reactor its module depends on; never an unrelated module's.
    assert_eq!(
        reads(CART_TEST),
        [
            CART_TEST,
            "app/src/main/java/shop/app/Cart.java",
            "lib/src/main/java/shop/lib/Price.java"
        ]
        .map(String::from)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    assert_eq!(
        reads(GREET_TEST),
        [
            "other/src/main/java/shop/other/Greet.java",
            "other/src/test/java/shop/other/ByeTest.java",
            GREET_TEST,
            "other/src/test/java/shop/other/WaveTest.java"
        ]
    );
    assert!(reads(PRICE_TEST).contains(&ROUND_TEST.to_string()));
    let dirs = &map["units"][CART_TEST]["dirs"];
    assert!(
        dirs.as_array()
            .unwrap()
            .iter()
            .any(|d| d == "lib/src/main/java"),
        "{dirs}"
    );
    // POMs are the rest: a change to one runs everything.
    assert!(map["rest"]["pom.xml"].is_string(), "{map:#}");
    assert!(map["rest"]["app/pom.xml"].is_string(), "{map:#}");
    let sources: Vec<&str> = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        sources.contains(&"lib/src/main/java/shop/lib/Price.java"),
        "{sources:?}"
    );
    assert_eq!(
        first.units,
        serde_json::json!({"total": 6, "ran": 6, "reused": 0})
    );
}

// covers: C-34, C-47
#[test]
fn a_change_runs_the_classes_of_its_module_and_of_the_modules_that_depend_on_it_only() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("reuse"), maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(
        classes(&first.stdout).len(),
        6,
        "{:?}",
        classes(&first.stdout)
    );

    // A change in `other` runs its classes, never lib's or app's, but the one of those three
    // the audit runs.
    r.write(
        "other/src/main/java/shop/other/Greet.java",
        "package shop.other;\n\npublic class Greet {\n  public static String hello() { return \"h\" + \"i\"; }\n}\n",
    );
    let other = run(&r, &["test"], &[]);
    assert_eq!(other.code, 0, "{}", other.said());
    assert_eq!(
        other.line("unit: "),
        "unit: 4 of 6 test classes run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    at_most_the_audit(
        &classes(&other.stdout),
        &[
            "shop.other.ByeTest",
            "shop.other.GreetTest",
            "shop.other.WaveTest",
        ],
    );
    let units = other.units.clone();
    assert_eq!(
        units,
        serde_json::json!({"total": 6, "ran": 4, "reused": 2, "savedMs": units["savedMs"]})
    );
    // \[C-37\] What `hushrun why` says of a class is end to end: e2e_maven.rs.

    // A change in `lib` runs lib's classes and app's, which depends on it, never other's.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x + x; }\n}\n",
    );
    let lib = run(&r, &["test"], &[]);
    assert_eq!(lib.code, 0, "{}", lib.said());
    at_most_the_audit(
        &classes(&lib.stdout),
        &[
            "shop.lib.PriceTest",
            "shop.lib.RoundTest",
            "shop.app.CartTest",
        ],
    );
    // A POM is the rest: everything runs.
    r.write(
        "app/pom.xml",
        &module_pom("app", &["lib"]).replace("</project>", "<!-- x -->\n</project>"),
    );
    let pom = run(&r, &["test"], &[]);
    assert_eq!(pom.code, 0, "{}", pom.said());
    assert_eq!(classes(&pom.stdout).len(), 6, "{:?}", classes(&pom.stdout));
}

// covers: C-39, C-47
#[test]
fn a_failed_test_runs_alone_and_the_other_tests_of_its_class_are_reused_one_by_one() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("{mode: reuse, unit: test}"), maven);
    // At the end: lib's failure doesn't stop other's classes, which don't depend on it; app's,
    // which do, don't run.
    let first = run(&r, &["--fail-at-end", "test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    // A test is its method, its nested class's path before it; a parametrized method is one
    // test, whatever its invocations.
    assert_eq!(
        map["units"][PRICE_TEST]["tests"],
        serde_json::json!({
            "Inner#innerPasses": "passed",
            "failsWhenAsked": "failed",
            "param": "passed",
            "passes": "passed"
        }),
        "{map:#}"
    );

    let second = run(&r, &["--fail-at-end", "test"], &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    let line = second.line("unit: ");
    assert!(line.contains(" tests run, in "), "{line}");
    // The failed test ran, and the class's others were reused one by one, but one the audit may
    // have run: PriceTest ran 1 or 2 of its 6 invocations, never all.
    let ran = classes(&second.stdout);
    let price = ran.get("shop.lib.PriceTest").copied().unwrap_or(0);
    assert!((1..=4).contains(&price), "{ran:?}\n{}", second.stdout);
    let map = r.map();
    assert_eq!(
        map["units"][PRICE_TEST]["tests"]["failsWhenAsked"], "passed",
        "{map:#}"
    );
    let tests = &second.units["tests"];
    assert!(
        tests["reused"].as_u64().unwrap() >= 4,
        "{tests}\n{}",
        second.said()
    );
}

// covers: C-47
#[test]
fn junit4_where_the_hook_cant_load_runs_everything_and_records_what_ran() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&junit4("reuse"), maven);
    let first = run(&r, &["test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert_eq!(
        r.unit_results(),
        BTreeMap::from([("fail".into(), 1), ("pass".into(), 2)])
    );
    // The plan lists the classes that passed, and the hook, which can't load in JUnit 4's
    // provider, leaves nothing out: every class runs, and says so.
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        classes(&second.stdout).len(),
        3,
        "{:?}",
        classes(&second.stdout)
    );
    second.line("unit: no test file was reused: the test-JVM hook loaded in no test JVM");
    assert_eq!(
        second.units,
        serde_json::json!({"total": 3, "ran": 3, "reused": 0})
    );
}

// covers: C-47
#[test]
fn a_fork_that_died_and_its_stale_report_never_read_as_a_pass() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&junit4("observe"), maven);
    // AskTest fails, so the check didn't pass; CrashTest passed, and wrote its report.
    let first = run(&r, &["test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert_eq!(first.units["total"], 3, "{}", first.said());
    let report = r
        .repo
        .join("core/target/surefire-reports/TEST-shop.core.CrashTest.xml");
    assert!(report.is_file());
    // CrashTest's fork dies: its report from the first run is still there, and is never this
    // run's, so it is never a pass.
    let crashed = run(&r, &["test"], &[("SHOP_CRASH", "1")]);
    assert_ne!(crashed.code, 0, "{}", crashed.said());
    assert!(
        crashed.stdout.contains("The forked VM terminated"),
        "{}",
        crashed.stdout
    );
    assert!(report.is_file(), "the stale report is still there");
    let units = &crashed.units;
    assert!(units["total"].as_u64().unwrap() <= 2, "{units}");
}

// covers: C-47
#[test]
fn a_module_built_without_the_modules_it_depends_on_isnt_followed() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("observe"), maven);
    if online() {
        // Filling the repository: the plugins that install lib.
        let _ = r.plain(&["-pl", "lib", "-am", "install", "-DskipTests"]);
    }
    let installed = r.plain(&["-pl", "lib", "-am", "install", "-DskipTests"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stdout)
    );
    let app = run(&r, &["-pl", "app", "test"], &[]);
    assert_eq!(app.code, 0, "{}", app.said());
    assert!(
        app.line("unit: with `tests: reuse`")
            .contains("hushrun couldn't follow all that 1 of them read"),
        "{}",
        app.said()
    );
    let map = r.map();
    assert_eq!(
        map["units"][CART_TEST]["notFollowed"],
        serde_json::json!(["upstream-not-built"]),
        "{map:#}"
    );
}

// covers: C-32, C-47
#[test]
fn tests_testng_skipped_after_a_failed_suite_setup_never_pass() {
    let Some(maven) = maven() else { return };
    let files = testng("reuse", SUITE_SETUP);
    let r = Reactor::new(&files, maven);
    // The suite's setup fails: TestNG skips every test, OrderTest's failing one among them.
    let first = run(&r, &["test"], &[("SHOP_NO_DB", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    // \[C-32\] The run's last line, which counts Maven's tests, is end to end: e2e_maven.rs.
    // Only the setup's failure is recorded: no skipped class passed.
    assert_eq!(
        r.unit_results(),
        BTreeMap::from([("fail".into(), 1)]),
        "{}",
        first.said()
    );
    // The database is back: OrderTest runs, and fails, as plain Maven says.
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("ng.OrderTest.total"),
        "{}",
        second.stdout
    );
}

// covers: C-39, C-47
#[test]
fn a_reused_testng_base_class_leaves_out_none_of_the_tests_its_subclasses_inherit() {
    let Some(maven) = maven() else { return };
    // ATest inherits Base's test, which fails in ATest: a test of each class, as TestNG runs it.
    let files = testng(
        "reuse",
        &[
            (
                "src/test/java/ng/Base.java",
                "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class Base {\n  protected int value() { return 1; }\n  @Test public void inherited() { assertEquals(value(), 1); }\n}\n",
            ),
            (
                "src/test/java/ng/ATest.java",
                "package ng;\nimport org.testng.annotations.*;\npublic class ATest extends Base {\n  @Override protected int value() { return 2; }\n  @Test public void own() {}\n}\n",
            ),
        ],
    );
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test", "-Dtest=Base,ATest,Green*Test"], &[]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert!(
        first.stdout.contains("ng.ATest.inherited"),
        "{}",
        first.stdout
    );
    // Nothing changed: Base is reused, and ATest runs whole, its inherited test failing again.
    let second = run(&r, &["test", "-Dtest=Base,ATest,Green*Test"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("ng.ATest.inherited"),
        "{}",
        second.stdout
    );
}

/// A processor's module, `proc`, and `app`, whose tests read only what `proc`'s processor
/// generates: `app` names `proc` in its compiler's `annotationProcessorPaths`, never as a
/// dependency. `VALUE` is what the generated `GenValue.value()` returns.
fn processor(value: u32) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version><packaging>pom</packaging>
  <modules><module>proc</module><module>app</module></modules>
  <properties><maven.compiler.release>17</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies><dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><version>5.11.4</version><scope>test</scope></dependency></dependencies>
  <build><plugins>
    <plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin>
    <plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId><version>3.15.0</version></plugin>
  </plugins></build>
</project>
"#,
        ),
        file(
            "proc/pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>proc</artifactId>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId><configuration><proc>none</proc></configuration></plugin></plugins></build>
</project>
"#,
        ),
        file(
            "proc/src/main/java/proc/GenProcessor.java",
            r#"package proc;
import java.io.Writer;
import java.util.Set;
import javax.annotation.processing.*;
import javax.lang.model.SourceVersion;
import javax.lang.model.element.TypeElement;
@SupportedAnnotationTypes("*")
public class GenProcessor extends AbstractProcessor {
  private boolean done;
  @Override public SourceVersion getSupportedSourceVersion() { return SourceVersion.latestSupported(); }
  @Override public boolean process(Set<? extends TypeElement> a, RoundEnvironment env) {
    if (done) return false; done = true;
    try (Writer w = processingEnv.getFiler().createSourceFile("app.GenValue").openWriter()) {
      w.write("package app; public class GenValue { public static int value() { return " + Values.VALUE + "; } }");
    } catch (Exception e) { throw new RuntimeException(e); }
    return false;
  }
}
"#,
        ),
        file(
            "proc/src/main/resources/META-INF/services/javax.annotation.processing.Processor",
            "proc.GenProcessor\n",
        ),
        file(
            "proc/src/test/java/proc/ProcTest.java",
            "package proc;\nimport org.junit.jupiter.api.Test;\nclass ProcTest { @Test void loads() { new GenProcessor(); } }\n",
        ),
        file(
            "app/pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId>
    <configuration><annotationProcessorPaths><path><groupId>shop</groupId><artifactId>proc</artifactId><version>1.0</version></path></annotationProcessorPaths></configuration>
  </plugin></plugins></build>
</project>
"#,
        ),
        file(
            "app/src/main/java/app/App.java",
            "package app;\npublic class App {}\n",
        ),
        file(
            "app/src/test/java/app/AppTest.java",
            "package app;\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\nclass AppTest { @Test void generated() { assertEquals(1, GenValue.value()); } }\n",
        ),
    ];
    files.push(values(value));
    for n in 1..=9 {
        files.push(file(
            &format!("app/src/test/java/app/Green{n}Test.java"),
            &format!("package app;\nimport org.junit.jupiter.api.Test;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
        ));
    }
    files
}

fn values(value: u32) -> (String, String) {
    file(
        "proc/src/main/java/proc/Values.java",
        &format!(
            "package proc;\npublic class Values {{ public static final int VALUE = {value}; }}\n"
        ),
    )
}

// covers: C-47
#[test]
fn a_change_to_an_annotation_processor_of_the_build_runs_the_tests_of_the_code_it_generates() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&processor(1), maven);
    let first = run(&r, &["clean", "test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // app's classes read proc's sources, which its compiler runs.
    let map = r.map();
    let reads = map["units"]["app/src/test/java/app/AppTest.java"]["reads"].clone();
    assert!(
        reads
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("proc/src/main/java/proc/Values.java")),
        "{map:#}"
    );
    let (path, text) = values(2);
    r.write(&path, &text);
    let second = run(&r, &["clean", "test"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("app.AppTest.generated"),
        "{}",
        second.stdout
    );
}

// covers: C-32, C-47
#[test]
fn a_fork_that_died_after_its_class_tests_passed_records_no_pass_for_it() {
    let Some(maven) = maven() else { return };
    // JUnit 4, which Surefire runs without the hook, and JUnit 5, which it runs with it: a class
    // whose tests passed and whose JVM dies in its class teardown, run last of its module.
    let crash4 = "package shop.core;\n\nimport org.junit.*;\n\npublic class CrashTest {\n  @Test public void a() {}\n  @Test public void b() {}\n  @AfterClass public static void after() { if (System.getenv(\"SHOP_CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n";
    let crash5 = "package shop.lib;\n\nimport org.junit.jupiter.api.*;\n\nclass CrashTest {\n  @Test void a() {}\n  @Test void b() {}\n  @AfterAll static void after() { if (System.getenv(\"SHOP_CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n";
    let mut four = junit4("observe");
    four.retain(|(path, _)| !path.ends_with("CrashTest.java"));
    four.push(file("core/src/test/java/shop/core/CrashTest.java", crash4));
    let mut five = shop("observe");
    five.push(file("lib/src/test/java/shop/lib/CrashTest.java", crash5));
    // JUnit 4: AskTest and CalcTest pass, then CrashTest. JUnit 5: lib's RoundTest and PriceTest
    // pass, then CrashTest; app, which depends on lib, doesn't run; other's three pass.
    for (files, order, passed) in [(four, "alphabetical", 2), (five, "reversealphabetical", 5)] {
        let r = Reactor::new(&files, maven.clone());
        let ran = run(
            &r,
            &[
                "--fail-at-end",
                "test",
                &format!("-Dsurefire.runOrder={order}"),
            ],
            &[("SHOP_CRASH", "1")],
        );
        assert_ne!(ran.code, 0, "{}", ran.said());
        assert!(
            ran.stdout.contains("The forked VM terminated"),
            "{}",
            ran.stdout
        );
        assert_eq!(
            r.unit_results(),
            BTreeMap::from([("pass".into(), passed)]),
            "{order}: {}",
            ran.said()
        );
    }
}

// covers: C-32, C-47
#[cfg(unix)]
#[test]
fn a_maven_wrapper_that_starts_maven_again_records_neither_start() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(maven) = maven() else { return };
    let counts = tempfile::tempdir().unwrap();
    // A wrapper that starts Maven again when it fails, and a test that fails the first time.
    let mut files = junit4("{mode: reuse, unit: test}");
    files.push(file(
        "mvnw",
        "#!/bin/sh\n\"$REAL_MVN\" \"$@\" || \"$REAL_MVN\" \"$@\"\n",
    ));
    files.push(file(
        "core/src/test/java/shop/core/FlakyTest.java",
        "package shop.core;\n\nimport org.junit.Test;\nimport java.nio.file.*;\nimport static org.junit.Assert.*;\n\npublic class FlakyTest {\n  @Test public void flaky() throws Exception { Path p = Paths.get(System.getenv(\"COUNTS\"), \"ran\"); boolean again = Files.exists(p); Files.write(p, new byte[0]); assertTrue(again); }\n}\n",
    ));
    let r = Reactor::new(&files, maven);
    std::fs::set_permissions(r.repo.join("mvnw"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut words = r.mvn(&["test"]);
    let real = std::mem::replace(&mut words[0], "./mvnw".into());
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let counts = counts.path().to_string_lossy().into_owned();
    let ran = run_command(&r, &words, &[("REAL_MVN", &real), ("COUNTS", &counts)]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.stdout.contains("<<< FAILURE!") && ran.stdout.contains("BUILD SUCCESS"),
        "{}",
        ran.stdout
    );
    // Two starts of Maven under one COMMAND: which ran in which can't be told, so neither the
    // first start's failure nor the second's pass is recorded.
    assert!(
        ran.lines
            .iter()
            .any(|l| l.contains("Maven started 2 times under COMMAND")),
        "{}",
        ran.said()
    );
    assert!(r.unit_results().is_empty(), "{}", ran.said());
}

/// \[C-39\] Test classes whose tests look alike: `Base`'s `inherited`, which `ATest`, `BTest`,
/// `OuterTest` and `OuterTest.Inner` each inherit, failing in a class whose simple name with
/// `_FAIL` is set; and `same`, a method of `OuterTest` and of its nested `Inner`. `FLIP` fails
/// `ATest` and `OuterTest`, so their other tests are listed one by one; `o1` to `o9` keep the audit
/// off the look-alikes, mostly.
const LOOK_ALIKE_J5: &[(&str, &str)] = &[
    (
        "shop/Base.java",
        "package shop;\nimport org.junit.jupiter.api.*;\npublic abstract class Base {\n  @Test void inherited() { Assertions.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n}\n",
    ),
    (
        "shop/ATest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass ATest extends Base {\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FLIP\")); }\n  @Test void o1() {}\n  @Test void o2() {}\n  @Test void o3() {}\n  @Test void o4() {}\n  @Test void o5() {}\n  @Test void o6() {}\n  @Test void o7() {}\n  @Test void o8() {}\n  @Test void o9() {}\n}\n",
    ),
    (
        "shop/BTest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass BTest extends Base {\n  @Test void own() {}\n}\n",
    ),
    (
        "shop/OuterTest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass OuterTest extends Base {\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FLIP\")); }\n  @Test void same() {}\n  @Nested class Inner extends Base {\n    @Test void same() { Assertions.assertNull(System.getenv(\"INNER_FAIL\")); }\n  }\n}\n",
    ),
];
/// Run 1's variables: the flips, and the look-alikes that fail and so aren't listed.
const LOOK_ALIKE_FIRST: &[(&str, &str)] = &[
    ("FLIP", "1"),
    ("BTest_FAIL", "1"),
    ("Inner_FAIL", "1"),
    ("INNER_FAIL", "1"),
];
/// Run 2's: the flips fixed, the look-alikes not listed still failing.
const LOOK_ALIKE_SECOND: &[(&str, &str)] = &[
    ("BTest_FAIL", "1"),
    ("Inner_FAIL", "1"),
    ("INNER_FAIL", "1"),
];

// covers: C-39, C-47
#[test]
fn junit5_tests_that_look_alike_in_subclasses_and_nested_classes_are_left_out_one_by_one() {
    let Some(maven) = maven() else { return };
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("pom.xml", &parent(&["core"], "5", "3.5.4")),
        file("core/pom.xml", &module_pom("core", &[])),
    ];
    for (path, text) in LOOK_ALIKE_J5 {
        files.push(file(&format!("core/src/test/java/{path}"), text));
    }
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], LOOK_ALIKE_FIRST);
    assert_eq!(first.code, 1, "{}", first.said());
    // ATest's, OuterTest's and their inherited tests passed and are listed; BTest's and Inner's
    // look-alikes failed, aren't, and run again, and fail again.
    let second = run(&r, &["test"], LOOK_ALIKE_SECOND);
    assert_eq!(second.code, 1, "{}", second.said());
    let failed: Vec<&str> = second
        .stdout
        .lines()
        .filter(|l| l.contains("<<< FAILURE!") && l.starts_with("[ERROR] shop."))
        .map(|l| l["[ERROR] ".len()..].split(' ').next().unwrap())
        .collect();
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = second.units["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.said());
    // Surefire names a nested class's test by its outer class: OuterTest's own `inherited` and
    // `same` never fail, so these are Inner's.
    for name in [
        "shop.BTest.inherited",
        "shop.OuterTest.inherited",
        "shop.OuterTest.same",
    ] {
        assert!(
            failed.contains(&name),
            "{name}: {failed:?}\n{}",
            second.stdout
        );
    }
}

/// \[C-39\] JUnit 4 test classes whose tests look alike: abstract `Base`'s `inherited`, which
/// `ATest` and `BTest` inherit; concrete `BaseTest`, whose `inherited` and `flip` `SubTest`
/// inherits; and concrete `Base2Test` and `Sub2Test`, the same. `inherited` fails in a class whose
/// simple name with `_FAIL` is set; `FLIP` fails `flip`.
const LOOK_ALIKE_J4: &[(&str, &str)] = &[
    (
        "shop/Base.java",
        "package shop;\nimport org.junit.*;\npublic abstract class Base {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n}\n",
    ),
    (
        "shop/ATest.java",
        "package shop;\nimport org.junit.*;\npublic class ATest extends Base {\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n  @Test public void o1() {}\n  @Test public void o2() {}\n  @Test public void o3() {}\n  @Test public void o4() {}\n  @Test public void o5() {}\n  @Test public void o6() {}\n  @Test public void o7() {}\n  @Test public void o8() {}\n  @Test public void o9() {}\n}\n",
    ),
    (
        "shop/BTest.java",
        "package shop;\nimport org.junit.*;\npublic class BTest extends Base {\n  @Test public void own() {}\n}\n",
    ),
    (
        "shop/BaseTest.java",
        "package shop;\nimport org.junit.*;\npublic class BaseTest {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n}\n",
    ),
    (
        "shop/SubTest.java",
        "package shop;\npublic class SubTest extends BaseTest {}\n",
    ),
    (
        "shop/Base2Test.java",
        "package shop;\nimport org.junit.*;\npublic class Base2Test {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n}\n",
    ),
    (
        "shop/Sub2Test.java",
        "package shop;\npublic class Sub2Test extends Base2Test {}\n",
    ),
];
/// Run 1's variables: the flips, and the look-alikes that fail and so aren't listed.
const LOOK_ALIKE_J4_FIRST: &[(&str, &str)] = &[
    ("FLIP", "1"),
    ("BTest_FAIL", "1"),
    ("SubTest_FAIL", "1"),
    ("Base2Test_FAIL", "1"),
];
/// Run 2's: the flips fixed, the look-alikes not listed still failing.
const LOOK_ALIKE_J4_SECOND: &[(&str, &str)] = &[
    ("BTest_FAIL", "1"),
    ("SubTest_FAIL", "1"),
    ("Base2Test_FAIL", "1"),
];

// covers: C-39, C-47
#[test]
fn junit4_tests_that_look_alike_in_subclasses_are_left_out_one_by_one_through_vintage() {
    let Some(maven) = maven() else { return };
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>vintage</artifactId><version>1.0</version>
  <properties><maven.compiler.release>11</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>
    <dependency><groupId>org.junit.vintage</groupId><artifactId>junit-vintage-engine</artifactId><version>5.14.4</version><scope>test</scope></dependency>
  </dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin></plugins></build>
</project>
"#,
        ),
    ];
    for (path, text) in LOOK_ALIKE_J4 {
        files.push(file(&format!("src/test/java/{path}"), text));
    }
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], LOOK_ALIKE_J4_FIRST);
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(&r, &["test"], LOOK_ALIKE_J4_SECOND);
    assert_eq!(second.code, 1, "{}", second.said());
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = second.units["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.said());
    let failed: Vec<&str> = second
        .stdout
        .lines()
        .filter(|l| l.contains("<<< FAILURE!") && l.starts_with("[ERROR] shop."))
        .map(|l| l["[ERROR] ".len()..].split(' ').next().unwrap())
        .collect();
    for name in [
        "shop.BTest.inherited",
        "shop.SubTest.inherited",
        "shop.Base2Test.inherited",
    ] {
        assert!(
            failed.contains(&name),
            "{name}: {failed:?}\n{}",
            second.stdout
        );
    }
}

// covers: C-30, C-47
#[test]
fn a_jvm_file_no_module_of_the_build_owns_is_the_rest() {
    let Some(maven) = maven() else { return };
    // A module the reactor leaves out, one in a profile that isn't active, and a tool beside
    // app's sources that a script builds.
    let mut files = shop("reuse");
    for (path, text) in files.iter_mut() {
        if path == "pom.xml" {
            *text = text.replace(
                "</modules>",
                "</modules>\n  <profiles><profile><id>extra</id><modules><module>prof</module></modules></profile></profiles>",
            );
        }
    }
    files.extend([
        file("extra/pom.xml", &module_pom("extra", &[])),
        file(
            "extra/src/main/java/shop/extra/E.java",
            "package shop.extra;\npublic class E {}\n",
        ),
        file("prof/pom.xml", &module_pom("prof", &[])),
        file(
            "prof/src/main/java/shop/prof/P.java",
            "package shop.prof;\npublic class P {}\n",
        ),
        file(
            "app/tools/Gen.java",
            "package tools;\npublic class Gen {}\n",
        ),
    ]);
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    for path in [
        "extra/src/main/java/shop/extra/E.java",
        "prof/src/main/java/shop/prof/P.java",
        "app/tools/Gen.java",
    ] {
        assert!(map["rest"][path].is_string(), "{path}: {map:#}");
    }
    r.write(
        "extra/src/main/java/shop/extra/E.java",
        "package shop.extra;\npublic class E { int x; }\n",
    );
    let changed = run(&r, &["test"], &[]);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        classes(&changed.stdout).len(),
        6,
        "{:?}",
        classes(&changed.stdout)
    );
}

// covers: C-30, C-47
#[test]
fn a_module_this_command_didnt_build_keeps_its_sources_on_the_lists_of_the_tests_that_read_them() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("reuse"), maven);
    // app and what it depends on: lib's sources are on app's list, other's are the rest.
    let first = run(&r, &["-pl", "app", "-am", "test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    let reads = map["units"][CART_TEST]["reads"].as_array().unwrap().clone();
    assert!(
        reads.contains(&serde_json::json!("lib/src/main/java/shop/lib/Price.java")),
        "{map:#}"
    );
    assert!(
        map["rest"]["other/src/main/java/shop/other/Greet.java"].is_string(),
        "{map:#}"
    );
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x + x; }\n}\n",
    );
    let second = run(&r, &["-pl", "app", "-am", "test"], &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        classes(&second.stdout).contains_key("shop.app.CartTest"),
        "{:?}",
        classes(&second.stdout)
    );
}

// covers: C-30, C-47
#[test]
fn the_modules_a_failed_build_never_reached_keep_what_the_last_run_that_reached_them_learned() {
    let Some(maven) = maven() else { return };
    for args in [&["test"][..], &["--fail-at-end", "test"][..]] {
        let mut files = shop("reuse");
        files.push(file("notes.txt", "a file of the rest\n"));
        let r = Reactor::new(&files, maven.clone());
        let first = run(&r, args, &[]);
        assert_eq!(first.code, 0, "{args:?}: {}", first.said());
        // lib changes and its PriceTest fails: Maven stops before app (and before other, at
        // once), whose classes never run.
        r.write(
            "lib/src/main/java/shop/lib/Price.java",
            "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x + x; }\n}\n",
        );
        let failed = run(&r, args, &[(FAIL, "1")]);
        assert_eq!(failed.code, 1, "{args:?}: {}", failed.said());
        assert!(
            !classes(&failed.stdout).contains_key("shop.app.CartTest"),
            "{args:?}: {:?}",
            classes(&failed.stdout)
        );
        let map = r.map();
        for path in [CART_TEST, "app/src/main/java/shop/app/Cart.java"] {
            assert!(map["rest"][path].is_null(), "{args:?}: {path}: {map:#}");
        }
        // The next run reuses what passed on the files it read (other's classes, in the first
        // run), and says nothing of a file that is there.
        let next = run(&r, args, &[]);
        assert_eq!(next.code, 0, "{args:?}: {}", next.said());
        let line = next.line("unit: ");
        assert!(!line.contains("gone"), "{args:?}: {line}");
        assert!(
            line.contains(" of 6 test classes run; "),
            "{args:?}: {line}"
        );
        at_most_the_audit(
            &classes(&next.stdout),
            &["shop.lib.PriceTest", "shop.app.CartTest"],
        );
        // A file of the rest that really is gone still says so.
        std::fs::remove_file(r.repo.join("notes.txt")).expect("remove");
        let gone = run(&r, args, &[]);
        assert_eq!(gone.code, 0, "{args:?}: {}", gone.said());
        assert_eq!(
            gone.line("unit: "),
            "unit: all 6 test classes run: notes.txt is gone, and any test may have read it",
            "{args:?}"
        );
    }
}

// covers: C-32, C-47
#[test]
fn a_test_that_ends_its_fork_with_system_exit_records_no_pass() {
    let Some(maven) = maven() else { return };
    // JUnit 4 without the hook and JUnit 5 with it: `b` ends the fork with `System.exit(0)`.
    let exit4 = "package shop.core;\n\nimport org.junit.*;\nimport org.junit.runners.MethodSorters;\n\n@FixMethodOrder(MethodSorters.NAME_ASCENDING)\npublic class ExitTest {\n  @Test public void a() {}\n  @Test public void b() { if (System.getenv(\"SHOP_EXIT\") != null) System.exit(0); }\n  @Test public void c() { Assert.fail(\"c\"); }\n}\n";
    let exit5 = "package shop.lib;\n\nimport org.junit.jupiter.api.*;\n\n@TestMethodOrder(MethodOrderer.MethodName.class)\nclass ExitTest {\n  @Test void a() {}\n  @Test void b() { if (System.getenv(\"SHOP_EXIT\") != null) System.exit(0); }\n  @Test void c() { Assertions.fail(\"c\"); }\n}\n";
    let mut four = junit4("observe");
    four.push(file("core/src/test/java/shop/core/ExitTest.java", exit4));
    let mut five = shop("observe");
    five.push(file("lib/src/test/java/shop/lib/ExitTest.java", exit5));
    // Run last of its module: JUnit 4's AskTest, CalcTest and CrashTest pass first; lib's
    // RoundTest and PriceTest, and other's three, in JUnit 5.
    for (files, order, passed) in [(four, "alphabetical", 3), (five, "reversealphabetical", 5)] {
        let r = Reactor::new(&files, maven.clone());
        let ran = run(
            &r,
            &[
                "--fail-at-end",
                "test",
                &format!("-Dsurefire.runOrder={order}"),
            ],
            &[("SHOP_EXIT", "1")],
        );
        assert_ne!(ran.code, 0, "{}", ran.said());
        assert!(
            ran.stdout.contains("The forked VM terminated"),
            "{}",
            ran.stdout
        );
        assert_eq!(
            r.unit_results(),
            BTreeMap::from([("pass".into(), passed)]),
            "{order}: {}",
            ran.said()
        );
    }
}

// covers: C-32, C-47
#[test]
fn a_failed_run_with_a_case_no_class_owns_says_once_why_it_keeps_no_pass() {
    let Some(maven) = maven() else { return };
    // When asked to fail, OrphanTest writes a report beside Surefire's whose failing case names
    // no class, as a runner that loses its class does; PriceTest fails too, so the build fails.
    let orphan = format!(
        r#"package shop.other;

import org.junit.jupiter.api.Test;
import java.nio.file.*;

class OrphanTest {{
  @Test void writes() throws Exception {{
    if (System.getenv("{FAIL}") == null) return;
    Path dir = Paths.get("target", "surefire-reports");
    Files.createDirectories(dir);
    Files.write(dir.resolve("TEST-orphan.xml"), ("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        + "<testsuite name=\"orphan\" tests=\"1\" failures=\"1\">"
        + "<testcase classname=\"\" name=\"lost\" time=\"0.001\"><failure message=\"lost\"/></testcase>"
        + "</testsuite>\n").getBytes("UTF-8"));
  }}
}}
"#
    );
    let mut files = shop("reuse");
    files.push(file(
        "other/src/test/java/shop/other/OrphanTest.java",
        &orphan,
    ));
    let r = Reactor::new(&files, maven);
    let failed = run(&r, &["--fail-at-end", "test"], &[(FAIL, "1")]);
    assert_ne!(failed.code, 0, "{}", failed.said());
    let why = "unit: hushrun couldn't tell which test class 1 of its tests or failures belong to, so no pass of a run that failed is kept";
    assert_eq!(
        failed.lines.iter().filter(|l| *l == why).count(),
        1,
        "{}",
        failed.said()
    );
    // Nothing passed is kept: the classes that passed run again.
    assert!(
        !r.unit_results().contains_key("pass"),
        "{:?}",
        r.unit_results()
    );
}

// covers: C-47
#[test]
fn test_classes_a_module_reaches_through_another_modules_test_jar_are_read() {
    let Some(maven) = maven() else { return };
    // core makes a test-jar of its test classes, Fixtures among them; testkit depends on it at
    // compile scope; x depends on testkit, and XTest reads Fixtures through Kit.
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file("pom.xml", &parent(&["core", "testkit", "x"], "5", "3.5.4")),
        file(
            "core/pom.xml",
            &reactor_module(
                "core",
                "<build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version><executions><execution><goals><goal>test-jar</goal></goals></execution></executions></plugin></plugins></build>",
            ),
        ),
        file(
            "core/src/main/java/core/Core.java",
            "package core;\n\npublic class Core {}\n",
        ),
        file(
            "core/src/test/java/core/Fixtures.java",
            "package core;\n\npublic class Fixtures {\n  public static int v() { return 1; }\n}\n",
        ),
        file(
            "core/src/test/java/core/CoreTest.java",
            "package core;\n\nimport org.junit.jupiter.api.Test;\n\nclass CoreTest {\n  @Test void core() {}\n}\n",
        ),
        file(
            "testkit/pom.xml",
            &reactor_module(
                "testkit",
                "<dependencies><dependency><groupId>shop</groupId><artifactId>core</artifactId><version>1.0</version><type>test-jar</type></dependency></dependencies>",
            ),
        ),
        file(
            "testkit/src/main/java/kit/Kit.java",
            "package kit;\n\npublic class Kit {\n  public static int v() { return core.Fixtures.v(); }\n}\n",
        ),
        file(
            "x/pom.xml",
            &reactor_module(
                "x",
                "<dependencies><dependency><groupId>shop</groupId><artifactId>testkit</artifactId><version>1.0</version><scope>test</scope></dependency></dependencies>",
            ),
        ),
        file(
            "x/src/test/java/x/XTest.java",
            "package x;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass XTest {\n  @Test void v() { assertEquals(1, kit.Kit.v()); }\n}\n",
        ),
    ];
    files.extend(greens("x/src/test/java", "x", 9));
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    let reads = map["units"]["x/src/test/java/x/XTest.java"]["reads"].clone();
    assert!(
        reads
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("core/src/test/java/core/Fixtures.java")),
        "{map:#}"
    );
    r.write(
        "core/src/test/java/core/Fixtures.java",
        "package core;\n\npublic class Fixtures {\n  public static int v() { return 2; }\n}\n",
    );
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(second.stdout.contains("x.XTest.v"), "{}", second.stdout);
}

// covers: C-47
#[test]
fn a_module_a_plugin_of_another_names_is_read_by_that_modules_tests() {
    let Some(maven) = maven() else { return };
    // `it` copies cli's jar with the dependency plugin and runs it: no dependency names cli.
    let jar = "<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version></plugin></plugins>";
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(
            "pom.xml",
            &parent(&["cli", "it"], "5", "3.5.4").replace("</plugins>", jar),
        ),
        file("cli/pom.xml", &reactor_module("cli", "")),
        file(
            "cli/src/main/java/cli/Cli.java",
            "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(1); }\n}\n",
        ),
        file(
            "cli/src/test/java/cli/CliTest.java",
            "package cli;\n\nimport org.junit.jupiter.api.Test;\n\nclass CliTest {\n  @Test void loads() { new Cli(); }\n}\n",
        ),
        file(
            "it/pom.xml",
            &reactor_module(
                "it",
                "<build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-dependency-plugin</artifactId><version>3.7.0</version><executions><execution><id>c</id><phase>process-test-resources</phase><goals><goal>copy</goal></goals><configuration><artifactItems><artifactItem><groupId>shop</groupId><artifactId>cli</artifactId><version>1.0</version><destFileName>cli.jar</destFileName></artifactItem></artifactItems><outputDirectory>${project.build.directory}/deps</outputDirectory></configuration></execution></executions></plugin></plugins></build>",
            ),
        ),
        file(
            "it/src/test/java/it/CliRunTest.java",
            "package it;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass CliRunTest {\n  @Test void prints1() throws Exception {\n    Process p = new ProcessBuilder(System.getProperty(\"java.home\") + \"/bin/java\", \"-cp\", \"target/deps/cli.jar\", \"cli.Cli\").start();\n    assertEquals(\"1\", new String(p.getInputStream().readAllBytes()));\n  }\n}\n",
        ),
    ];
    files.extend(greens("it/src/test/java", "it", 9));
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["package"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    let reads = map["units"]["it/src/test/java/it/CliRunTest.java"]["reads"].clone();
    assert!(
        reads
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("cli/src/main/java/cli/Cli.java")),
        "{map:#}"
    );
    r.write(
        "cli/src/main/java/cli/Cli.java",
        "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(2); }\n}\n",
    );
    let second = run(&r, &["package"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("it.CliRunTest.prints1"),
        "{}",
        second.stdout
    );
}

// covers: C-47
#[test]
fn a_dependency_on_a_modules_coordinates_at_another_version_reads_that_module() {
    let Some(maven) = maven() else { return };
    if online() {
        return;
    }
    // app takes lib at 0.9, which the local repository holds (a jar an earlier build of this
    // checkout installed), not the reactor's 1.0.
    let mut files = shop("reuse");
    for (path, text) in files.iter_mut() {
        if path == "app/pom.xml" {
            *text = text.replace(
                "<version>1.0</version></dependency>",
                "<version>0.9</version></dependency>",
            );
        }
        if path == "app/src/main/java/shop/app/Cart.java" {
            *text = "package shop.app;\n\npublic class Cart {\n  public static int total(int... xs) { return 6; }\n}\n".into();
        }
    }
    let r = Reactor::new(&files, maven);
    let dir = r.root.join("m2/shop/lib/0.9");
    std::fs::create_dir_all(&dir).unwrap();
    let mut empty = b"PK\x05\x06".to_vec();
    empty.extend([0u8; 18]);
    std::fs::write(dir.join("lib-0.9.jar"), &empty).unwrap();
    std::fs::write(
        dir.join("lib-0.9.pom"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>shop</groupId><artifactId>lib</artifactId><version>0.9</version></project>\n",
    )
    .unwrap();
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // The extension names a module by its coordinates without a version: app's classes read lib.
    let map = r.map();
    assert!(
        map["units"][CART_TEST]["reads"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("lib/src/main/java/shop/lib/Price.java")),
        "{map:#}"
    );
}

// covers: C-32, C-47
#[test]
fn a_fork_killed_or_out_of_memory_records_no_pass_for_its_class() {
    let Some(maven) = maven() else { return };
    // lib's WhatTest does what `SHOP_WHAT` says, run last of lib; other's three pass.
    let what = "package shop.lib;\n\nimport org.junit.jupiter.api.*;\n\nclass ZWhatTest {\n  @Test void a() {}\n  @Test void b() throws Exception {\n    String what = System.getenv(\"SHOP_WHAT\");\n    if (\"kill\".equals(what)) new ProcessBuilder(\"kill\", \"-9\", String.valueOf(ProcessHandle.current().pid())).start().waitFor();\n    if (\"oom\".equals(what)) { java.util.List<long[]> all = new java.util.ArrayList<>(); while (true) all.add(new long[1 << 20]); }\n  }\n  @Test void c() {}\n}\n";
    for case in ["kill", "oom"] {
        let mut files = shop("observe");
        files.push(file("lib/src/test/java/shop/lib/ZWhatTest.java", what));
        let r = Reactor::new(&files, maven.clone());
        let ran = run(
            &r,
            &[
                "--fail-at-end",
                "test",
                "-Dsurefire.runOrder=alphabetical",
                "-DargLine=-Xmx16m",
            ],
            &[("SHOP_WHAT", case)],
        );
        assert_ne!(ran.code, 0, "{case}: {}", ran.said());
        let results = r.unit_results();
        // lib's PriceTest and RoundTest, other's three; ZWhatTest passes nowhere: its fork died,
        // or its test failed with the error.
        assert_eq!(
            results.get("pass"),
            Some(&5),
            "{case}: {results:?}\n{}",
            ran.stdout
        );
    }
}

// covers: C-47
#[test]
fn the_modules_that_depend_on_a_module_read_it_under_amd() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("reuse"), maven);
    let first = run(&r, &["-pl", "lib", "-amd", "test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    assert!(
        map["units"][CART_TEST]["reads"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("lib/src/main/java/shop/lib/Price.java")),
        "{map:#}"
    );
    assert!(map["units"].get(GREET_TEST).is_none(), "{map:#}");
}

// covers: C-39, C-47
#[test]
fn junit4_and_junit5_in_one_module_and_parameterized_through_vintage_are_left_out_one_by_one() {
    let Some(maven) = maven() else { return };
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>mixed</artifactId><version>1.0</version>
  <properties><maven.compiler.release>11</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>
    <dependency><groupId>org.junit.vintage</groupId><artifactId>junit-vintage-engine</artifactId><version>5.14.4</version><scope>test</scope></dependency>
    <dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><version>5.11.4</version><scope>test</scope></dependency>
  </dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin></plugins></build>
</project>
"#,
        ),
        // JUnit 4's Parameterized: `small` fails for one of its invocations, `big` passes.
        file(
            "src/test/java/shop/ParamTest.java",
            "package shop;\n\nimport org.junit.*;\nimport org.junit.runner.RunWith;\nimport org.junit.runners.Parameterized;\nimport java.util.*;\n\n@RunWith(Parameterized.class)\npublic class ParamTest {\n  @Parameterized.Parameters(name = \"{index}: x={0}\") public static Collection<Object[]> data() { return Arrays.asList(new Object[][]{{1},{2},{3}}); }\n  private final int x; public ParamTest(int x) { this.x = x; }\n  @Test public void small() { Assert.assertTrue(x < 3 || System.getenv(\"FAIL\") == null); }\n  @Test public void big() { Assert.assertTrue(x > 0); }\n}\n",
        ),
        // JUnit 5 beside it.
        file(
            "src/test/java/shop/JupiterTest.java",
            "package shop;\n\nimport org.junit.jupiter.api.*;\n\nclass JupiterTest {\n  @Test void j() {}\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FAIL\")); }\n}\n",
        ),
    ];
    for n in 1..=9 {
        files.push(file(
            &format!("src/test/java/shop/Green{n}Test.java"),
            &format!("package shop;\n\nimport org.junit.Test;\n\npublic class Green{n}Test {{\n  @Test public void green() {{}}\n}}\n"),
        ));
    }
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert_eq!(
        r.map()["units"]["src/test/java/shop/ParamTest.java"]["tests"],
        serde_json::json!({"big": "passed", "small": "failed"}),
        "{:#}\n{}",
        r.map(),
        first.stdout
    );
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // `small` ran, all three invocations; `big` and JupiterTest's `j` were listed (one of them
    // maybe audited); `flip` ran.
    let ran = classes(&second.stdout);
    assert!(
        ran.get("shop.ParamTest").copied().unwrap_or(0) >= 3,
        "{ran:?}"
    );
    assert!(
        ran.get("shop.JupiterTest").copied().unwrap_or(0) >= 1,
        "{ran:?}"
    );
    let reused = second.units["tests"]["reused"].as_u64().unwrap();
    assert!(reused >= 1, "{}", second.said());
}

// covers: C-39, C-47
#[test]
fn a_projects_own_testng_interceptor_and_the_hooks_leave_out_what_was_listed_only() {
    let Some(maven) = maven() else { return };
    // The project's interceptor runs its tests in reverse order, beside the hook's.
    let files = testng(
        "{mode: reuse, unit: test}",
        &[
            (
                "src/test/java/ng/Reverse.java",
                "package ng;\nimport java.util.*;\nimport org.testng.*;\npublic class Reverse implements IMethodInterceptor {\n  public List<IMethodInstance> intercept(List<IMethodInstance> methods, ITestContext context) { List<IMethodInstance> out = new ArrayList<>(methods); Collections.reverse(out); return out; }\n}\n",
            ),
            (
                "src/test/java/ng/OrderTest.java",
                "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\n@Listeners(Reverse.class)\npublic class OrderTest {\n  @Test public void a() {}\n  @Test public void b() { assertNull(System.getenv(\"FAIL_B\")); }\n  @Test public void flip() { assertNull(System.getenv(\"FLIP\")); }\n}\n",
            ),
        ],
    );
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["test"], &[("FLIP", "1"), ("FAIL_B", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    // `a` is listed; `b` failed, isn't, and runs and fails again.
    let second = run(&r, &["test"], &[("FAIL_B", "1")]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("ng.OrderTest.b"),
        "{}",
        second.stdout
    );
}

// covers: C-47
#[test]
fn the_maven_opts_a_person_set_reach_maven_beside_hushruns() {
    let Some(maven) = maven() else { return };
    // hushrun adds its extension and hook to MAVEN_OPTS, after what it had: a person's own
    // property still reaches Maven, which skips the tests as it says.
    let r = Reactor::new(&shop("observe"), maven);
    let ran = run(&r, &["test"], &[("MAVEN_OPTS", "-DskipTests=true")]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        classes(&ran.stdout).is_empty(),
        "{:?}\n{}",
        classes(&ran.stdout),
        ran.stdout
    );
    assert!(ran.stdout.contains("Tests are skipped"), "{}", ran.stdout);
}

// covers: C-47
#[test]
fn a_module_an_assembly_descriptor_names_is_read_by_that_modules_tests() {
    let Some(maven) = maven() else { return };
    // `it` assembles cli's jar with a descriptor file the assembly plugin's configuration names,
    // and its test runs it: no dependency, and no configuration of the POM, names cli.
    let jar = "<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version></plugin></plugins>";
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(
            "pom.xml",
            &parent(&["cli", "it"], "5", "3.5.4").replace("</plugins>", jar),
        ),
        file("cli/pom.xml", &reactor_module("cli", "")),
        file(
            "cli/src/main/java/cli/Cli.java",
            "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(1); }\n}\n",
        ),
        file(
            "it/pom.xml",
            &reactor_module(
                "it",
                "<build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-assembly-plugin</artifactId><version>3.7.1</version><executions><execution><id>d</id><phase>process-test-resources</phase><goals><goal>single</goal></goals><configuration><descriptors><descriptor>src/assembly/dist.xml</descriptor></descriptors></configuration></execution></executions></plugin></plugins></build>",
            ),
        ),
        file(
            "it/src/assembly/dist.xml",
            "<assembly><id>dist</id><formats><format>dir</format></formats><includeBaseDirectory>false</includeBaseDirectory>\n<moduleSets><moduleSet><useAllReactorProjects>true</useAllReactorProjects><includes><include>shop:cli</include></includes><binaries><outputDirectory>lib</outputDirectory><unpack>false</unpack></binaries></moduleSet></moduleSets></assembly>\n",
        ),
        file(
            "it/src/test/java/it/CliRunTest.java",
            "package it;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass CliRunTest {\n  @Test void prints1() throws Exception {\n    Process p = new ProcessBuilder(System.getProperty(\"java.home\") + \"/bin/java\", \"-cp\", \"target/it-1.0-dist/lib/cli-1.0.jar\", \"cli.Cli\").start();\n    assertEquals(\"1\", new String(p.getInputStream().readAllBytes()));\n  }\n}\n",
        ),
    ];
    files.extend(greens("it/src/test/java", "it", 9));
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["package"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    assert!(
        map["units"]["it/src/test/java/it/CliRunTest.java"]["reads"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("cli/src/main/java/cli/Cli.java")),
        "{map:#}"
    );
    r.write(
        "cli/src/main/java/cli/Cli.java",
        "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(2); }\n}\n",
    );
    let second = run(&r, &["package"], &[]);
    assert_eq!(second.code, 1, "{}", second.said());
}

// covers: C-47
#[test]
fn a_module_unpacked_as_a_dependency_or_copied_in_an_active_profile_is_read() {
    let Some(maven) = maven() else { return };
    let jar = "<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version></plugin></plugins>";
    let item = "<artifactItems><artifactItem><groupId>shop</groupId><artifactId>cli</artifactId><version>1.0</version><destFileName>cli.jar</destFileName></artifactItem></artifactItems><outputDirectory>${project.build.directory}/deps</outputDirectory>";
    // unpack-dependencies of a dependency on cli; the dependency plugin's copy only in a profile
    // the command line turns on.
    let unpack = "<dependencies><dependency><groupId>shop</groupId><artifactId>cli</artifactId><version>1.0</version><scope>provided</scope></dependency></dependencies><build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-dependency-plugin</artifactId><version>3.7.0</version><executions><execution><id>u</id><phase>process-test-resources</phase><goals><goal>unpack-dependencies</goal></goals><configuration><includeArtifactIds>cli</includeArtifactIds><outputDirectory>${project.build.directory}/unpacked</outputDirectory></configuration></execution></executions></plugin></plugins></build>".to_string();
    let profile = format!(
        "<profiles><profile><id>copy</id><activation><property><name>with.copy</name></property></activation><build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-dependency-plugin</artifactId><version>3.7.0</version><executions><execution><id>c</id><phase>process-test-resources</phase><goals><goal>copy</goal></goals><configuration>{item}</configuration></execution></executions></plugin></plugins></build></profile></profiles>"
    );
    for (body, classpath, args) in [
        (unpack, "target/unpacked", vec!["package"]),
        (
            profile,
            "target/deps/cli.jar",
            vec!["package", "-Dwith.copy"],
        ),
    ] {
        let mut files = vec![
            file("hushrun.yaml", &yaml("reuse")),
            file(
                "pom.xml",
                &parent(&["cli", "it"], "5", "3.5.4").replace("</plugins>", jar),
            ),
            file("cli/pom.xml", &reactor_module("cli", "")),
            file(
                "cli/src/main/java/cli/Cli.java",
                "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(1); }\n}\n",
            ),
            file("it/pom.xml", &reactor_module("it", &body)),
            file(
                "it/src/test/java/it/CliRunTest.java",
                &format!(
                    "package it;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass CliRunTest {{\n  @Test void prints1() throws Exception {{\n    Process p = new ProcessBuilder(System.getProperty(\"java.home\") + \"/bin/java\", \"-cp\", \"{classpath}\", \"cli.Cli\").start();\n    assertEquals(\"1\", new String(p.getInputStream().readAllBytes()));\n  }}\n}}\n"
                ),
            ),
        ];
        files.extend(greens("it/src/test/java", "it", 9));
        let r = Reactor::new(&files, maven.clone());
        let first = run(&r, &args, &[]);
        assert_eq!(first.code, 0, "{classpath}: {}", first.said());
        let map = r.map();
        assert!(
            map["units"]["it/src/test/java/it/CliRunTest.java"]["reads"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("cli/src/main/java/cli/Cli.java")),
            "{classpath}: {map:#}"
        );
        r.write(
            "cli/src/main/java/cli/Cli.java",
            "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(2); }\n}\n",
        );
        let second = run(&r, &args, &[]);
        assert_eq!(second.code, 1, "{classpath}: {}", second.said());
    }
}

// covers: C-47
#[test]
fn a_module_an_inline_or_shared_assembly_descriptor_takes_is_read_by_that_modules_tests() {
    let Some(maven) = maven() else { return };
    let jar = "<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version></plugin></plugins>";
    let assembly = |inside: &str| {
        format!(
            "<plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-assembly-plugin</artifactId><version>3.7.1</version>{inside}</plugin>"
        )
    };
    let execution = |config: &str| {
        format!(
            "<executions><execution><id>d</id><phase>process-test-classes</phase><goals><goal>single</goal></goals><configuration><appendAssemblyId>false</appendAssemblyId><finalName>bundle</finalName>{config}</configuration></execution></executions>"
        )
    };
    let set = |rule: &str| {
        format!(
            "<id>bin</id><formats><format>dir</format></formats><includeBaseDirectory>false</includeBaseDirectory><moduleSets><moduleSet><useAllReactorProjects>true</useAllReactorProjects>{rule}<binaries><unpack>false</unpack><includeDependencies>false</includeDependencies></binaries></moduleSet></moduleSets>"
        )
    };
    let inline = |rule: &str| {
        assembly(&execution(&format!(
            "<inlineDescriptors><inlineDescriptor>{}</inlineDescriptor></inlineDescriptors>",
            set(rule)
        )))
    };
    // Inline, with no includes but an exclude; inline, with a wildcard include; and a shared
    // descriptor of the reactor's `asm` module, which only holds descriptors, that the plugin
    // takes as its dependency and names by `descriptorRefs`.
    let shared = assembly(&format!(
        "{}<dependencies><dependency><groupId>shop</groupId><artifactId>asm</artifactId><version>1.0</version></dependency></dependencies>",
        execution("<descriptorRefs><descriptorRef>cli</descriptorRef></descriptorRefs>")
    ));
    let cases = [
        (
            "no-includes",
            inline("<excludes><exclude>shop:dist</exclude></excludes>"),
            false,
        ),
        (
            "wildcard",
            inline("<includes><include>shop:c*</include></includes>"),
            false,
        ),
        ("shared", shared, true),
    ];
    for (case, plugin, with_asm) in cases {
        let modules: &[&str] = if with_asm {
            &["cli", "asm", "dist"]
        } else {
            &["cli", "dist"]
        };
        let mut files = vec![
            file("hushrun.yaml", &yaml("reuse")),
            file(
                "pom.xml",
                &parent(modules, "5", "3.5.4").replace("</plugins>", jar),
            ),
            file("cli/pom.xml", &reactor_module("cli", "")),
            file(
                "cli/src/main/java/cli/Cli.java",
                "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(1); }\n}\n",
            ),
            file(
                "dist/pom.xml",
                &reactor_module(
                    "dist",
                    &format!("<build><plugins>{plugin}</plugins></build>"),
                ),
            ),
            file(
                "dist/src/test/java/dist/BundleTest.java",
                "package dist;\n\nimport org.junit.jupiter.api.Test;\nimport java.nio.file.*;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass BundleTest {\n  @Test void prints1() throws Exception {\n    Path jar;\n    try (var s = Files.walk(Paths.get(\"target/bundle\"))) { jar = s.filter(p -> p.getFileName().toString().startsWith(\"cli-\")).findFirst().orElseThrow(); }\n    Process p = new ProcessBuilder(System.getProperty(\"java.home\") + \"/bin/java\", \"-cp\", jar.toString(), \"cli.Cli\").start();\n    assertEquals(\"1\", new String(p.getInputStream().readAllBytes()));\n  }\n}\n",
            ),
        ];
        if with_asm {
            files.push(file("asm/pom.xml", &reactor_module("asm", "")));
            files.push(file(
                "asm/src/main/resources/assemblies/cli.xml",
                &format!(
                    "<assembly>{}</assembly>\n",
                    set("<includes><include>shop:cli</include></includes>")
                ),
            ));
        }
        files.extend(greens("dist/src/test/java", "dist", 9));
        let r = Reactor::new(&files, maven.clone());
        let first = run(&r, &["package"], &[]);
        assert_eq!(first.code, 0, "{case}: {}", first.said());
        let map = r.map();
        assert!(
            map["units"]["dist/src/test/java/dist/BundleTest.java"]["reads"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("cli/src/main/java/cli/Cli.java")),
            "{case}: {map:#}"
        );
        r.write(
            "cli/src/main/java/cli/Cli.java",
            "package cli;\n\npublic class Cli {\n  public static void main(String[] a) { System.out.print(2); }\n}\n",
        );
        let second = run(&r, &["package"], &[]);
        assert_eq!(second.code, 1, "{case}: {}", second.said());
    }
}

// covers: C-39, C-47
#[test]
fn a_suite_classs_cases_no_test_file_owns_are_its_own_and_it_is_never_counted_as_reused() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&suites("{mode: reuse, unit: test}"), maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // The suite's cases are the suite class's tests, by the class each names; no unit is made of
    // a class no test file is.
    let map = r.map();
    let units: Vec<&String> = map["units"].as_object().unwrap().keys().collect();
    assert_eq!(
        units,
        [
            "core/src/test/java/shop/core/AskTest.java",
            CORE_CALC_TEST,
            SUITE_TEST,
            "other/src/test/java/shop/other/CalcTest.java"
        ],
        "{map:#}"
    );
    let suite_tests: Vec<&String> = map["units"][SUITE_TEST]["tests"]
        .as_object()
        .unwrap_or_else(|| panic!("{map:#}"))
        .keys()
        .collect();
    assert_eq!(
        suite_tests,
        [
            "testlib.Checks.testOne",
            "testlib.Checks.testTwo",
            "testlib.Gone.check"
        ],
        "{map:#}"
    );

    // A change in `other`: core's plain classes are reused, the suite class runs, as the hook
    // can't leave it out, and the plan says so.
    r.write(
        "other/src/test/java/shop/other/CalcTest.java",
        "package shop.other;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {\n  @Test public void adds() { assertEquals(3, 2 + 1); }\n}\n",
    );
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        classes(&second.stdout).contains_key("shop.core.MapSuiteTest"),
        "{:?}",
        classes(&second.stdout)
    );
    // Its three tests run, other's one, and the class of core's two the audit takes.
    assert_eq!(
        second.line("unit: "),
        "unit: 5 of 6 tests run, in 3 of 4 test classes; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    // \[C-37\] What `hushrun why` says of the suite class is end to end: e2e_maven.rs.

    // The suite's cases fail, one of which names no class: the suite class failed, and the plain
    // classes of a module that ran keep their passes.
    r.write(
        CORE_CALC_TEST,
        "package shop.core;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {\n  @Test public void adds() { assertEquals(2, 0 + 2); }\n}\n",
    );
    let failed = run(&r, &["test"], &[(OTHER, "1")]);
    assert_eq!(failed.code, 1, "{}", failed.said());
    assert!(
        classes(&failed.stdout).contains_key("shop.core.CalcTest"),
        "{:?}",
        classes(&failed.stdout)
    );
    let map = r.map();
    assert_eq!(
        map["units"][CORE_CALC_TEST]["tests"]["adds"], "passed",
        "{map:#}"
    );
    assert_eq!(
        map["units"][SUITE_TEST]["tests"]["testlib.Gone.check"], "failed",
        "{map:#}"
    );
}

// covers: C-30, C-47
#[test]
fn a_test_class_of_an_execution_a_run_didnt_reach_stays_a_test_class() {
    let Some(maven) = maven() else { return };
    // lib's integration test, which Failsafe runs after Surefire's tests passed.
    let files = failsafe("{mode: reuse, ignore: ['notes/**']}");
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["verify"], &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(
        classes(&first.stdout).contains_key("shop.lib.PriceIT"),
        "{:?}",
        classes(&first.stdout)
    );

    // lib changes and its PriceTest fails: Surefire stops the build, and Failsafe never runs
    // PriceIT. It is a test class all the same, and keeps the pass it had.
    let price_test = files
        .iter()
        .find(|(path, _)| path == PRICE_TEST)
        .map(|(_, text)| text.clone())
        .unwrap();
    r.write(PRICE_TEST, &format!("{price_test}// changed\n"));
    let failed = run(&r, &["verify"], &[(FAIL, "1")]);
    assert_eq!(failed.code, 1, "{}", failed.said());
    assert!(
        !classes(&failed.stdout).contains_key("shop.lib.PriceIT"),
        "{:?}",
        classes(&failed.stdout)
    );
    let map = r.map();
    let sources: Vec<&str> = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!sources.contains(&PRICE_IT), "{map:#}");
    // Back at the files that passed, before the run. (That `hushrun why` says what passed on
    // them would be reused is end to end: e2e_maven.rs.)
    r.write(PRICE_TEST, &price_test);
    r.write("notes/n.txt", "2\n");

    let back = run(&r, &["verify"], &[]);
    assert_eq!(back.code, 0, "{}", back.said());
    let line = back.line("unit: ");
    assert!(!line.contains("none had passed"), "{line}");
    at_most_the_audit(&classes(&back.stdout), &[]);
}

// covers: K-10
#[test]
fn a_package_stores_tail_in_maven_opts_changes_no_units_key_or_reads() {
    let Some(maven) = maven() else { return };
    let shared = maven.m2.clone();
    let mvn = maven.mvn.clone();
    let r = Reactor::new(&junit4("reuse"), maven);
    // The same COMMAND both times, its own repository empty: only MAVEN_OPTS names the tail, as
    // `hushrun run` gives Maven a package store's part (K-10). First the tests' repository.
    let local = format!("-Dmaven.repo.local={}", r.root.join("m2").display());
    let command = [mvn.as_str(), "-B", "-o", local.as_str(), "test"];
    let tail = |at: &std::path::Path| format!("-Dmaven.repo.local.tail={}", at.display());
    let first = run_command(&r, &command, &[("MAVEN_OPTS", &tail(&shared))]);
    assert_eq!(first.code, 0, "{}", first.said());
    let before = r.map()["units"].clone();
    assert!(
        before.as_object().is_some_and(|u| !u.is_empty()),
        "{before:#}"
    );
    // Then a store's part at another path, the same packages: no unit's key or reads changes.
    let part = r.root.join("packages/parts/maven");
    std::fs::create_dir_all(&part).unwrap();
    std::os::unix::fs::symlink(&shared, part.join("repository")).unwrap();
    let second = run_command(
        &r,
        &command,
        &[("MAVEN_OPTS", &tail(&part.join("repository")))],
    );
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        r.map()["units"],
        before,
        "the store changed a unit's key or reads"
    );
}

//! `HUSHRUN_AGENT` (ADR 0046), end to end: one string in a sandbox's environment records and reads
//! results, against a fake server that takes the agent's token and, as hushrun cloud does, records
//! only what the agent's key signed. The private key leaves no process here: the tests read every
//! byte of every request, and everything printed.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use hushrun_attest::SigningKey;
use hushrun_core::agent::AgentString;
use hushrun_testkit::fake_ledger::Role;
use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use hushrun_testkit::http::Request;
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::Value;

/// Nothing listens on the discard port, so no test reaches the real hosted backend.
const NO_HOST: &str = "http://127.0.0.1:9";
/// A command a test can tell ran: it prints this.
const UNIT: &[&str] = &["run", "unit", "--", "git", "--version"];
const RAN: &str = "git version";

/// What came of a command.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn of(cmd: &mut assert_cmd::Command) -> Ran {
        let output = cmd.output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn lines_with(&self, text: &str) -> usize {
        self.stderr.lines().filter(|l| l.contains(text)).count()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}\n{}", self.stdout, self.stderr))
    }
}

/// A repository in a sandbox: the code, and a home of its own. Nothing in it names a backend.
struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        std::fs::write(repo.join("lib.txt"), "one\n").expect("write");
        git::commit_all(&repo, "init");
        Sandbox {
            _dir: dir,
            root,
            repo,
        }
    }

    /// `hushrun` with no environment but `PATH`, a home of `name`'s own, and `vars`. No login, no
    /// cache and no key are around unless a test puts them there, so `vars` is all it has.
    fn hushrun_in(&self, name: &str, vars: &[(&str, &str)]) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.env_clear()
            .current_dir(&self.repo)
            .env("PATH", std::env::var_os("PATH").expect("PATH"))
            .env("HOME", self.root.join(name))
            .env("XDG_CACHE_HOME", self.root.join(name).join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join(name).join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HUSHRUN_HOST", NO_HOST);
        for (name, value) in vars {
            cmd.env(name, value);
        }
        cmd
    }

    fn hushrun(&self, vars: &[(&str, &str)]) -> assert_cmd::Command {
        self.hushrun_in("home", vars)
    }

    /// A person's login for `origin`, as `hushrun login` saves it, and `backend` as the last one
    /// signed in to: the file to name as `HUSHRUN_CREDENTIALS`.
    fn a_persons_login(&self, origin: &str, backend: &str) -> PathBuf {
        let file = self.root.join("credentials.json");
        let saved = serde_json::json!({
            "backend": backend,
            "hosts": {origin: {"token": FakeResults::TOKEN, "email": "ana@acme.dev"}},
        });
        std::fs::write(&file, saved.to_string()).expect("write");
        file
    }

    /// Writes a `package.json` that names `backend`, as `hushrun init` commits one.
    fn name_backend(&self, backend: &str) {
        std::fs::write(
            self.repo.join("package.json"),
            format!(r#"{{"name": "shop", "hushrun": {{"backend": "{backend}"}}}}"#),
        )
        .expect("write");
        git::commit_all(&self.repo, "a backend");
    }
}

/// `http://127.0.0.1:<port>` of an org's URL.
fn origin_of_org(url: &str) -> &str {
    &url[..url.find("/o/").expect("an org's URL")]
}

/// A key as `hushrun token create` makes one.
fn agent_key() -> SigningKey {
    SigningKey::generate_p256().expect("a P-256 key")
}

/// What must stay in the process, and how it could look on its way out.
struct Secrets {
    /// Never in a request, and never printed.
    never_sent: Vec<Vec<u8>>,
    /// Goes to the backend, as it must, and is never printed.
    token: String,
}

impl Secrets {
    fn of(string: &str, key: &SigningKey, token: &str) -> Secrets {
        let pem = key.to_pem().to_string();
        let lines: Vec<&str> = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let body = lines.concat();
        let der = base64::engine::general_purpose::STANDARD
            .decode(&body)
            .expect("a PEM body");
        let mut never_sent: Vec<Vec<u8>> = vec![
            string.as_bytes().to_vec(),
            // The string without its prefix, and pieces of it, as a log might cut it.
            string.as_bytes()["hushrun_agent_v1_".len()..].to_vec(),
            string.as_bytes()[..40].to_vec(),
            pem.clone().into_bytes(),
            pem.replace('\n', "\\n").into_bytes(),
            body.clone().into_bytes(),
            der,
        ];
        never_sent.extend(lines.iter().map(|line| line.as_bytes().to_vec()));
        Secrets {
            never_sent,
            token: token.to_string(),
        }
    }

    fn in_bytes<'a>(&'a self, bytes: &[u8]) -> Option<&'a [u8]> {
        self.never_sent
            .iter()
            .find(|secret| bytes.windows(secret.len()).any(|w| w == secret.as_slice()))
            .map(Vec::as_slice)
    }

    /// Every byte of `requests`: method, path, query, each header's name and value, and body.
    fn assert_not_in_requests(&self, requests: &[Request], what: &str) {
        for request in requests {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(request.method.as_bytes());
            bytes.extend_from_slice(request.path.as_bytes());
            for (name, value) in request.query.iter().chain(&request.headers) {
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(b'\n');
                bytes.extend_from_slice(value.as_bytes());
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&request.body);
            if let Some(found) = self.in_bytes(&bytes) {
                panic!(
                    "{what}: {} {} holds {}",
                    request.method,
                    request.path,
                    String::from_utf8_lossy(found)
                );
            }
        }
    }

    fn assert_not_printed(&self, ran: &Ran, what: &str) {
        for text in [&ran.stdout, &ran.stderr] {
            if let Some(found) = self.in_bytes(text.as_bytes()) {
                panic!("{what} printed {}:\n{text}", String::from_utf8_lossy(found));
            }
            assert!(
                !text.contains(&self.token),
                "{what} printed the token:\n{text}"
            );
        }
    }
}

// covers: C-11, C-13, C-17, C-18
#[test]
fn one_variable_records_a_result_and_reads_it_back() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let s = Sandbox::new();
    // A sandbox that sets CI=true, and has nothing else: the repository names no backend.
    let vars = [("HUSHRUN_AGENT", string.as_str()), ("CI", "true")];

    let first = Ran::of(s.hushrun_in("one", &vars).args(UNIT));
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains(RAN), "{}", first.stdout);
    assert!(
        first.stderr.contains("it won't run again on this code"),
        "{}",
        first.stderr
    );
    let recorded = fake.recorded();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    // It means local: a pass here counts in CI, as a laptop's does (C-10).
    assert_eq!(recorded[0]["provider"], "local", "{}", recorded[0]);
    assert!(recorded[0].get("job").is_none(), "{}", recorded[0]);

    // Another sandbox, with a home and a cache of its own: the server is all they share.
    let second = Ran::of(s.hushrun_in("two", &vars).args(UNIT));
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(!second.stdout.contains(RAN), "{}", second.stdout);
    assert!(second.stderr.contains("unit skipped"), "{}", second.stderr);

    // The agent's token went with every request, and the fake took only what its key signed.
    let bearer = format!("Bearer {}", FakeResults::AGENT_TOKEN);
    let requests = fake.requests();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str())),
        "{requests:?}"
    );

    // A refusal names the variable that was set, the only one the sandbox has.
    fake.answer(
        "GET",
        "v0/results",
        401,
        serde_json::json!({"error": {"code": "unauthenticated", "message": "this token expired"}}),
        1,
    );
    let refused =
        Ran::of(
            s.hushrun_in("three", &vars)
                .args(["run", "refused", "--", "git", "--version"]),
        );
    assert!(
        refused
            .stderr
            .contains("refused running: acme refused HUSHRUN_AGENT: this token expired"),
        "{}",
        refused.stderr
    );
}

// covers: C-18 (the dashboard's strings hold an Ed25519 key)
#[test]
fn a_string_from_the_dashboard_signs_too() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&SigningKey::from_seed(&[7; 32]));
    let s = Sandbox::new();
    let ran = Ran::of(s.hushrun(&[("HUSHRUN_AGENT", &string)]).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(fake.recorded().len(), 1, "{}", ran.stderr);
}

// covers: C-11, C-13
#[test]
fn the_backend_in_the_string_beats_the_repositorys_and_gets_the_token() {
    let fake = FakeResults::start();
    let elsewhere = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let s = Sandbox::new();
    s.name_backend(&elsewhere.url());
    let ran = Ran::of(s.hushrun(&[("HUSHRUN_AGENT", &string)]).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(!ran.stderr.contains("isn't sent"), "{}", ran.stderr);
    assert_eq!(fake.recorded().len(), 1);
    assert!(elsewhere.requests().is_empty());
}

// covers: C-13
#[test]
fn the_token_in_the_string_goes_to_no_other_origin() {
    // hushrun's host is one server and the string names another: an agent token from
    // HUSHRUN_TOKEN follows a repository's backend to the host, and this one must not.
    let fake = FakeResults::start();
    let host = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let s = Sandbox::new();
    s.name_backend(&host.url());
    let origin = host.url();
    let origin = &origin[..origin.find("/o/").expect("an org's URL")];
    let mut cmd = s.hushrun(&[("HUSHRUN_AGENT", &string)]);
    cmd.env("HUSHRUN_HOST", origin);
    let ran = Ran::of(cmd.args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(host.requests().is_empty());
}

// covers: C-17
#[test]
fn whoami_and_doctor_say_where_it_all_came_from() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let s = Sandbox::new();
    let vars = [("HUSHRUN_AGENT", string.as_str())];
    let backend = fake.url();

    let whoami = Ran::of(
        s.hushrun(&vars)
            .args(["whoami", "--now", "2026-10-20T10:00:00Z"]),
    );
    assert_eq!(whoami.code, 0, "{}", whoami.stderr);
    let (text, machine) = whoami.stdout.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(
        format!("{text}\n"),
        format!(
            "backend   {backend} (HUSHRUN_AGENT names it)\n\
             token     HUSHRUN_AGENT, an agent token\n\
             as        agent:fleet-a in acme\n\
             agent     fleet-a ({}…)\n\
             for       the whole org\n\
             expires   2026-11-19 10:00 UTC (in 30 days)\n\
             limit     300 results a minute\n",
            &FakeResults::AGENT_TOKEN[..10]
        )
    );
    assert!(
        machine.starts_with("machine   sha256:") && machine.ends_with("… (HUSHRUN_AGENT)"),
        "{machine}"
    );

    let json = Ran::of(s.hushrun(&vars).args(["whoami", "--format", "json"]));
    let answer = json.json();
    schema::assert_valid(&schema::load("whoami.v1.schema.json"), &answer);
    assert_eq!(answer["named_by"], "HUSHRUN_AGENT");
    assert_eq!(answer["credential"], "agent_token");
    assert_eq!(answer["as"], "agent:fleet-a");
    assert_eq!(answer["machine"]["keyid"], key.public_key().keyid());
    assert_eq!(answer["machine"]["source"], "HUSHRUN_AGENT");

    let doctor = Ran::of(s.hushrun(&vars).args(["doctor", "--format", "json"]));
    let checks = doctor.json();
    schema::assert_valid(&schema::load("doctor.v1.schema.json"), &checks);
    let detail = |name: &str| {
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == name)
            .map(|check| check["detail"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("no {name} in {checks}"))
    };
    assert_eq!(detail("ci"), "local run (not CI), as HUSHRUN_AGENT says");
    assert!(
        detail("results").starts_with(&format!(
            "{backend} as agent:fleet-a with HUSHRUN_AGENT, an agent token, for the whole org"
        )),
        "{}",
        detail("results")
    );
}

// covers: C-13, C-17, C-18
#[test]
fn no_request_and_no_line_holds_the_key_or_the_string() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let s = Sandbox::new();
    let vars = [("HUSHRUN_AGENT", string.as_str())];
    let mut sent = 0;

    for args in [
        UNIT,
        // Run again, skipped; forced; and failing.
        UNIT,
        &["run", "unit", "--force", "--", "git", "--version"],
        &["run", "bad", "--", "git", "no-such-command"],
        &["whoami"],
        &["whoami", "--format", "json"],
        &["doctor"],
        &["doctor", "--format", "json"],
        &["ready"],
        &["ready", "--format", "json"],
        &["repository", "usage"],
        &["repository", "usage", "--format", "json"],
        // Every level of logging hushrun has.
        &["-vv", "run", "traced", "--", "git", "--version"],
        &["-vv", "whoami"],
    ] {
        let ran = Ran::of(s.hushrun(&vars).args(args));
        secrets.assert_not_printed(&ran, &format!("{args:?}"));
        sent += 1;
    }
    let mut traced = s.hushrun(&vars);
    traced
        .env("HUSHRUN_LOG", "trace")
        .args(["run", "traced-more", "--", "git", "--version"]);
    secrets.assert_not_printed(&Ran::of(&mut traced), "HUSHRUN_LOG=trace");
    let mut hook = s.hushrun(&vars);
    hook.args(["hook", "pre-push", "origin", "url"])
        .write_stdin("refs/heads/a 0123 refs/heads/a 0000\n");
    secrets.assert_not_printed(&Ran::of(&mut hook), "the hook");

    // When the server refuses, in every way a run reports.
    for (status, code) in [
        (401, "unauthenticated"),
        (403, "forbidden"),
        (429, "rate_limited"),
        (500, "broken"),
    ] {
        let body = serde_json::json!({"error": {"code": code, "message": "no"}});
        fake.answer("GET", "v0/results", status, body.clone(), 1);
        let ran =
            Ran::of(
                s.hushrun(&vars)
                    .args(["run", "refused", "--force", "--", "git", "--version"]),
            );
        secrets.assert_not_printed(&ran, &format!("a run refused with {status}"));
        fake.answer("POST", "v0/results", status, body, 1);
        let ran =
            Ran::of(
                s.hushrun(&vars)
                    .args(["run", "refused", "--force", "--", "git", "--version"]),
            );
        secrets.assert_not_printed(&ran, &format!("a record refused with {status}"));
    }

    let requests = fake.raw_requests();
    assert!(
        sent > 10 && requests.len() > 10,
        "only {} requests",
        requests.len()
    );
    assert!(
        requests.iter().any(|r| r.method == "POST"),
        "nothing recorded"
    );
    secrets.assert_not_in_requests(&requests, "results");
    // The token is what authorizes, and the signature proves the key without holding it.
    assert!(
        requests
            .iter()
            .filter(|r| r.method == "POST")
            .all(|r| r.header("hushrun-signature").is_some()),
        "{requests:?}"
    );
}

/// A sandbox's `hushrun` in a pipeline: `w`'s server, on the origin of the string's backend, knows
/// the agent's token, and the agent has no other credential or signer.
fn piped(w: &World, string: &str) -> assert_cmd::Command {
    let mut cmd = w.hushrun();
    cmd.env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .env("HUSHRUN_AGENT", string);
    cmd
}

/// The world's server, `http://127.0.0.1:<port>`.
fn origin_of(url: &str) -> &str {
    &url[..url.find("/l/").expect("a repository URL")]
}

const PIPELINE_TOKEN: &str = "hra_pipe00123456789abcdefghijABCDEFGHIJ0123";

// covers: C-13, C-18, L-10
#[test]
fn the_pipeline_paths_send_the_token_and_never_the_key() {
    let w = World::new("acme-api.yaml");
    let key = agent_key();
    let string = AgentString::encode(
        &format!("{}/o/acme", origin_of(&w.ledger_url())),
        PIPELINE_TOKEN,
        key.to_pem().as_str(),
    );
    let secrets = Secrets::of(&string, &key, PIPELINE_TOKEN);
    w.fake.add_token(PIPELINE_TOKEN, Role::Writer);
    w.fake
        .register_machine(&key.public_key(), "agent:fleet-a", false);
    let plan_file = w.path("plan.json");
    let plan_file = plan_file.to_str().unwrap();

    let plan = Ran::of(piped(&w, &string).args(["plan", "--format", "json", "--out", plan_file]));
    assert_eq!(plan.code, 0, "{}", plan.stderr);
    secrets.assert_not_printed(&plan, "plan");
    let attest =
        Ran::of(piped(&w, &string).args(["attest", "--plan", plan_file, "--check", "unit=pass"]));
    assert_eq!(attest.code, 0, "{}", attest.stderr);
    secrets.assert_not_printed(&attest, "attest");
    assert!(!w.keys("v1/events/").is_empty(), "attest wrote no event");
    for args in [
        &["status"][..],
        &["status", "--format", "json"],
        &["doctor"],
        &["doctor", "--format", "json"],
        &["whoami", "--format", "json"],
        &["-vv", "plan"],
    ] {
        let ran = Ran::of(piped(&w, &string).args(args));
        secrets.assert_not_printed(&ran, &format!("{args:?}"));
    }

    // doctor names the agent's key as the signer, which the server lists.
    let doctor = Ran::of(piped(&w, &string).args(["doctor", "--format", "json"]));
    let checks = doctor.json();
    let signer = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "signer")
        .unwrap_or_else(|| panic!("no signer in {checks}"));
    assert_eq!(signer["status"], "ok", "{signer}");
    assert!(
        signer["detail"].as_str().unwrap().contains("agent:fleet-a"),
        "{signer}"
    );

    // The event is the agent's: signed by the key in the string.
    let events = w.keys("v1/events/");
    let event = String::from_utf8_lossy(&w.object(&events[0])).into_owned();
    assert!(event.contains(&key.public_key().keyid()), "{event}");

    let requests = w.fake.raw_requests();
    assert!(requests.len() > 5, "only {} requests", requests.len());
    secrets.assert_not_in_requests(&requests, "the ledger");
    // The ledger is on the string's origin, so its token went there, and nothing else did.
    let bearer = format!("Bearer {PIPELINE_TOKEN}");
    let carried: Vec<_> = requests
        .iter()
        .filter_map(|r| r.header("authorization"))
        .collect();
    assert!(!carried.is_empty());
    assert!(carried.iter().all(|t| *t == bearer), "{carried:?}");
}

// covers: C-13, L-10
#[test]
fn a_ledger_on_another_origin_doesnt_get_the_strings_token() {
    let w = World::new("acme-api.yaml");
    let key = agent_key();
    // The string's backend is elsewhere, and the ledger on hushrun's host, as far as this run
    // knows. Only the string's own origin gets its token.
    let string = AgentString::encode(
        "http://127.0.0.1:9/o/acme",
        PIPELINE_TOKEN,
        key.to_pem().as_str(),
    );
    let mut cmd = piped(&w, &string);
    cmd.env("HUSHRUN_HOST", origin_of(&w.ledger_url()))
        .args(["plan"]);
    let _ = Ran::of(&mut cmd);
    let requests = w.fake.raw_requests();
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|r| r.header("authorization").is_none()),
        "{requests:?}"
    );
}

// covers: C-13, L-10
#[test]
fn a_string_in_a_tokens_place_is_never_sent() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let s = Sandbox::new();

    // Alone, and as a value goes astray: docker's --env-file keeps quotes, and people paste.
    let url = fake.url();
    for value in wrapped(&string) {
        let vars = [("HUSHRUN_TOKEN", value.as_str()), ("HUSHRUN_BACKEND", &url)];
        let ran = Ran::of(s.hushrun(&vars).args(UNIT));
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        assert!(ran.stdout.contains(RAN), "{}", ran.stdout);
        assert_eq!(
            ran.lines_with(
                "HUSHRUN_TOKEN holds a HUSHRUN_AGENT string, which is never sent: set it as HUSHRUN_AGENT"
            ),
            1,
            "{}",
            ran.stderr
        );
        secrets.assert_not_printed(&ran, "a run");
    }
    secrets.assert_not_in_requests(&fake.raw_requests(), "a string in HUSHRUN_TOKEN");
    assert!(fake.recorded().is_empty());

    // In a login's place, which no variable holds, the ledger client itself refuses.
    let login = s.a_persons_login(origin_of_org(&url), &url);
    let held = std::fs::read_to_string(&login)
        .unwrap()
        .replace(FakeResults::TOKEN, &format!("Bearer {string}"));
    std::fs::write(&login, held).unwrap();
    let mut cmd = s.hushrun(&[]);
    cmd.env("HUSHRUN_CREDENTIALS", &login);
    let ran = Ran::of(cmd.args(["run", "login", "--", "git", "--version"]));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    secrets.assert_not_printed(&ran, "a run with a login");
    secrets.assert_not_in_requests(&fake.raw_requests(), "a string in a login");

    // On the pipeline paths, in the token that goes to a ledger, too.
    let w = World::new("acme-api.yaml");
    let ran = Ran::of(
        w.hushrun()
            .env("HUSHRUN_REPOSITORY_TOKEN", &string)
            .args(["plan"]),
    );
    assert_ne!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr
            .contains("HUSHRUN_REPOSITORY_TOKEN holds a HUSHRUN_AGENT string, which is never sent"),
        "{}",
        ran.stderr
    );
    secrets.assert_not_printed(&ran, "a plan");
    // What a token command prints: only the ledger client sees that one, whatever is around it.
    let command = w.path("token-command.sh");
    std::fs::write(&command, format!("#!/bin/sh\necho '\"{string}\"'\n")).unwrap();
    make_executable(&command);
    let ran = Ran::of(
        w.hushrun()
            .env_remove("HUSHRUN_REPOSITORY_TOKEN")
            .env("HUSHRUN_REPOSITORY_TOKEN_COMMAND", &command)
            .args(["plan"]),
    );
    assert_ne!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr
            .contains("HUSHRUN_REPOSITORY_TOKEN_COMMAND holds a HUSHRUN_AGENT string"),
        "{}",
        ran.stderr
    );
    secrets.assert_not_printed(&ran, "a plan with a token command");
    secrets.assert_not_in_requests(&w.fake.raw_requests(), "a string in a ledger token");
}

// covers: C-13, C-17
#[test]
fn the_string_wins_over_what_is_beside_it_and_says_so_once() {
    let fake = FakeResults::start();
    let elsewhere = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let s = Sandbox::new();
    let other_key = agent_key();
    let other_pem = other_key.to_pem().to_string();
    let other_url = elsewhere.url();
    let beside = [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_TOKEN", "hra_a-stale-token"),
        ("HUSHRUN_MACHINE_KEY", other_pem.as_str()),
        ("HUSHRUN_MACHINE_KEY_FILE", "/nowhere/key.pem"),
        ("HUSHRUN_BACKEND", other_url.as_str()),
    ];

    let says = "hushrun: HUSHRUN_AGENT wins over HUSHRUN_TOKEN, HUSHRUN_MACHINE_KEY, HUSHRUN_MACHINE_KEY_FILE and HUSHRUN_BACKEND: it holds the backend, the token and the key together";
    let ran = Ran::of(s.hushrun(&beside).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.lines_with(says), 1, "{}", ran.stderr);
    // The string's token, signed with the string's key, at the string's backend.
    assert_eq!(fake.recorded().len(), 1);
    assert!(elsewhere.requests().is_empty());
    let bearer = format!("Bearer {}", FakeResults::AGENT_TOKEN);
    assert!(
        fake.requests()
            .iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str()))
    );
    let whoami = Ran::of(s.hushrun(&beside).args(["whoami", "--format", "json"]));
    assert_eq!(whoami.json()["machine"]["keyid"], key.public_key().keyid());
    assert_eq!(whoami.lines_with(says), 1, "{}", whoami.stderr);

    // -q silences it; the same values beside it have nothing to say.
    let quiet = Ran::of(
        s.hushrun(&beside)
            .args(["-q", "run", "quiet", "--", "git", "--version"]),
    );
    assert!(quiet.stderr.is_empty(), "{}", quiet.stderr);
    let pem = key.to_pem().to_string();
    let same = [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_MACHINE_KEY", pem.as_str()),
    ];
    let ran = Ran::of(s.hushrun(&same).args(["whoami"]));
    assert!(!ran.stderr.contains("wins over"), "{}", ran.stderr);

    // An empty HUSHRUN_BACKEND still turns results off.
    let before = fake.requests().len();
    let off = [("HUSHRUN_AGENT", string.as_str()), ("HUSHRUN_BACKEND", "")];
    let ran = Ran::of(s.hushrun(&off).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains(RAN), "{}", ran.stdout);
    assert_eq!(fake.requests().len(), before);

    // An explicit HUSHRUN_PROVIDER still wins.
    let ci = [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_PROVIDER", "generic"),
    ];
    let ran = Ran::of(
        s.hushrun(&ci)
            .args(["run", "as-ci", "--", "git", "--version"]),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let recorded = fake.recorded();
    let last = recorded
        .iter()
        .find(|r| r["check"] == "as-ci")
        .expect("recorded");
    assert_eq!(last["provider"], "generic", "{last}");
}

// covers: C-10, C-17
#[test]
fn real_ci_outside_github_behaves_as_it_did() {
    // Bare `CI=true`, an agent token and its key, with no HUSHRUN_AGENT and no HUSHRUN_PROVIDER.
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let pem = key.to_pem().to_string();
    let backend = fake.url();
    let s = Sandbox::new();
    let vars = [
        ("CI", "true"),
        ("HUSHRUN_RUN_ID", "1234"),
        ("HUSHRUN_BACKEND", backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_MACHINE_KEY", pem.as_str()),
    ];
    let ran = Ran::of(s.hushrun(&vars).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(!ran.stderr.contains("HUSHRUN_AGENT"), "{}", ran.stderr);
    let recorded = fake.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["provider"], "generic", "{}", recorded[0]);
    let whoami = Ran::of(s.hushrun(&vars).args(["whoami"]));
    assert!(
        whoami
            .stdout
            .contains(&format!("{backend} (HUSHRUN_BACKEND names it)")),
        "{}",
        whoami.stdout
    );
    assert!(
        whoami.stdout.contains("HUSHRUN_TOKEN, an agent token"),
        "{}",
        whoami.stdout
    );
    assert!(
        whoami.stdout.contains("(HUSHRUN_MACHINE_KEY)\n"),
        "{}",
        whoami.stdout
    );
    let doctor = Ran::of(s.hushrun(&vars).args(["doctor"]));
    assert!(
        doctor.stdout.contains("generic run 1234"),
        "{}",
        doctor.stdout
    );
    // The string beside the same variables makes the same machine a sandbox: only it does.
    let mut with_string = vars.to_vec();
    with_string.push(("HUSHRUN_AGENT", string.as_str()));
    let doctor = Ran::of(s.hushrun(&with_string).args(["doctor"]));
    assert!(
        doctor.stdout.contains("local run (not CI)"),
        "{}",
        doctor.stdout
    );
}

// covers: C-13, C-17, C-18
#[test]
fn the_old_form_keeps_working_in_a_sandbox_set_up_with_it() {
    let fake = FakeResults::start();
    let key = agent_key();
    let _ = fake.agent_string(&key);
    let pem = key.to_pem().to_string();
    let backend = fake.url();
    let s = Sandbox::new();
    let vars = [
        ("CI", "true"),
        ("HUSHRUN_PROVIDER", "local"),
        ("HUSHRUN_BACKEND", backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_MACHINE_KEY", pem.as_str()),
    ];
    let ran = Ran::of(s.hushrun(&vars).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(!ran.stderr.contains("HUSHRUN_AGENT"), "{}", ran.stderr);
    let recorded = fake.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["provider"], "local");

    // The key in a file does too.
    let file = s.root.join("agent-key.pem");
    std::fs::write(&file, &pem).expect("write");
    let vars = [
        ("HUSHRUN_BACKEND", backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_MACHINE_KEY_FILE", file.to_str().unwrap()),
    ];
    let ran = Ran::of(
        s.hushrun(&vars)
            .args(["run", "unit", "--force", "--", "git", "--version"]),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(fake.recorded().len(), 2);
}

// covers: C-14, C-17
#[test]
fn a_string_hushrun_cant_read_says_whats_wrong_and_the_check_still_runs() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let cut = &string[..string.len() - 11];
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let s = Sandbox::new();
    let vars = [("HUSHRUN_AGENT", cut), ("CI", "true")];

    let ran = Ran::of(s.hushrun(&vars).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains(RAN), "{}", ran.stdout);
    assert_eq!(
        ran.lines_with(
            "hushrun: HUSHRUN_AGENT is damaged: part of it is missing, or something was added, such as a line break, so it's ignored"
        ),
        1,
        "{}",
        ran.stderr
    );
    secrets.assert_not_printed(&ran, "a run");
    assert!(!ran.stderr.contains(cut), "{}", ran.stderr);
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());

    // The exit code stays the command's.
    let failed = Ran::of(
        s.hushrun(&vars)
            .args(["run", "bad", "--", "git", "no-such-command"]),
    );
    assert_ne!(failed.code, 0);
    assert_eq!(failed.lines_with("HUSHRUN_AGENT is damaged"), 1);

    // doctor calls it what it is, and whoami still answers.
    let doctor = Ran::of(s.hushrun(&vars).args(["doctor"]));
    assert_eq!(doctor.code, 2, "{}", doctor.stdout);
    assert!(
        doctor
            .stdout
            .lines()
            .any(|l| l.starts_with("FAIL  agent") && l.contains("HUSHRUN_AGENT is damaged")),
        "{}",
        doctor.stdout
    );
    secrets.assert_not_printed(&doctor, "doctor");
    let whoami = Ran::of(s.hushrun(&vars).args(["whoami"]));
    assert_eq!(whoami.code, 0, "{}", whoami.stderr);
    secrets.assert_not_printed(&whoami, "whoami");
    // It doesn't tell an agent to sign in, which it refuses.
    assert!(
        !whoami.stdout.contains("hushrun login"),
        "{}",
        whoami.stdout
    );
    let elsewhere = Sandbox::new();
    elsewhere.name_backend(&fake.url());
    let whoami = Ran::of(elsewhere.hushrun(&vars).args(["whoami"]));
    assert!(
        whoami.stdout.contains(
            "token     none: HUSHRUN_AGENT can't be read, and only it speaks for an agent"
        ),
        "{}",
        whoami.stdout
    );

    // Old variables beside it carry on.
    let pem = key.to_pem().to_string();
    let backend = fake.url();
    let both = [
        ("HUSHRUN_AGENT", "not a string"),
        ("HUSHRUN_BACKEND", backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_MACHINE_KEY", pem.as_str()),
    ];
    let ran = Ran::of(s.hushrun(&both).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.lines_with(
            "hushrun: HUSHRUN_AGENT doesn't start with hushrun_agent_v1_, as the string `hushrun token create --agent` shows does, so it's ignored"
        ),
        1,
        "{}",
        ran.stderr
    );
    assert_eq!(fake.recorded().len(), 1);
}

// covers: C-14, C-18
#[test]
fn a_string_whose_key_cant_sign_records_nothing_and_says_why() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let broken = AgentString::encode(
        &fake.url(),
        FakeResults::AGENT_TOKEN,
        "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
    );
    assert_ne!(string, broken);
    let s = Sandbox::new();
    let ran = Ran::of(s.hushrun(&[("HUSHRUN_AGENT", &broken)]).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains(RAN), "{}", ran.stdout);
    let advice = "not recorded: HUSHRUN_AGENT's key can't sign for its token: make a new string with `hushrun token create --agent`";
    assert!(ran.stderr.contains(advice), "{}", ran.stderr);
    assert!(fake.recorded().is_empty());
    assert!(
        !ran.stderr.contains("HUSHRUN_MACHINE_KEY"),
        "{}",
        ran.stderr
    );

    // whoami says the same, and no variable it couldn't use.
    let vars = [("HUSHRUN_AGENT", broken.as_str())];
    let whoami = Ran::of(s.hushrun(&vars).args(["whoami"]));
    assert!(
        whoami.stdout.ends_with(
            "machine   none: HUSHRUN_AGENT's key can't sign for its token: make a new string with `hushrun token create --agent`\n"
        ),
        "{}",
        whoami.stdout
    );

    // doctor tries the key, rather than the string's form alone.
    let doctor = Ran::of(s.hushrun(&vars).args(["doctor"]));
    assert_eq!(doctor.code, 2, "{}", doctor.stdout);
    assert!(
        doctor.stdout.lines().any(|l| l.starts_with("FAIL  agent")
            && l.contains("make a new string with `hushrun token create --agent`")),
        "{}",
        doctor.stdout
    );
    let good = Ran::of(s.hushrun(&[("HUSHRUN_AGENT", &string)]).args(["doctor"]));
    assert!(
        good.stdout
            .lines()
            .any(|l| l.starts_with("ok    agent") && l.contains("can sign")),
        "{}",
        good.stdout
    );

    // Another key than the token's: the server's 401 gets the same advice, which is what's true
    // for a string, whose key no variable can replace.
    let other = AgentString::encode(
        &fake.url(),
        FakeResults::AGENT_TOKEN,
        agent_key().to_pem().as_str(),
    );
    let ran = Ran::of(s.hushrun(&[("HUSHRUN_AGENT", &other)]).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stderr.contains(advice), "{}", ran.stderr);
    assert!(
        !ran.stderr.contains("HUSHRUN_MACHINE_KEY"),
        "{}",
        ran.stderr
    );
    assert!(fake.recorded().is_empty());
}

// covers: C-14, C-17
#[test]
fn a_token_where_the_string_goes_says_what_to_do() {
    // `HUSHRUN_AGENT=$(hushrun token create …)`, whose stdout is the token.
    let s = Sandbox::new();
    let ran = Ran::of(
        s.hushrun(&[("HUSHRUN_AGENT", FakeResults::AGENT_TOKEN)])
            .args(UNIT),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.lines_with(
            "hushrun: HUSHRUN_AGENT holds an agent token alone, not the whole string: `hushrun token create --agent` shows the string at a terminal, and --format json gives it as hushrun_agent, so it's ignored"
        ),
        1,
        "{}",
        ran.stderr
    );
    assert!(
        !ran.stderr.contains(FakeResults::AGENT_TOKEN),
        "{}",
        ran.stderr
    );
}

/// hushrun's variables, every one it reads: a string in any is dropped, and said once.
const HUSHRUNS_VARIABLES: &[&str] = &[
    "HUSHRUN_BACKEND",
    "HUSHRUN_CACHE_DIR",
    "HUSHRUN_CONFIG",
    "HUSHRUN_CONTAINER_RUNTIME",
    "HUSHRUN_CREDENTIALS",
    "HUSHRUN_CUSTODY",
    "HUSHRUN_EXPORT_DIR",
    "HUSHRUN_FORCE_FULL",
    "HUSHRUN_FORMAT",
    "HUSHRUN_FULCIO_URL",
    "HUSHRUN_GITLAB_TOKEN",
    "HUSHRUN_GIT_READER",
    "HUSHRUN_HOST",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_INSTALLER_GITHUB_BASE_URL",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_LOG",
    "HUSHRUN_MACHINE_KEY",
    "HUSHRUN_MACHINE_KEY_FILE",
    "HUSHRUN_NO_CI_OUTPUT",
    "HUSHRUN_NPM_PACKAGE",
    "HUSHRUN_OIDC_CLIENT_ID",
    "HUSHRUN_OIDC_ISSUER",
    "HUSHRUN_ORG_TOKEN_COMMAND",
    "HUSHRUN_PLAN_JSON",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_REPOSITORY_BASE",
    "HUSHRUN_REPOSITORY_TOKEN",
    "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
    "HUSHRUN_REPOSITORY_URL",
    "HUSHRUN_RESTRICTED_NETWORK",
    "HUSHRUN_RUN_ATTEMPT",
    "HUSHRUN_RUN_ID",
    "HUSHRUN_RUN_URL",
    "HUSHRUN_SIGNING",
    "HUSHRUN_SIGNING_KEY",
    "HUSHRUN_SIGNING_KEY_FILE",
    "HUSHRUN_SIGSTORE_TRUSTED_ROOT",
    "HUSHRUN_SIGSTORE_TUF_ROOT",
    "HUSHRUN_SIGSTORE_TUF_URL",
    "HUSHRUN_TOKEN",
    "HUSHRUN_TSA",
];

/// A string as it goes astray: alone, in the quotes `docker --env-file` keeps, pasted whole, and
/// without its prefix.
fn wrapped(string: &str) -> [String; 5] {
    [
        string.to_string(),
        format!("\"{string}\""),
        format!("Bearer {string}"),
        format!("HUSHRUN_AGENT={string}"),
        string["hushrun_agent_v1_".len()..].to_string(),
    ]
}

// covers: C-13, C-14, C-17
#[test]
fn a_string_in_any_of_hushruns_variables_is_never_printed_or_sent() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let s = Sandbox::new();
    let url = fake.url();
    // A backend and a token of its own, so that what it does with the variable goes somewhere.
    let others = [
        ("HUSHRUN_BACKEND", url.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::TOKEN),
    ];
    for name in HUSHRUNS_VARIABLES {
        for args in [
            &["run", "unit", "--force", "--", "git", "--version"][..],
            &["whoami", "--format", "json"],
            &["doctor"],
            &["keys", "public"],
        ] {
            let ran = Ran::of(s.hushrun(&others).env(name, &string).args(args));
            secrets.assert_not_printed(&ran, &format!("{name}, {args:?}"));
            assert_eq!(
                ran.lines_with(&format!("{name} holds a HUSHRUN_AGENT string")),
                1,
                "{name}, {args:?}: {}",
                ran.stderr
            );
        }
        let quoted = format!("\"{string}\"");
        let ran = Ran::of(s.hushrun(&others).env(name, &quoted).args(UNIT));
        secrets.assert_not_printed(&ran, &format!("{name}, quoted"));
    }
    // With nothing else set, where the cache, the credentials and a repository's backend come in.
    let named = Sandbox::new();
    std::fs::write(
        named.repo.join("package.json"),
        format!(
            r#"{{"name": "shop", "scripts": {{"test": "vitest"}}, "hushrun": {{"backend": "{url}"}}}}"#
        ),
    )
    .unwrap();
    git::commit_all(&named.repo, "a backend");
    for name in HUSHRUNS_VARIABLES {
        for args in [
            &["run", "unit", "--force", "--", "git", "--version"][..],
            &["init", "--dry-run"],
        ] {
            for sandbox in [&s, &named] {
                let ran = Ran::of(sandbox.hushrun(&[]).env(name, &string).args(args));
                secrets.assert_not_printed(&ran, &format!("{name}, {args:?}, alone"));
            }
        }
    }
    // A token that isn't sent names hushrun's host in its line.
    let withheld = Ran::of(
        named
            .hushrun(&[("HUSHRUN_TOKEN", FakeResults::TOKEN)])
            .env("HUSHRUN_HOST", &string)
            .args(UNIT),
    );
    assert!(
        withheld.stderr.contains("isn't sent"),
        "{}",
        withheld.stderr
    );
    secrets.assert_not_printed(&withheld, "a withheld token's line");

    // `update` asks the releases' host, which a dropped variable leaves at the real one: nothing
    // listens on this proxy, so the question goes nowhere.
    let mut update = s.hushrun(&[]);
    update
        .env("HTTPS_PROXY", NO_HOST)
        .env("https_proxy", NO_HOST)
        .env("HUSHRUN_INSTALLER_GITHUB_BASE_URL", &string)
        .arg("update");
    secrets.assert_not_printed(&Ran::of(&mut update), "update");

    let requests = fake.raw_requests();
    assert!(
        requests.len() > HUSHRUNS_VARIABLES.len(),
        "{}",
        requests.len()
    );
    secrets.assert_not_in_requests(&requests, "hushrun's variables");

    // On the pipeline paths, where other readers take them. A dropped variable goes to its default,
    // so what a test mustn't reach stays out: a home of its own holds the default credentials and
    // cache, and doctor, which without HUSHRUN_TSA would ask the public timestamp authority, leaves
    // that variable alone.
    let w = World::new("acme-api.yaml");
    for name in HUSHRUNS_VARIABLES {
        for args in [&["plan"][..], &["doctor"], &["keys", "public"]] {
            // doctor takes a second here, so it reads the ones that decide who signs.
            let signs = matches!(
                *name,
                "HUSHRUN_MACHINE_KEY" | "HUSHRUN_MACHINE_KEY_FILE" | "HUSHRUN_SIGNING_KEY_FILE"
            );
            if args == ["doctor"] && !signs {
                continue;
            }
            let ran = Ran::of(
                w.hushrun()
                    .env("HOME", &w.root)
                    .env("XDG_CACHE_HOME", w.root.join("xdg-cache"))
                    .env("XDG_CONFIG_HOME", w.root.join("xdg-config"))
                    .env(name, &string)
                    .args(args),
            );
            secrets.assert_not_printed(&ran, &format!("{name}, {args:?}"));
        }
    }
    secrets.assert_not_in_requests(&w.fake.raw_requests(), "the ledger");
}

// covers: C-17
#[test]
fn a_string_on_the_command_line_goes_no_further() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let s = Sandbox::new();
    let url = fake.url();
    let says = "error: the command line holds a HUSHRUN_AGENT string, which holds a private key: set it in the environment as HUSHRUN_AGENT. Nothing ran";
    let echo = format!("echo {string}");
    for args in [
        vec!["login", &string],
        vec!["logout", &string],
        vec!["token", "create", "--agent", "fleet", "--backend", &string],
        vec!["token", "list", "--backend", &string],
        vec!["token", "revoke", &string, "--backend", &url],
        vec!["whoami", "--repository-url", &string],
        vec!["plan", "--scope", &string],
        vec!["--format", &string, "whoami"],
        vec!["-C", &string, "whoami"],
        vec!["--nothing", &string],
        vec![&string],
        // A run records its command, so the string would reach the server.
        vec!["run", &string, "--", "git", "--version"],
        vec!["run", "unit", "--", "sh", "-c", &echo],
    ] {
        let ran = Ran::of(s.hushrun(&[]).args(&args));
        assert_eq!(ran.code, 2, "{args:?}: {}", ran.stderr);
        assert_eq!(ran.stderr.trim_end(), says, "{args:?}");
        assert!(ran.stdout.is_empty(), "{args:?}: {}", ran.stdout);
        secrets.assert_not_printed(&ran, "the command line");
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());

    // A URL in a file names no value either.
    std::fs::write(
        s.repo.join("package.json"),
        format!(r#"{{"name": "shop", "hushrun": {{"backend": "file:{string}"}}}}"#),
    )
    .unwrap();
    let ran = Ran::of(s.hushrun(&[]).args(UNIT));
    secrets.assert_not_printed(&ran, "a repository's backend");
}

// covers: C-17 (the check is for a string, and only names one's are none of its business: our own
// work names these things all the time)
#[test]
fn what_only_names_a_string_goes_through() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.repo.join("src")).unwrap();
    std::fs::write(
        s.repo.join("src/hushrun_agent_notes.txt"),
        "a note on hushrun_agent_ strings\n",
    )
    .unwrap();
    for (args, says) in [
        (
            &["run", "search", "--", "grep", "-r", "hushrun_agent_", "src"][..],
            "a note",
        ),
        (
            &["run", "read", "--", "cat", "src/hushrun_agent_notes.txt"],
            "a note",
        ),
        (
            &[
                "run",
                "search-v1",
                "--",
                "grep",
                "-rc",
                "hushrun_agent_v1_",
                "src",
            ],
            "0",
        ),
        (
            &[
                "run",
                "echo",
                "--",
                "echo",
                "cargo test hushrun_agent_string",
            ],
            "hushrun_agent_string",
        ),
    ] {
        let ran = Ran::of(s.hushrun(&[]).args(args));
        assert_ne!(ran.code, 2, "{args:?}: {}", ran.stderr);
        assert!(
            ran.stdout.contains(says),
            "{args:?}: {}{}",
            ran.stdout,
            ran.stderr
        );
        assert!(
            !ran.stderr.contains("HUSHRUN_AGENT"),
            "{args:?}: {}",
            ran.stderr
        );
    }
    // A token that names it is a token, and a variable that does a value: only a string is dropped.
    let ran = Ran::of(
        s.hushrun(&[("HUSHRUN_INVOKED_BY", "agent:hushrun_agent_tests")])
            .args(["doctor"]),
    );
    assert!(!ran.stdout.contains("FAIL  agent"), "{}", ran.stdout);
    assert!(
        !ran.stderr.contains("holds a HUSHRUN_AGENT"),
        "{}",
        ran.stderr
    );
    // A remote that names it gets the hook's real answer, not a refusal of its own URL.
    let mut hook = s.hushrun(&[]);
    hook.args([
        "hook",
        "pre-push",
        "origin",
        "https://example.com/hushrun_agent_notes.git",
    ])
    .write_stdin("");
    assert_eq!(Ran::of(&mut hook).code, 0);
}

// covers: C-11
#[test]
fn a_string_in_a_repositorys_url_is_refused_not_sent() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let secrets = Secrets::of(&string, &key, FakeResults::AGENT_TOKEN);
    let url = fake.url();
    let s = Sandbox::new();
    s.name_backend(&format!("{url}/{string}"));
    for args in [UNIT, &["whoami"], &["doctor"]] {
        let ran = Ran::of(s.hushrun(&[]).args(args));
        secrets.assert_not_printed(&ran, &format!("{args:?}"));
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
}

// covers: C-13, C-17
#[test]
fn nothing_of_a_persons_speaks_for_an_agent_no_string_can_be_read_for() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let url = fake.url();
    let origin = origin_of_org(&url).to_string();
    let s = Sandbox::new();
    let login = s.a_persons_login(&origin, &url);
    let person = || {
        let mut cmd = s.hushrun(&[]);
        cmd.env("HUSHRUN_CREDENTIALS", &login);
        cmd
    };

    // Without the variable, the login records, signed by the key beside it, which is what must
    // stop.
    std::fs::write(
        s.root.join("machine-key.pem"),
        agent_key().to_pem().as_str(),
    )
    .unwrap();
    let ran = Ran::of(person().args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(fake.recorded().len(), 1, "{}", ran.stderr);
    let requests = fake.requests().len();

    let bearer = format!("Bearer {}", FakeResults::TOKEN);
    for (name, value) in [
        ("HUSHRUN_AGENT", "not a string"),
        ("HUSHRUN_AGENT", FakeResults::AGENT_TOKEN),
        ("HUSHRUN_TOKEN", string.as_str()),
        ("HUSHRUN_INVOKED_BY", string.as_str()),
    ] {
        // A lookup goes first, and carries what a record would: nothing may sign one here.
        let ran =
            Ran::of(
                person()
                    .env(name, value)
                    .args(["run", "looked-up", "--", "git", "--version"]),
            );
        assert_eq!(ran.code, 0, "{name}: {}", ran.stderr);
        assert!(ran.stdout.contains(RAN), "{}", ran.stdout);
    }
    assert_eq!(fake.recorded().len(), 1, "nothing more recorded");
    assert!(
        fake.requests()[requests..]
            .iter()
            .all(|r| r.authorization.as_deref() != Some(bearer.as_str())),
        "{:?}",
        fake.requests()
    );

    // A person's token in the variable goes no further.
    let vars = [
        ("HUSHRUN_AGENT", "not a string"),
        ("HUSHRUN_TOKEN", "hru_a-persons-token"),
        ("HUSHRUN_BACKEND", url.as_str()),
    ];
    let before = fake.requests().len();
    let ran = Ran::of(s.hushrun(&vars).args(UNIT));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        fake.requests()[before..]
            .iter()
            .all(|r| r.authorization.is_none()),
        "{:?}",
        fake.requests()
    );

    // Nor does the key a sign-in left beside the credentials.
    let signer = |vars: &[(&str, &str)]| {
        let mut cmd = s.hushrun(vars);
        cmd.env("HUSHRUN_CREDENTIALS", &login);
        Ran::of(cmd.args(["keys", "public", "--format", "json"]))
    };
    assert_eq!(signer(&[]).code, 0, "{}", signer(&[]).stderr);
    assert_eq!(signer(&[("HUSHRUN_AGENT", "not a string")]).code, 2);

    // On the pipeline paths too: the login for the ledger's origin stays out.
    let w = World::new("acme-api.yaml");
    let ledger = w.ledger_url();
    let login = w.path("credentials.json");
    let saved = serde_json::json!({
        "hosts": {origin_of(&ledger): {"token": "hru_a-persons-token", "email": "ana@acme.dev"}},
    });
    std::fs::write(&login, saved.to_string()).unwrap();
    let plan = |value: Option<&str>| {
        let mut cmd = w.hushrun();
        cmd.env_remove("HUSHRUN_REPOSITORY_TOKEN")
            .env_remove("HUSHRUN_REPOSITORY_URL");
        if let Some(value) = value {
            cmd.env("HUSHRUN_AGENT", value);
        }
        cmd.args(["plan"]);
        let before = w.fake.raw_requests().len();
        let _ = Ran::of(&mut cmd);
        w.fake.raw_requests()[before..]
            .iter()
            .filter_map(|r| r.header("authorization").map(str::to_string))
            .collect::<Vec<_>>()
    };
    assert!(
        plan(None).iter().any(|t| t.contains("hru_a-persons-token")),
        "without the variable, the login goes"
    );
    assert_eq!(plan(Some("not a string")), Vec::<String>::new());
    // Beside a readable string, for a ledger on another origin than its own, it stays out too.
    let elsewhere = AgentString::encode(
        "http://127.0.0.1:9/o/acme",
        PIPELINE_TOKEN,
        agent_key().to_pem().as_str(),
    );
    assert_eq!(plan(Some(&elsewhere)), Vec::<String>::new());
}

// covers: C-17
#[test]
fn an_agent_has_no_token_commands() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let url = fake.url();
    let s = Sandbox::new();
    let login = s.a_persons_login(origin_of_org(&url), &url);
    for (name, value) in [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_AGENT", "not a string"),
        ("HUSHRUN_TOKEN", string.as_str()),
    ] {
        for args in [
            &["token", "create", "--agent", "sneaky", "--org-wide"][..],
            &["token", "list"],
            &["token", "revoke", "sneaky"],
        ] {
            let mut cmd = s.hushrun(&[(name, value)]);
            cmd.env("HUSHRUN_CREDENTIALS", &login);
            let ran = Ran::of(cmd.args(args));
            assert_eq!(ran.code, 2, "{args:?}: {}", ran.stderr);
            assert!(
                ran.stderr.contains("agent tokens are an org admin's"),
                "{}",
                ran.stderr
            );
        }
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
}

// covers: C-13 (a login goes where a run would, and logging out too)
#[test]
fn logout_goes_where_a_run_would() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let url = fake.url();
    let origin = origin_of_org(&url).to_string();
    let s = Sandbox::new();
    // The last sign-in was elsewhere.
    let login = s.a_persons_login(&origin, "https://elsewhere.example/o/acme");
    let mut cmd = s.hushrun(&[("HUSHRUN_AGENT", &string)]);
    cmd.env("HUSHRUN_CREDENTIALS", &login);
    let ran = Ran::of(cmd.arg("logout"));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stdout.contains(&format!("Signed out of {origin}")),
        "{}\n{}",
        ran.stdout,
        ran.stderr
    );
}

// covers: C-14, C-17
#[cfg(unix)]
#[test]
fn a_string_that_isnt_utf8_is_a_string_hushrun_cant_read() {
    use std::os::unix::ffi::OsStringExt as _;
    let s = Sandbox::new();
    let mut cmd = s.hushrun(&[("CI", "true")]);
    cmd.env(
        "HUSHRUN_AGENT",
        std::ffi::OsString::from_vec(vec![b'h', 0xff]),
    );
    let ran = Ran::of(cmd.args(["doctor"]));
    assert!(ran.stdout.contains("local run (not CI)"), "{}", ran.stdout);
    assert!(ran.stdout.contains("FAIL  agent"), "{}", ran.stdout);
}

/// `hushrun run outer --force -- sh -c LINE`, where LINE's `HUSHRUN_BIN` is this hushrun, as a
/// check's own script would run it.
#[cfg(unix)]
fn outer(line: &str) -> Vec<String> {
    let binary = assert_cmd::cargo::cargo_bin("hushrun");
    let line = line.replace("HUSHRUN_BIN", binary.to_str().unwrap());
    ["run", "outer", "--force", "--", "sh", "-c", line.as_str()]
        .map(str::to_string)
        .to_vec()
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_: &Path) {}

// covers: C-17
#[cfg(unix)]
#[test]
fn the_command_gets_the_string_and_a_nested_run_uses_it() {
    let fake = FakeResults::start();
    let key = agent_key();
    let string = fake.agent_string(&key);
    let other_pem = agent_key().to_pem().to_string();
    let s = Sandbox::new();
    let vars = [("HUSHRUN_AGENT", string.as_str()), ("CI", "true")];

    // What the command sees: the variable it inherited, and nothing hushrun decoded.
    let ran = Ran::of(s.hushrun(&vars).args(outer(
        r#"test -n "$HUSHRUN_AGENT" && test -z "$HUSHRUN_TOKEN$HUSHRUN_MACHINE_KEY$HUSHRUN_BACKEND" && test "$HUSHRUN_INSIDE_RUN" = 1"#,
    )));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(fake.recorded().len(), 1);

    // A check's own `hushrun run`: both are recorded, signed, by the same agent. The nested one
    // needed the string, since no other credential is here.
    let ran = Ran::of(
        s.hushrun(&vars)
            .args(outer("HUSHRUN_BIN run inner -- git --version")),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let recorded = fake.recorded();
    let inner = recorded
        .iter()
        .find(|r| r["check"] == "inner")
        .unwrap_or_else(|| panic!("no nested result: {recorded:?}\n{}", ran.stderr));
    assert_eq!(inner["provider"], "local", "{inner}");

    // It skips, nested, the second time.
    let again = Ran::of(
        s.hushrun(&vars)
            .args(outer("HUSHRUN_BIN run inner -- git --version")),
    );
    assert!(again.stderr.contains("inner skipped"), "{}", again.stderr);

    // The precedence line isn't said twice: the nested run has nothing to add.
    let beside = [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_MACHINE_KEY", other_pem.as_str()),
    ];
    let ran = Ran::of(
        s.hushrun(&beside)
            .args(outer("HUSHRUN_BIN run nested-line -- git --version")),
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stderr.contains("nested-line"), "{}", ran.stderr);
    assert_eq!(
        ran.lines_with("HUSHRUN_AGENT wins over"),
        1,
        "{}",
        ran.stderr
    );
}

// covers: C-13
#[test]
fn signing_in_isnt_for_a_sandbox_with_a_string() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&agent_key());
    let s = Sandbox::new();
    // A readable string, and one hushrun can't read: the key must not become a person's.
    for value in [string.as_str(), "not a string"] {
        let mut cmd = s.hushrun(&[("HUSHRUN_AGENT", value)]);
        let ran = Ran::of(cmd.args(["login", &fake.url(), "--no-browser"]));
        assert_eq!(ran.code, 2, "{}", ran.stderr);
        assert!(
            ran.stderr.contains("signing in is for people"),
            "{}",
            ran.stderr
        );
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
    assert!(!Path::new(&s.root.join("home/config/hushrun/credentials.json")).exists());
}

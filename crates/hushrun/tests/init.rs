//! `hushrun init` (C-15, C-16): the setup wizard, end to end, against a fake backend on the
//! loopback (`HUSHRUN_HOST`), with fake `npm`, `gh` and checks first on `PATH`. Nothing reaches
//! the machine's own credentials, cache, package managers or GitHub.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use hushrun_testkit::git;
use hushrun_testkit::http::{Reply, Request, Response, Server};
use serde_json::{Value, json};

const TOKEN: &str = "hru_0123456789abcdefghijABCDEFGHIJ0123456789";
/// This hushrun's version, which init pins in `package.json` as `^<version>`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_REPOSITORY",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_RUN_ID",
    "HUSHRUN_RUN_URL",
    "HUSHRUN_PLAN_JSON",
    "HUSHRUN_NPM_PACKAGE",
    "npm_lifecycle_event",
];

/// A hushrun backend for the org `acme`: device sign-in, the results protocol, connecting
/// repositories, and their ledgers.
#[derive(Default)]
struct Fake {
    url: OnceLock<String>,
    /// The test approves the sign-in, unless the fake does on its second poll.
    approved: AtomicBool,
    /// What the approved sign-in answers instead of a token for acme, when set.
    token_answer: Mutex<Option<Value>>,
    auto_approve: bool,
    polls: AtomicUsize,
    /// What `POST /cli/login` was sent.
    logins: Mutex<Vec<Value>>,
    /// What `POST /o/acme/v0/repositories` was sent.
    connected: Mutex<Vec<Value>>,
    /// What `POST /o/acme/v0/ledgers` was sent.
    ledgers: Mutex<Vec<Value>>,
    /// How `POST /o/acme/v0/ledgers` answers instead of making or finding the ledger.
    ledger_refusal: Mutex<Option<(u16, Value)>>,
    results: Mutex<Vec<(String, Value)>>,
    /// Every request: method and path.
    seen: Mutex<Vec<(String, String)>>,
    /// The org `initech`, which ana isn't in, and its requests to join; without it, a backend
    /// from before requests to join, which has no `/o/initech/v0/access-requests`.
    initech: Mutex<Option<Initech>>,
    /// What `GET /cli/orgs` answers; without it, a backend from before that list.
    my_orgs: Mutex<Option<(u16, Value)>>,
    /// \[C-38\] It keeps hints: `POST /o/acme/v0/units/hints/lookup` answers; without it, 404, a
    /// backend from before them.
    hints: AtomicBool,
}

/// What the fake keeps of initech.
#[derive(Default, Clone)]
struct Initech {
    /// ana's request to join waits for an admin.
    pending: bool,
    /// An admin approved it: ana is a member.
    member: bool,
    /// How `POST …/access-requests` answers instead, when set.
    refusal: Option<(u16, Value)>,
    /// What `POST …/access-requests` was sent.
    asked: Vec<Value>,
}

impl Fake {
    fn start(auto_approve: bool) -> (Server, Arc<Fake>) {
        let fake = Arc::new(Fake {
            auto_approve,
            ..Fake::default()
        });
        let answering = fake.clone();
        let server = Server::start(move |request| answering.reply(request));
        fake.url.set(server.url()).expect("set once");
        (server, fake)
    }

    fn backend(&self) -> String {
        format!("{}/o/acme", self.url.get().expect("started"))
    }

    /// ana belongs to acme and initech, and `GET /cli/orgs` says so.
    fn in_two_orgs(&self) {
        *self.initech.lock().unwrap() = Some(Initech {
            member: true,
            ..Initech::default()
        });
        *self.my_orgs.lock().unwrap() = Some((
            200,
            json!({
                "orgs": [
                    {"slug": "acme", "name": "Acme", "role": "owner", "capabilities": []},
                    {"slug": "initech", "name": "Initech", "role": "owner", "capabilities": []},
                ],
                "dashboard": "http://dashboard.invalid",
            }),
        ));
    }

    fn reply(&self, request: Request) -> Reply {
        self.seen
            .lock()
            .unwrap()
            .push((request.method.clone(), request.path.clone()));
        let bearer = request
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "));
        let send = |status: u16, value: Value| Reply::Send(Response::json(status, &value));
        let error =
            |code: &str, message: &str| json!({"error": {"code": code, "message": message}});
        let body = || serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/cli/login") => {
                self.logins.lock().unwrap().push(body());
                send(
                    200,
                    json!({
                        "device_code": "device-1",
                        "user_code": "BCDF-GHJK",
                        "verification_url": "http://dashboard.invalid/device?code=BCDF-GHJK",
                        "expires_in": 60,
                        "interval": 1,
                    }),
                )
            }
            ("POST", "/cli/login/token") => {
                let polls = self.polls.fetch_add(1, Ordering::SeqCst);
                if self.approved.load(Ordering::SeqCst) || (self.auto_approve && polls > 0) {
                    if let Some(answer) = self.token_answer.lock().unwrap().clone() {
                        return send(200, answer);
                    }
                    send(
                        200,
                        json!({"token": TOKEN, "email": "ana@acme.dev", "org": "acme", "backend": self.backend()}),
                    )
                } else {
                    send(400, error("authorization_pending", "waiting for approval"))
                }
            }
            ("GET", "/cli/orgs") => match self.my_orgs.lock().unwrap().clone() {
                Some((status, answer)) => send(status, answer),
                None => send(404, error("not_found", "no such endpoint")),
            },
            (method, path) if path.starts_with("/o/acme/v0") => {
                if bearer != Some(TOKEN) {
                    return send(401, error("unauthenticated", "no token"));
                }
                match (method, path.strip_prefix("/o/acme/v0")) {
                    ("GET", Some("")) => send(
                        200,
                        json!({"protocol": "hushrun-results/v0", "org": "acme", "as": "user:ana@acme.dev", "dashboard": "http://dashboard.invalid/acme"}),
                    ),
                    ("POST", Some("/repositories")) => {
                        let body = body();
                        self.connected.lock().unwrap().push(body.clone());
                        send(
                            201,
                            json!({"repository": body["repository"], "connected": true}),
                        )
                    }
                    ("POST", Some("/ledgers")) => {
                        let body = body();
                        let mut asked = self.ledgers.lock().unwrap();
                        asked.push(body.clone());
                        if let Some((status, refusal)) = self.ledger_refusal.lock().unwrap().clone()
                        {
                            return send(status, refusal);
                        }
                        // Made by the first request for the repository, found by the next; a
                        // dry run makes nothing, and says whether it exists.
                        let repository = body["repository"].as_str().unwrap_or_default();
                        let dry_run = body["dry_run"] == true;
                        let exists = asked
                            .iter()
                            .filter(|b| {
                                b["repository"] == body["repository"] && b["dry_run"] != true
                            })
                            .count()
                            > usize::from(!dry_run);
                        let name = repository.rsplit('/').next().unwrap_or_default();
                        let url = self.url.get().expect("started");
                        let mut answer = json!({
                            "name": name,
                            "repository": repository,
                            "url": format!("{url}/l/acme/{name}"),
                            "github_repository_id": body["github_repository_id"],
                            "default_branch": body["default_branch"],
                            "dashboard": format!("http://dashboard.invalid/acme/{name}"),
                        });
                        if dry_run {
                            answer["dry_run"] = json!(true);
                            answer["exists"] = json!(exists);
                        }
                        send(if dry_run || exists { 200 } else { 201 }, answer)
                    }
                    ("GET", Some(key)) if key.starts_with("/results/") => {
                        let newest = self
                            .results
                            .lock()
                            .unwrap()
                            .iter()
                            .rev()
                            .find(|(k, r)| k == key && r["result"] != "skip")
                            .map(|(_, r)| r.clone());
                        send(200, json!({"result": newest}))
                    }
                    ("POST", Some(key)) if key.starts_with("/results/") => {
                        self.results.lock().unwrap().push((key.to_string(), body()));
                        send(201, json!({}))
                    }
                    ("POST", Some("/units/hints/lookup")) if self.hints.load(Ordering::SeqCst) => {
                        send(200, json!({"hints": []}))
                    }
                    _ => send(404, error("not_found", "no such endpoint")),
                }
            }
            (method, path) if path.starts_with("/o/initech/v0") => {
                let mut guard = self.initech.lock().unwrap();
                let Some(initech) = guard.as_mut() else {
                    return send(404, error("not_found", "no such endpoint"));
                };
                if bearer != Some(TOKEN) {
                    return send(401, error("unauthenticated", "no token"));
                }
                let request = |status: &str| json!({"org": "initech", "org_name": "Initech", "status": status, "repository": null, "requested_at": "2026-10-22T10:00:00Z", "decided_at": null});
                match (method, path.strip_prefix("/o/initech/v0")) {
                    ("GET", Some("")) if initech.member => send(
                        200,
                        json!({"protocol": "hushrun-results/v0", "org": "initech", "as": "user:ana@acme.dev", "dashboard": "http://dashboard.invalid/initech"}),
                    ),
                    ("GET", Some("/access-requests")) => send(
                        200,
                        json!({
                            "member": initech.member,
                            "request": if initech.member { request("approved") } else if initech.pending { request("pending") } else { Value::Null },
                        }),
                    ),
                    ("POST", Some("/access-requests")) => {
                        initech.asked.push(body());
                        if let Some((status, refusal)) = initech.refusal.clone() {
                            return send(status, refusal);
                        }
                        let created = !initech.pending;
                        initech.pending = true;
                        let mut answer = request("pending");
                        answer["created"] = json!(created);
                        send(if created { 201 } else { 200 }, answer)
                    }
                    ("POST", Some("/repositories")) if initech.member => {
                        let body = body();
                        self.connected.lock().unwrap().push(body.clone());
                        send(
                            201,
                            json!({"repository": body["repository"], "connected": true}),
                        )
                    }
                    ("POST", Some("/ledgers")) if initech.member => {
                        let body = body();
                        self.ledgers.lock().unwrap().push(body.clone());
                        let url = self.url.get().expect("started");
                        send(
                            200,
                            json!({"name": "shop", "repository": body["repository"], "url": format!("{url}/l/initech/shop"), "dry_run": body["dry_run"], "exists": true}),
                        )
                    }
                    ("GET", Some(key)) if key.starts_with("/results/") => {
                        send(200, json!({"result": null}))
                    }
                    ("POST", Some(key)) if key.starts_with("/results/") => send(201, json!({})),
                    _ => send(
                        404,
                        error(
                            "not_found",
                            "there is no org initech that ana@acme.dev belongs to",
                        ),
                    ),
                }
            }
            _ => send(404, error("not_found", "no such endpoint")),
        }
    }

    /// What `POST /o/initech/v0/access-requests` was sent.
    fn asked_to_join(&self) -> Vec<Value> {
        self.initech
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.asked.clone())
            .unwrap_or_default()
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.seen.lock().unwrap().clone()
    }
}

/// What `POST /o/acme/v0/ledgers` was sent, dry runs left out.
fn made(fake: &Fake) -> Vec<Value> {
    fake.ledgers
        .lock()
        .unwrap()
        .iter()
        .filter(|b| b["dry_run"] != true)
        .cloned()
        .collect()
}

/// The package.json init finds, in the repository the tests set up.
const PACKAGE: &str = r#"{
  "name": "shop",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "dev": "vite",
    "lint": "eslint .",
    "test": "vitest",
    "check": "tsc --noEmit || true"
  },
  "devDependencies": {
    "eslint": "^9.0.0",
    "vitest": "^2.0.0"
  }
}
"#;

const WORKFLOW: &str = "\
name: CI # pushes and pull requests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # Install, then test.
      - run: npm ci
      - run: npm test
";

/// What `gh api … --jq …` prints for the repository's runs: workflow 1 passes on tree `a`, then
/// runs twice more on it, for 12 and 5 minutes.
#[cfg(unix)]
const RUNS: &str = "\
1\ta\tcompleted\tsuccess\t2026-10-01T10:00:00Z\t2026-10-01T10:10:00Z
1\ta\tcompleted\tsuccess\t2026-10-01T11:00:00Z\t2026-10-01T11:12:00Z
1\ta\tcompleted\tfailure\t2026-10-01T12:00:00Z\t2026-10-01T12:05:00Z
1\tb\tcompleted\tsuccess\t2026-10-01T13:00:00Z\t2026-10-01T13:09:00Z
";

/// A repository with an origin on github.com, its own credentials and cache, and a directory
/// of fake tools first on PATH.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        let bin = root.join("bin");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(&bin).expect("mkdir");
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/shop.git"],
            None,
        );
        let r = Repo {
            _dir: dir,
            root,
            repo,
            bin,
        };
        r.write(".gitignore", "node_modules/\n");
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r.tools();
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.repo.join(path)).expect("read")
    }

    /// Fake npm, gh and eslint, and the npm package's `hushrun`. Each records its arguments
    /// beside itself.
    #[cfg(unix)]
    fn tools(&self) {
        use std::os::unix::fs::PermissionsExt as _;
        let tool = |name: &str, body: &str| {
            let path = self.bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        tool(
            "npm",
            r#"printf '%s\n' "$*" >> "$(dirname "$0")/npm.log"
case "$1" in
  install) echo "added 1 package"; exit 0 ;;
  run)
    script=$(sed -n "s/^ *\"$2\": \"\(.*\)\",\{0,1\}\$/\1/p" package.json | head -n 1)
    [ -n "$script" ] || { echo "npm error: missing script: $2" >&2; exit 1; }
    echo "> $2"
    export npm_lifecycle_event="$2"
    exec sh -c "$script" ;;
esac
echo "fake npm: unexpected $*" >&2
exit 1
"#,
        );
        // The repository's runs (C-16), its rulesets and who can push (ADR 0026 item 5), or
        // what GitHub says about it (its ledger), each with its own record of the
        // arguments. A ruleset created records the body it was sent.
        tool(
            "gh",
            r#"case "$*" in
  */actions/runs*) printf '%s\n' "$@" > "$(dirname "$0")/gh.args"
    cat "$(dirname "$0")/gh.out" 2>/dev/null || exit 1 ;;
  *"--method POST"*rulesets*) printf '%s\n' "$@" > "$(dirname "$0")/gh-ruleset.args"
    cat > "$(dirname "$0")/gh-ruleset.body"
    cat "$(dirname "$0")/gh-ruleset.out" 2>/dev/null || { echo '{"message":"Upgrade to GitHub Pro or make this repository public to enable this feature."}'; exit 1; } ;;
  */rulesets*) echo '[]' ;;
  */collaborators*) cat "$(dirname "$0")/gh-collaborators.out" 2>/dev/null || exit 1 ;;
  *) printf '%s\n' "$@" > "$(dirname "$0")/gh-id.args"
    cat "$(dirname "$0")/gh-id.out" 2>/dev/null || exit 1 ;;
esac
"#,
        );
        // What GitLab says about the project, through glab (ADR 0026 item 3).
        tool(
            "glab",
            r#"printf '%s\n' "$@" > "$(dirname "$0")/glab.args"
cat "$(dirname "$0")/glab.out" 2>/dev/null || exit 1
"#,
        );
        tool("eslint", "echo 'eslint: no problems'\n");
        // The npm package's bin is hushrun itself. It runs the binary under test, naming the
        // check after the script as C-9 does, so that the demo needs no more than this crate.
        tool(
            "hushrun",
            r#"if [ "$1" = run ] && [ "$2" = -- ] && [ -n "$npm_lifecycle_event" ]; then
  shift 2
  exec "$HUSHRUN_UNDER_TEST" run "$npm_lifecycle_event" -- "$@"
fi
exec "$HUSHRUN_UNDER_TEST" "$@"
"#,
        );
        std::fs::write(self.bin.join("gh.out"), RUNS).expect("write");
        // GitHub's id, then: public, default branch main, the viewer isn't an admin.
        std::fs::write(self.bin.join("gh-id.out"), "4242\tfalse\tmain\tfalse\n").expect("write");
    }

    #[cfg(not(unix))]
    fn tools(&self) {}

    fn credentials(&self) -> PathBuf {
        self.root.join("config").join("credentials.json")
    }

    #[cfg(unix)]
    fn saved(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.credentials()).expect("credentials"))
            .expect("JSON")
    }

    /// Saves a login for `server`, as `hushrun login` would.
    #[cfg(unix)]
    fn sign_in(&self, fake: &Fake) {
        let origin = fake.url.get().expect("started");
        std::fs::create_dir_all(self.credentials().parent().expect("parent")).expect("mkdir");
        std::fs::write(
            self.credentials(),
            json!({"backend": fake.backend(), "hosts": {origin: {"token": TOKEN, "email": "ana@acme.dev"}}})
                .to_string(),
        )
        .expect("write");
    }

    /// The lines a fake tool recorded.
    fn log(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.bin.join(name))
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// `hushrun` in the repository, against `fake`, with nothing of the machine's.
    fn hushrun(&self, fake: &Fake) -> Command {
        self.program(env!("CARGO_BIN_EXE_hushrun").as_ref(), fake)
    }

    /// `binary` as a `hushrun` in the repository, against `fake`.
    fn program(&self, binary: &std::path::Path, fake: &Fake) -> Command {
        let mut cmd = Command::new(binary);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        cmd.current_dir(&self.repo)
            .env("PATH", path)
            .env("HUSHRUN_HOST", fake.url.get().expect("started"))
            .env("HUSHRUN_CREDENTIALS", self.credentials())
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_UNDER_TEST", env!("CARGO_BIN_EXE_hushrun"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .stdin(Stdio::null());
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ran(mut cmd: Command, args: &[&str]) -> Ran {
    let output = cmd.args(args).output().expect("hushrun");
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The repository the tests set up: package.json, npm's lockfile and a workflow.
fn shop() -> Repo {
    Repo::new(&[
        ("package.json", PACKAGE),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (".github/workflows/ci.yml", WORKFLOW),
    ])
}

// covers: C-15, C-16
#[cfg(unix)]
#[test]
fn init_wraps_the_checks_signs_in_connects_and_shows_a_skip() {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (_server, fake) = Fake::start(false);
    let r = shop();
    let mut cmd = r.hushrun(&fake);
    let mut child = cmd
        .args(["init", "--yes", "--run", "--no-browser"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hushrun");

    // hushrun prints the code to approve, and waits: the test approves it.
    let stdout = child.stdout.take().expect("stdout");
    let (lines, read) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("utf-8");
            all.push_str(&line);
            all.push('\n');
            let _ = lines.send(line);
        }
        all
    });
    let mut stderr = child.stderr.take().expect("stderr");
    let errors = std::thread::spawn(move || {
        let mut all = String::new();
        stderr.read_to_string(&mut all).expect("utf-8");
        all
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let line = read
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("hushrun asks for the code's approval");
        if line.contains("approve the code BCDF-GHJK") {
            break;
        }
    }
    // No org hint: the repository names no backend yet.
    assert_eq!(fake.logins.lock().unwrap().len(), 1);
    assert!(fake.logins.lock().unwrap()[0].get("org").is_none());
    fake.approved.store(true, Ordering::SeqCst);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("hushrun init didn't finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = reader.join().expect("reader");
    let stderr = errors.join().expect("reader");
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");

    // The files: only the edits, each in its place.
    let backend = fake.backend();
    assert_eq!(
        r.read("package.json"),
        format!(
            r#"{{
  "name": "shop",
  "version": "1.0.0",
  "private": true,
  "scripts": {{
    "dev": "vite",
    "lint": "hushrun run -- eslint .",
    "test": "hushrun run -- vitest run",
    "check": "tsc --noEmit || true",
    "prepare": "hushrun hook install || exit 0"
  }},
  "devDependencies": {{
    "eslint": "^9.0.0",
    "hushrun": "^{VERSION}",
    "vitest": "^2.0.0"
  }},
  "hushrun": {{
    "backend": "{backend}"
  }}
}}
"#
        )
    );
    assert_eq!(
        r.read(".github/workflows/ci.yml"),
        WORKFLOW.replace(
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n",
        )
    );
    // The plan said so first.
    let dev_dependency = format!("    devDependencies: \"hushrun\": \"^{VERSION}\"");
    let installing = format!("Installing hushrun: npm install --save-dev hushrun@^{VERSION}");
    for line in [
        "  repository  github.com/acme/shop",
        "  packages    npm (package-lock.json)",
        "  checks      lint, test, check",
        "  workflows   .github/workflows/ci.yml runs test (job test)",
        "Signed in to acme as ana@acme.dev.",
        r#"    "lint": "eslint ." → "hushrun run -- eslint .""#,
        r#"    "test": "vitest" → "hushrun run -- vitest run" (vitest alone watches on a terminal)"#,
        dev_dependency.as_str(),
        "    job test: adds permissions contents: read and id-token: write. The job's token then has only these two: add any other it needs",
        "  \"check\": it uses ||",
        "Wrote package.json and .github/workflows/ci.yml.",
        "    \"prepare\": \"hushrun hook install || exit 0\", so that every clone gets the pre-push hook",
        "  .claude/skills/hushrun/SKILL.md\n    hushrun's skill for coding agents\n  .agents/skills/hushrun/SKILL.md\n    hushrun's skill for coding agents\n  AGENTS.md\n    a new file: what agents do before they push\n",
        "  .git/hooks/pre-push\n    the pre-push hook: says what CI would run again, and refuses a coding agent's push until that is done\n",
        "Wrote .claude/skills/hushrun/SKILL.md and 2 more, for coding agents.",
        "Installed the pre-push hook in .git/hooks/pre-push.",
        installing.as_str(),
        "github.com/acme/shop can record results from GitHub Actions.\nAdded github.com/acme/shop to hushrun as shop: http://dashboard.invalid/acme/shop\n",
    ] {
        assert!(stdout.contains(line), "{line:?} in\n{stdout}");
    }
    assert!(
        stdout.contains(&format!(
            "    \"hushrun\": {{\"backend\": \"{backend}\"}}, where CI, teammates and agents share results"
        )),
        "{stdout}"
    );
    // ADR 0026 item 19: the plan names the ledger before it exists, from a dry run.
    let url = fake.url.get().unwrap();
    assert!(
        stdout.contains(&format!(
            "  acme\n    connects github.com/acme/shop, so that its CI records results with its own ID token\n    adds github.com/acme/shop to acme's dashboard as shop: {url}/l/acme/shop\n"
        )),
        "{stdout}"
    );

    // C-16: what CI repeated, from gh, shown while the sign-in waited.
    assert!(
        stdout
            .contains("Last 30 days, CI ran 2 times on code it had already passed: about 17 min."),
        "{stdout}"
    );
    let gh = r.log("gh.args");
    assert_eq!(
        &gh[..7],
        [
            "api",
            "--hostname",
            "github.com",
            "--paginate",
            "-X",
            "GET",
            "repos/acme/shop/actions/runs"
        ]
    );
    assert!(gh.iter().any(|a| a.starts_with("created=>=")), "{gh:?}");

    // The install, then the demo through the package manager: lint runs, then skips.
    assert_eq!(
        r.log("npm.log"),
        [
            format!("install --save-dev hushrun@^{VERSION}"),
            "run lint".to_string(),
            "run lint".to_string()
        ]
    );
    assert!(
        stdout.contains("Running lint twice, to show a skip: npm run lint"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("eslint: no problems").count(), 1, "{stdout}");
    assert!(stderr.contains("lint passed in"), "{stderr}");
    assert!(
        stderr.contains("lint skipped: this exact code passed just now"),
        "{stderr}"
    );
    let results = fake.results.lock().unwrap();
    let verdicts: Vec<&Value> = results.iter().map(|(_, r)| &r["result"]).collect();
    assert_eq!(verdicts, ["pass", "skip"]);
    assert_eq!(results[0].1["check"], "lint");
    drop(results);

    // The repository's CI may record, its ledger takes GitHub's id for it from gh, and the next
    // step says what to do.
    assert_eq!(
        *fake.connected.lock().unwrap(),
        [json!({"repository": "github.com/acme/shop"})]
    );
    assert_eq!(
        *fake.ledgers.lock().unwrap(),
        [
            json!({"repository": "github.com/acme/shop", "github_repository_id": 4242, "default_branch": "main", "dry_run": true}),
            json!({"repository": "github.com/acme/shop", "github_repository_id": 4242, "default_branch": "main"}),
        ]
    );
    assert_eq!(
        r.log("gh-id.args"),
        [
            "api",
            "--hostname",
            "github.com",
            "repos/acme/shop",
            "--jq",
            "[.id, .private, .default_branch, .permissions.admin] | @tsv"
        ]
    );
    // ADR 0031: hushrun.yaml names the repository's URL, and no trust: what members' machines
    // and the repository's CI sign counts as it is (T-15).
    assert!(
        stdout.contains(
            "  hushrun.yaml\n    a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("Wrote hushrun.yaml, with the repository's URL."),
        "{stdout}"
    );
    let policy = r.read("hushrun.yaml");
    assert_eq!(
        policy,
        format!(
            "# hushrun.yaml: where this repository's events go.\nversion: 1\nrepository: github.com/acme/shop\nrepository_url: {url}/l/acme/shop\n"
        )
    );
    // This fake server tells no log key to the sign-in, so hushrun.yaml names none yet, and
    // init says so.
    assert!(stderr.contains("didn't tell its log key"), "{stderr}");
    assert!(
        stdout.contains(
            "Commit and push: CI records its first results. From then on a job skips what this exact code already passed on a laptop, in an agent's sandbox or in that job; each variant of a job with several skips only on its own pass.\n  http://dashboard.invalid/acme/-/start\n"
        ),
        "{stdout}"
    );
    // ADR 0031: init asks GitHub for no rulesets.
    assert!(r.log("gh-ruleset.args").is_empty());
    let saved = r.saved();
    assert_eq!(saved["backend"], backend);
    assert_eq!(saved["hosts"][fake.url.get().unwrap()]["token"], TOKEN);
}

// covers: C-15
#[test]
fn a_dry_run_changes_nothing_and_contacts_nothing() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let mut cmd = r.hushrun(&fake);
    cmd.env("HUSHRUN_NPM_PACKAGE", "/tmp/dist/hushrun-0.1.0.tgz");
    let out = ran(cmd, &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for line in [
        r#"    "lint": "eslint ." → "hushrun run -- eslint .""#,
        r#"    "hushrun": {"backend": "<your org's URL, once you sign in>"}"#,
        r#"    devDependencies: "hushrun": "file:/tmp/dist/hushrun-0.1.0.tgz""#,
        "  .github/workflows/ci.yml",
        "  your org\n    connects github.com/acme/shop, so that its CI records results with its own ID token\n    adds github.com/acme/shop to your org's dashboard, unless it is there already\n",
        "Nothing changed (--dry-run).",
    ] {
        assert!(out.stdout.contains(line), "{line:?} in\n{}", out.stdout);
    }
    assert_eq!(r.read("package.json"), PACKAGE);
    assert_eq!(r.read(".github/workflows/ci.yml"), WORKFLOW);
    assert_eq!(fake.requests(), []);
    assert!(r.log("npm.log").is_empty() && r.log("gh.args").is_empty());
    assert!(r.log("gh-id.args").is_empty());
    assert!(!r.credentials().exists());
    // C-24: the plan shows what agents get, and none of it is there yet.
    assert!(out.stdout.contains("  AGENTS.md\n"), "{}", out.stdout);
    assert!(
        out.stdout.contains("  .git/hooks/pre-push\n"),
        "{}",
        out.stdout
    );
    assert!(!r.repo.join("AGENTS.md").exists());
    assert!(!r.repo.join(".claude").exists());
    assert!(!r.repo.join(".agents").exists());
    assert!(!r.repo.join(".git/hooks/pre-push").exists());
}

/// \[ADR 0067\] What the plan of `--local` says first.
const LOCAL_PLAN: &str = "Results stay on this machine (--local): init signs in to nothing, names no backend and gives CI's jobs no ID token, so CI runs every check as before.";

// covers: C-15, C-24 (ADR 0067: init --local wraps the checks and sets agents up, with no sign-in,
// no backend, no ID token and no machine key)
#[cfg(unix)]
#[test]
fn init_local_sets_up_this_machine_and_signs_in_to_nothing() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let out = ran(r.hushrun(&fake), &["init", "--local", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains(LOCAL_PLAN), "{}", out.stdout);
    assert_eq!(fake.requests(), [], "nothing asked of any server");
    assert!(
        r.log("gh.args").is_empty() && r.log("gh-id.args").is_empty(),
        "nor gh"
    );
    assert!(!r.credentials().exists());
    assert!(
        !r.root.join("config/machine-key.pem").exists(),
        "no machine key"
    );
    let package = r.read("package.json");
    assert!(
        package.contains(r#""lint": "hushrun run -- eslint .""#),
        "{package}"
    );
    assert!(
        package.contains(&format!(r#""hushrun": "^{VERSION}""#)),
        "{package}"
    );
    assert!(
        package.contains(r#""prepare": "hushrun hook install || exit 0""#),
        "{package}"
    );
    assert!(!package.contains("backend"), "{package}");
    // CI's job gets no ID token: nothing names where it would record.
    assert_eq!(r.read(".github/workflows/ci.yml"), WORKFLOW);
    assert!(!r.repo.join("hushrun.yaml").exists());
    let agents = r.read("AGENTS.md");
    assert!(agents.contains("hushrun ready"), "{agents}");
    // Nothing here is shared, so the block doesn't tell an agent that CI skips what passed here.
    assert!(!agents.contains("skips a check in CI when"), "{agents}");
    assert!(
        agents.contains("in CI once the repository shares"),
        "{agents}"
    );
    assert!(r.repo.join(".claude/skills/hushrun/SKILL.md").exists());
    assert!(r.repo.join(".git/hooks/pre-push").exists());
    assert!(
        r.log("npm.log")
            .iter()
            .any(|line| line.starts_with("install --save-dev")),
        "{:?}",
        r.log("npm.log")
    );
    assert!(
        out.stdout.contains(
            "To share results with CI and teammates, run hushrun init again, without --local."
        ),
        "{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("Commit and push: CI records"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("your org"), "{}", out.stdout);

    // Its checks now skip here, with no server anywhere.
    let mut run = r.hushrun(&fake);
    run.args(["run", "lint", "--", "eslint", "."]);
    assert_eq!(ran(run, &[]).code, 0);
    let mut again = r.hushrun(&fake);
    again.args(["run", "lint", "--", "eslint", "."]);
    let again = ran(again, &[]);
    assert!(again.stderr.contains("lint skipped"), "{}", again.stderr);
    assert_eq!(fake.requests(), []);
}

// covers: C-15 (ADR 0067: in any language, --local wraps CI's lines, which ready reads, and
// gives their jobs no ID token, and hushrun.yaml no backend)
#[cfg(unix)]
#[test]
fn init_local_wraps_cis_lines_with_no_id_token_and_no_backend() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: go test ./...\n";
    let r = Repo::new(&[
        ("go.mod", "module shop\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--local", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(fake.requests(), []);
    let ci = r.read(".github/workflows/ci.yml");
    assert!(ci.contains("hushrun run "), "{ci}");
    assert!(ci.contains(" -- go test ./..."), "{ci}");
    assert!(!ci.contains("id-token"), "{ci}");
    assert!(
        std::fs::read_to_string(r.repo.join("hushrun.yaml"))
            .map_or(true, |text| !text.contains("backend")),
        "no backend in hushrun.yaml"
    );
    assert!(!out.stdout.contains("HUSHRUN_TOKEN"), "{}", out.stdout);
}

// covers: C-15 (ADR 0067: --dry-run shows the local plan, as text and as JSON)
#[test]
fn init_local_dry_run_shows_the_local_plan() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for line in [
        LOCAL_PLAN,
        r#"    "lint": "eslint ." → "hushrun run -- eslint .""#,
        "  AGENTS.md\n",
        // The hook reads this machine's results there, not CI's.
        "  .git/hooks/pre-push\n    the pre-push hook: says which checks haven't passed here, and refuses a coding agent's push until they have\n",
        "Nothing changed (--dry-run).",
    ] {
        assert!(out.stdout.contains(line), "{line:?} in\n{}", out.stdout);
    }
    let plan = out.stdout.split(LOCAL_PLAN).nth(1).expect("the plan");
    for absent in [
        "backend",
        "your org",
        "id-token",
        "ID token",
        "sign",
        "what CI would run again",
    ] {
        assert!(!plan.contains(absent), "{absent:?} in\n{}", out.stdout);
    }
    assert_eq!(r.read("package.json"), PACKAGE);
    assert_eq!(fake.requests(), []);

    let mut cmd = r.hushrun(&fake);
    cmd.args(["init", "--local", "--dry-run", "--format", "json"]);
    let json = ran(cmd, &[]);
    assert_eq!(json.code, 0, "{}", json.stderr);
    let plan: Value = serde_json::from_str(&json.stdout).expect("JSON");
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("init-plan.v1.schema.json"),
        &plan,
    );
    assert_eq!(plan["notes"][0], LOCAL_PLAN, "{plan:#}");
    assert!(!plan["edits"].to_string().contains("backend"), "{plan:#}");
    assert!(!plan["edits"].to_string().contains("id-token"), "{plan:#}");

    // Without a terminal it needs --yes, and names the command with --local.
    let out = ran(r.hushrun(&fake), &["init", "--local"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("run hushrun init --local --yes to apply"),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), PACKAGE);
    // --local and --org don't go together, nor in a repository that shares already.
    assert_eq!(
        ran(r.hushrun(&fake), &["init", "--local", "--org", "acme"]).code,
        2
    );
    let named = Repo::new(&[(
        "package.json",
        &format!(
            r#"{{"scripts": {{"test": "vitest"}}, "hushrun": {{"backend": "{}"}}}}"#,
            fake.backend()
        ),
    )]);
    let out = ran(named.hushrun(&fake), &["init", "--local", "--yes"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("shares its results at") && out.stderr.contains("hushrun login"),
        "{}",
        out.stderr
    );
    assert_eq!(fake.requests(), []);
}

// covers: C-15 (ADR 0067: plain init in a repository set up with --local signs in and adds what
// sharing needs, and wraps nothing twice)
#[cfg(unix)]
#[test]
fn plain_init_after_local_adds_the_backend_and_keeps_the_rest() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let local = ran(r.hushrun(&fake), &["init", "--local", "--yes", "--no-run"]);
    assert_eq!(local.code, 0, "{}", local.stderr);
    git::commit_all(&r.repo, "hushrun init --local");
    let before = r.read("package.json");

    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("Signed in to acme as ana@acme.dev."),
        "{}",
        out.stdout
    );
    let package = r.read("package.json");
    assert!(
        package.contains(&format!(r#""backend": "{}""#, fake.backend())),
        "{package}"
    );
    assert!(!package.contains("hushrun run -- hushrun run"), "{package}");
    assert_eq!(
        package.matches("hushrun hook install").count(),
        1,
        "{package}"
    );
    // Only the backend is new in package.json.
    let mut was: Value = serde_json::from_str(&before).unwrap();
    let now: Value = serde_json::from_str(&package).unwrap();
    was["hushrun"] = json!({"backend": fake.backend()});
    assert_eq!(now, was);
    // CI's job gets its ID token now, and the org the repository.
    assert!(
        r.read(".github/workflows/ci.yml")
            .contains("id-token: write"),
        "{}",
        r.read(".github/workflows/ci.yml")
    );
    assert_eq!(made(&fake).len(), 1);
    assert_eq!(r.read("AGENTS.md").matches("<!-- hushrun:start").count(), 1);
    assert_eq!(
        std::fs::read_to_string(r.repo.join(".git/hooks/pre-push"))
            .unwrap()
            .matches("# hushrun:start")
            .count(),
        1
    );
}

// covers: C-15 (ADR 0067: at a terminal with no login, init asks where results go)
#[cfg(unix)]
#[test]
fn at_a_terminal_without_a_login_init_asks_where_results_go() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    // `l` keeps them here; then the plan's own question.
    let (code, said) = terminal::answering(r.hushrun(&fake), &["init", "--no-run"], "l\ny\n");
    assert_eq!(code, 0, "{said}");
    assert!(
        said.contains(
            "Share results with CI and teammates (signs in), or keep them on this machine only? [S/l]"
        ),
        "{said}"
    );
    assert!(said.contains(LOCAL_PLAN), "{said}");
    assert!(!r.read("package.json").contains("backend"));
    assert_eq!(fake.requests(), []);

    // Signed in already, it asks nothing, and shares.
    let r = shop();
    r.sign_in(&fake);
    let (code, said) = terminal::answering(r.hushrun(&fake), &["init", "--no-run"], "y\n");
    assert_eq!(code, 0, "{said}");
    assert!(!said.contains("[S/l]"), "{said}");
    assert!(r.read("package.json").contains("backend"), "{said}");
}

#[cfg(unix)]
mod terminal {
    use std::io::{Read as _, Write as _};
    use std::process::Command;

    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    /// Runs `cmd` with `args` on a terminal of its own, stdin, stdout and stderr, typing `input`
    /// there, and returns its exit code and what it wrote.
    pub fn answering(mut cmd: Command, args: &[&str], input: &str) -> (i32, String) {
        let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        grantpt(&controller).expect("grantpt");
        unlockpt(&controller).expect("unlockpt");
        let name = ptsname(&controller, Vec::new()).expect("ptsname");
        // CLOEXEC: no other test's command may go on holding this terminal.
        let open = || {
            std::fs::File::from(
                rustix::fs::open(
                    name.as_c_str(),
                    OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .expect("the terminal"),
            )
        };
        cmd.args(args).stdin(open()).stdout(open()).stderr(open());
        let mut child = cmd.spawn().expect("hushrun");
        // Only hushrun holds the terminal now, so reading it ends when hushrun exits.
        drop(cmd);
        let mut controller = std::fs::File::from(controller);
        controller.write_all(input.as_bytes()).expect("typing");
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut buf = [0; 4096];
            // Once nothing holds the terminal, Linux answers EIO, and macOS the end.
            while let Ok(n @ 1..) = controller.read(&mut buf) {
                text.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&text).replace("\r\n", "\n")
        });
        let status = child.wait().expect("hushrun");
        (status.code().unwrap_or(-1), reader.join().expect("reader"))
    }
}

// covers: C-24
#[cfg(unix)]
#[test]
fn what_a_repository_keeps_for_itself_stays_as_it_was() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        (
            "package.json",
            &PACKAGE.replace(
                "\"dev\": \"vite\",",
                "\"dev\": \"vite\",\n    \"prepare\": \"husky\",",
            ),
        ),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        ("AGENTS.md", "# Rules\n\nBe kind.\n"),
        (".husky/pre-push", "npm run lint\n"),
    ]);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    // The team's `prepare` runs first, and can still fail an install; hushrun's can't.
    assert!(
        r.read("package.json")
            .contains(r#""prepare": "husky && (hushrun hook install || exit 0)""#),
        "{}",
        r.read("package.json")
    );
    let agents = r.read("AGENTS.md");
    assert!(
        agents.starts_with("# Rules\n\nBe kind.\n\n<!-- hushrun:start"),
        "{agents}"
    );
    assert!(
        r.read(".husky/pre-push")
            .starts_with("npm run lint\n# hushrun:start")
    );
    assert!(!r.repo.join(".git/hooks/pre-push").exists());
    assert_eq!(
        r.read(".claude/skills/hushrun/SKILL.md"),
        r.read(".agents/skills/hushrun/SKILL.md")
    );
    assert!(
        r.read(".claude/skills/hushrun/SKILL.md")
            .starts_with("---\nname: hushrun\n")
    );
}

// covers: C-24
#[cfg(unix)]
#[test]
fn yarn_from_2_on_gets_the_hook_from_postinstall() {
    let (_server, fake) = Fake::start(true);
    let plan = |version: &str| {
        let package = PACKAGE.replace(
            "\"private\": true,",
            &format!("\"private\": true,\n  \"packageManager\": \"yarn@{version}\","),
        );
        let r = Repo::new(&[("package.json", &package)]);
        let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        out.stdout
    };
    // Yarn 2 and later run no `prepare` on an install; Yarn 1 does.
    assert!(plan("4.5.0").contains("    \"postinstall\": \"hushrun hook install || exit 0\""));
    assert!(plan("1.22.22").contains("    \"prepare\": \"hushrun hook install || exit 0\""));
}

// covers: C-15
#[test]
fn without_a_terminal_init_needs_yes() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let out = ran(r.hushrun(&fake), &["init", "--no-browser"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stdout.contains("hushrun init will change:"),
        "{}",
        out.stdout
    );
    assert!(
        out.stderr
            .contains("error: init can't ask without a terminal: run hushrun init --yes to apply"),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), PACKAGE);
    assert_eq!(fake.requests(), []);
    assert!(r.log("npm.log").is_empty() && r.log("gh.args").is_empty());
    assert!(r.log("gh-id.args").is_empty());
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_saved_login_the_backend_no_longer_takes_signs_in_again() {
    let (_server, fake) = Fake::start(true);
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    // A token the backend revoked: it answers 401 to everything.
    let origin = fake.url.get().expect("started");
    std::fs::create_dir_all(r.credentials().parent().expect("parent")).expect("mkdir");
    std::fs::write(
        r.credentials(),
        json!({"backend": fake.backend(), "hosts": {origin: {"token": "hru_revoked", "email": "ana@acme.dev"}}})
            .to_string(),
    )
    .expect("write");
    let out = ran(r.hushrun(&fake), &["init", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains(&format!(
            "Your login for {origin} no longer works, so init signs in again."
        )),
        "{}",
        out.stdout
    );
    assert!(!out.stderr.contains("refused"), "{}", out.stderr);
    assert_eq!(fake.logins.lock().unwrap().len(), 1);
    assert_eq!(fake.logins.lock().unwrap()[0]["org"], "acme");
    assert!(
        out.stdout.contains("Signed in to acme as ana@acme.dev."),
        "{}",
        out.stdout
    );
    assert_eq!(made(&fake).len(), 1, "{}", out.stdout);
    assert_eq!(r.saved()["hosts"][origin]["token"], TOKEN);
}

// covers: C-15
#[test]
fn a_repository_already_set_up_signs_in_and_gets_its_ledger() {
    let (_server, fake) = Fake::start(true);
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains(&format!(
            "This repository already uses hushrun: its checks share results at {}.",
            fake.backend()
        )),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Signed in to acme as ana@acme.dev."),
        "{}",
        out.stdout
    );
    // The sign-in hints at the repository's org, and the org makes the repository's ledger;
    // nothing else changes.
    assert_eq!(fake.logins.lock().unwrap()[0]["org"], "acme");
    assert!(
        out.stdout.contains(
            "Added github.com/acme/shop to hushrun as shop: http://dashboard.invalid/acme/shop"
        ),
        "{}",
        out.stdout
    );
    let asked = fake.ledgers.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["repository"], "github.com/acme/shop");
    #[cfg(unix)]
    assert_eq!(asked[0]["github_repository_id"], 4242);
    assert!(fake.connected.lock().unwrap().is_empty());
    assert_eq!(r.read("package.json"), package);
    assert_eq!(r.read(".github/workflows/ci.yml"), WORKFLOW);
    assert!(r.log("npm.log").is_empty() && r.log("gh.args").is_empty());
    // C-24: what agents get waits for an answer, as hushrun.yaml does.
    assert!(
        out.stdout
            .contains("hushrun init can also write:\n  hushrun.yaml\n"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("  AGENTS.md\n    a new file"),
        "{}",
        out.stdout
    );
    assert!(!r.repo.join("AGENTS.md").exists());

    // --dry-run says what it would write, and asks nothing.
    let requests = fake.requests().len();
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        dry.stdout.contains(
            "init signs in, and adds github.com/acme/shop to acme unless it is there already.\nIt would also write:\n"
        ),
        "{}",
        dry.stdout
    );
    assert!(
        dry.stdout.ends_with("Nothing changed (--dry-run).\n"),
        "{}",
        dry.stdout
    );
    assert_eq!(fake.requests().len(), requests);
    assert!(!r.repo.join("AGENTS.md").exists());

    // Signed in already, it doesn't sign in again; the ledger is the one the org has.
    let before = fake.logins.lock().unwrap().len();
    let again = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains("Signed in to acme as ana@acme.dev."));
    assert!(
        again.stdout.contains(
            "github.com/acme/shop is on hushrun as shop: http://dashboard.invalid/acme/shop"
        ),
        "{}",
        again.stdout
    );
    assert_eq!(fake.logins.lock().unwrap().len(), before);
    assert_eq!(fake.ledgers.lock().unwrap().len(), 2);

    assert_eq!(
        fake.ledgers.lock().unwrap().len(),
        2,
        "no more than these two"
    );
    // C-24: a repository set up before this gets what agents need, with --yes.
    assert!(r.read("AGENTS.md").contains("hushrun ready"));
    assert!(r.repo.join(".claude/skills/hushrun/SKILL.md").exists());
    assert!(r.repo.join(".git/hooks/pre-push").exists());
    assert!(
        r.read("package.json")
            .contains("\"prepare\": \"hushrun hook install || exit 0\""),
        "{}",
        r.read("package.json")
    );

    // And then there is nothing more to write.
    let third = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(third.code, 0, "{}", third.stderr);
    assert!(
        !third.stdout.contains("AGENTS.md") && !third.stdout.contains("pre-push"),
        "{}",
        third.stdout
    );
}

// covers: C-23, C-24
#[cfg(unix)]
#[test]
fn init_keeps_the_hooks_mode_and_an_uninstall() {
    let (_server, fake) = Fake::start(true);
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\", \"prepare\": \"hushrun hook install || exit 0\"}},\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    let r = Repo::new(&[("package.json", &package)]);
    let hook = r.repo.join(".git/hooks/pre-push");
    let out = ran(r.hushrun(&fake), &["hook", "install", "--warn"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let chosen = std::fs::read_to_string(&hook).expect("hook");
    assert!(chosen.contains("hook pre-push --warn"), "{chosen}");

    // The plan has nothing to do to it, and neither has the run.
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(!dry.stdout.contains("pre-push"), "{}", dry.stdout);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(!out.stdout.contains("pre-push"), "{}", out.stdout);
    assert_eq!(std::fs::read_to_string(&hook).expect("hook"), chosen);

    // A clone that said no stays without, until someone asks for the hook.
    let out = ran(r.hushrun(&fake), &["hook", "uninstall"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!hook.exists());
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert!(!dry.stdout.contains("pre-push"), "{}", dry.stdout);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(!hook.exists(), "{}", out.stdout);
    let out = ran(r.hushrun(&fake), &["hook", "install"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(hook.exists());
}

// covers: C-15
#[cfg(unix)]
#[test]
fn without_package_json_init_signs_in_connects_and_says_how() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("  checks      no package.json"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "There is no package.json here, and no check in CI's files that init knows, so init wraps no checks. Put hushrun run NAME -- in front of a check's command wherever it runs, CI included, such as\n  hushrun run test -- go test ./...",
        ),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("github.com/acme/shop can record results from GitHub Actions.")
    );
    // With nothing going through hushrun, CI will do nothing new.
    assert!(
        !out.stdout.contains("CI records its first results"),
        "{}",
        out.stdout
    );
    // Nothing of a check's changes, but the org's do: the plan says so first.
    assert!(
        out.stdout.contains(&format!(
            "  acme\n    connects github.com/acme/shop, so that its CI records results with its own ID token\n    adds github.com/acme/shop to acme's dashboard as shop: {}/l/acme/shop\n  hushrun.yaml\n    a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log\n    backend: {}, where CI, teammates and agents share results\n\nApplying (--yes).\n",
            fake.url.get().unwrap(),
            fake.backend()
        )),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "Added github.com/acme/shop to hushrun as shop: http://dashboard.invalid/acme/shop"
        ),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("Wrote package.json"), "{}", out.stdout);
    // Every repository with a URL on hushrun gets hushrun.yaml naming it, and with no package.json
    // to say so, where its checks share results: no variable to set.
    assert!(out.stdout.contains("Wrote hushrun.yaml."), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("Added the repository's URL and log key to hushrun.yaml."),
        "{}",
        out.stdout
    );
    assert!(
        r.read("hushrun.yaml")
            .contains(&format!("backend: {}\n", fake.backend())),
        "{}",
        r.read("hushrun.yaml")
    );
    // With nothing to install the hook in every clone, it says so.
    assert!(
        out.stdout
            .contains("in each clone, an agent's sandbox included, the first hushrun ready or hushrun run installs it"),
        "{}",
        out.stdout
    );
    assert_eq!(
        *fake.connected.lock().unwrap(),
        [json!({"repository": "github.com/acme/shop"})]
    );
    assert_eq!(
        made(&fake),
        [
            json!({"repository": "github.com/acme/shop", "github_repository_id": 4242, "default_branch": "main"})
        ]
    );
    assert!(r.log("npm.log").is_empty());
    assert!(!r.repo.join("package.json").exists());
    // C-24: agents work in repositories without package.json too.
    assert_eq!(
        git::text(&r.repo, &["status", "--porcelain"]),
        "?? .agents/\n?? .claude/\n?? AGENTS.md\n?? hushrun.yaml"
    );
    assert!(r.repo.join(".git/hooks/pre-push").exists());
}

/// A repository whose `scripts` are these, with its dependencies installed.
#[cfg(unix)]
fn installed_with(scripts: &str) -> Repo {
    let package = format!("{{\n  \"name\": \"shop\",\n  \"scripts\": {{\n{scripts}\n  }}\n}}\n");
    let r = Repo::new(&[
        ("package.json", package.as_str()),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        ("checks/test.sh", "exit 0\n"),
    ]);
    r.write("node_modules/.bin/hushrun", "exit 0\n");
    r
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_script_whose_tool_isnt_installed_leaves_the_skip_to_the_next() {
    let (_server, fake) = Fake::start(true);
    let r = installed_with(
        "    \"lint\": \"a-tool-nobody-has .\",\n    \"test\": \"sh checks/test.sh\"",
    );
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("\nNot running lint: a-tool-nobody-has isn't installed here.\n\nRunning test twice, to show a skip: npm run test\n"),
        "{}",
        out.stdout
    );
    assert_eq!(
        r.log("npm.log"),
        [
            format!("install --save-dev hushrun@^{VERSION}"),
            "run test".to_string(),
            "run test".to_string()
        ]
    );
    assert!(!out.stdout.contains("failed"), "{}", out.stdout);
    assert!(!out.stderr.contains("can't start"), "{}", out.stderr);
    assert!(out.stderr.contains("test passed"), "{}", out.stderr);
    // It stays wrapped, for when the tool is there.
    assert!(
        r.read("package.json")
            .contains("\"lint\": \"hushrun run -- a-tool-nobody-has .\""),
        "{}",
        r.read("package.json")
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn with_no_tool_installed_init_shows_no_skip_and_succeeds() {
    let (_server, fake) = Fake::start(true);
    let r = installed_with("    \"lint\": \"a-tool-nobody-has .\"");
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("Not running lint: a-tool-nobody-has isn't installed here."),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("to show a skip"), "{}", out.stdout);
    assert_eq!(
        r.log("npm.log"),
        [format!("install --save-dev hushrun@^{VERSION}")]
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_failed_install_exits_1_after_connecting() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    r.sign_in(&fake);
    std::fs::write(
        r.bin.join("npm"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/npm.log\"\necho 'npm error: network' >&2\nexit 1\n",
    )
    .unwrap();
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(&format!(
            "error: hushrun isn't installed: run npm install --save-dev hushrun@^{VERSION} once the error above is fixed"
        )),
        "{}",
        out.stderr
    );
    // The files are written, CI connected and the ledger made; no demo without hushrun.
    assert_ne!(r.read("package.json"), PACKAGE);
    assert_eq!(fake.connected.lock().unwrap().len(), 1);
    assert_eq!(made(&fake).len(), 1);
    assert_eq!(
        r.log("npm.log"),
        [format!("install --save-dev hushrun@^{VERSION}")]
    );
    // Signed in already: no device flow.
    assert!(fake.logins.lock().unwrap().is_empty());
}

// covers: C-15
#[test]
fn pipeline_flags_still_run_the_pipeline_init() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let mut cmd = r.hushrun(&fake);
    cmd.args(["init", "--image", "127.0.0.1:9/acme/app"]);
    let out = ran(cmd, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(r.repo.join("hushrun.yaml").exists());
    assert!(out.stdout.contains("wrote "), "{}", out.stdout);
    assert_eq!(r.read("package.json"), PACKAGE);
    // --pipeline alone is the same init: hushrun.yaml exists now.
    let again = ran(r.hushrun(&fake), &["init", "--pipeline"]);
    assert_eq!(again.code, 2, "{}", again.stderr);
    assert!(
        again.stderr.contains("pass --force to replace it"),
        "{}",
        again.stderr
    );
    // The wizard's flags don't mix with the pipeline's.
    let mixed = ran(r.hushrun(&fake), &["init", "--yes", "--image", "x/y"]);
    assert_eq!(mixed.code, 2, "{}", mixed.stderr);
    assert!(
        mixed.stderr.contains("cannot be used with"),
        "{}",
        mixed.stderr
    );
    assert_eq!(fake.requests(), []);
}

// covers: C-15
#[test]
fn outside_a_git_repository_init_exits_2() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let mut cmd = r.hushrun(&fake);
    cmd.current_dir(elsewhere.path());
    let out = ran(cmd, &["init", "--yes"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("hushrun init sets up a git repository: run it inside one"),
        "{}",
        out.stderr
    );
    assert_eq!(fake.requests(), []);
}

/// A refusal as hushrun cloud words it.
#[cfg(unix)]
fn refusal(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

// The ledger never fails init: a member who can't make one, a backend without the request, a
// refusal.
// covers: C-15
#[cfg(unix)]
#[test]
fn a_ledger_init_cannot_get_is_a_warning() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    r.sign_in(&fake);
    let member = "acme hasn't added github.com/acme/shop to hushrun, and adding it needs the admin or owner role in acme; you are a member. An admin can add it by running npx hushrun init in the repository";
    *fake.ledger_refusal.lock().unwrap() = Some((403, refusal("forbidden", member)));
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(&format!("warning: {member}\n")),
        "{}",
        out.stderr
    );
    // The plan said so first, from the dry run.
    assert!(
        out.stdout.contains(&format!(
            "    can't add github.com/acme/shop to acme: {member}\n"
        )),
        "{}",
        out.stdout
    );
    // The checks are set up all the same.
    assert_ne!(r.read("package.json"), PACKAGE);
    assert_eq!(fake.connected.lock().unwrap().len(), 1);
    assert!(out.stdout.contains("Commit and push"), "{}", out.stdout);

    // Set up now: init only signs in and asks for the ledger, of a backend without the request,
    // then of one that refuses it.
    *fake.ledger_refusal.lock().unwrap() = Some((404, refusal("not_found", "no such endpoint")));
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains(&format!(
            "warning: {} doesn't add repositories from here: promoting github.com/acme/shop's builds needs it added in {}'s own settings",
            fake.backend(),
            fake.backend()
        )),
        "{}",
        out.stderr
    );
    *fake.ledger_refusal.lock().unwrap() = Some((
        401,
        refusal(
            "unauthenticated",
            "this token is unknown or revoked: run hushrun login again",
        ),
    ));
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains(
            "warning: github.com/acme/shop wasn't added: this token is unknown or revoked: run hushrun login again"
        ),
        "{}",
        out.stderr
    );
    assert_eq!(made(&fake).len(), 3);
}

// Without an id from gh, init asks for the ledger by the repository's name alone.
// covers: C-15
#[cfg(unix)]
#[test]
fn without_gh_init_asks_for_the_ledger_by_name() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    r.sign_in(&fake);
    std::fs::remove_file(r.bin.join("gh-id.out")).expect("remove");
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    // gh named no default branch either: the clone recorded none.
    assert_eq!(made(&fake), [json!({"repository": "github.com/acme/shop"})]);
    assert!(
        out.stdout.contains(
            "Added github.com/acme/shop to hushrun as shop: http://dashboard.invalid/acme/shop"
        ),
        "{}",
        out.stdout
    );
}

// A repository on another host: no connection, no gh, and a ledger all the same.
// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_elsewhere_gets_its_ledger_too() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    git::run(
        &r.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://gitlab.com/acme/shop.git",
        ],
        None,
    );
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("  acme\n    adds gitlab.com/acme/shop to acme's dashboard as shop: "),
        "{}",
        out.stdout
    );
    assert!(
        out.stderr
            .contains("warning: gitlab.com/acme/shop isn't on GitHub"),
        "{}",
        out.stderr
    );
    assert!(fake.connected.lock().unwrap().is_empty());
    assert_eq!(made(&fake), [json!({"repository": "gitlab.com/acme/shop"})]);
    assert!(r.log("gh-id.args").is_empty() && r.log("gh.args").is_empty());
    // ADR 0031: init asks glab for nothing: no identity to write.
    assert!(r.log("glab.args").is_empty());
}

// covers: C-15 (ADR 0031: a GitLab project's pipelines show in the plan; init writes no identity
// for them, and asks glab for nothing)
#[cfg(unix)]
#[test]
fn a_gitlab_project_gets_hushrun_yaml_and_no_identity() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("package.json", PACKAGE),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (
            ".gitlab-ci.yml",
            "test:\n  image: node:22\n  script:\n    - npm ci\n    - npm test\n",
        ),
    ]);
    git::run(
        &r.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:acme/shop.git",
        ],
        None,
    );
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("  workflows   .gitlab-ci.yml runs a wrapped check\n"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Wrote hushrun.yaml, with the repository's URL.\n"),
        "{}",
        out.stdout
    );
    assert!(!r.read("hushrun.yaml").contains("trust"));
    assert!(r.log("glab.args").is_empty());
    assert!(r.log("gh-id.args").is_empty());
    assert!(r.log("gh-ruleset.args").is_empty());
}

// covers: C-15
#[cfg(unix)]
#[test]
fn an_existing_hushrun_yaml_gets_the_url_and_is_kept_after() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("package.json", PACKAGE),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (".github/workflows/ci.yml", WORKFLOW),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\n# the team's checks\nchecks:\n  unit: {kind: source}\n",
        ),
    ]);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("    adds its URL on hushrun and the key that verifies hushrun's log"),
        "{}",
        out.stdout
    );
    // ADR 0062: every check is a source check, so its `kind` goes.
    assert!(
        out.stdout.contains("    removes `checks.unit.kind`"),
        "{}",
        out.stdout
    );
    let policy = r.read("hushrun.yaml");
    let url = fake.url.get().unwrap();
    assert!(
        policy.starts_with(&format!(
            "version: 1\nrepository: github.com/acme/shop\nrepository_url: {url}/l/acme/shop\n# the team's checks\nchecks:\n  unit: {{}}\n"
        )),
        "{policy}"
    );
    // Run again: hushrun.yaml is the team's now, and stays as it is.
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(r.read("hushrun.yaml"), policy);
}

// covers: C-13, C-15 (a first-time user: no login, signed in once, and the repository added to
// the org the approval named, in the same run)
#[cfg(unix)]
#[test]
fn a_first_time_users_init_signs_in_and_adds_the_repository_in_one_run() {
    let (_server, fake) = Fake::start(true);
    let r = shop();
    assert!(!r.credentials().exists());
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-run", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(fake.logins.lock().unwrap().len(), 1);
    // No org to hint at: the approval page chooses (or makes) it.
    assert!(fake.logins.lock().unwrap()[0].get("org").is_none());
    assert!(
        out.stdout.contains("Signed in to acme as ana@acme.dev."),
        "{}",
        out.stdout
    );
    assert_eq!(
        fake.connected.lock().unwrap().clone(),
        vec![json!({"repository": "github.com/acme/shop"})]
    );
    assert_eq!(made(&fake).len(), 1);
    assert!(r.read("hushrun.yaml").contains("repository_url:"));
    assert!(
        r.read("package.json")
            .contains(&format!("\"backend\": \"{}\"", fake.backend()))
    );
    assert_eq!(r.saved()["backend"], fake.backend());
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(!all.contains("init again"), "{all}");
}

// covers: C-15 (a sign-in approved for no org, as a server from before the approval page's
// organizations answers: where to make one)
#[cfg(unix)]
#[test]
fn a_sign_in_approved_for_no_org_says_where_to_create_one() {
    let (_server, fake) = Fake::start(true);
    *fake.token_answer.lock().unwrap() = Some(json!({"token": TOKEN, "email": "new@studio.dev"}));
    let r = shop();
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-run", "--no-browser"],
    );
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "signed in as new@studio.dev, but the sign-in wasn't approved for an organization, so init can't add this repository to one: create or join an organization at http://dashboard.invalid/new, then run npx hushrun init again"
        ),
        "{}",
        out.stderr
    );
    assert!(fake.connected.lock().unwrap().is_empty());
    assert!(fake.ledgers.lock().unwrap().is_empty());
}

// covers: C-15 (a repository of an org the person isn't in, signed in to one of their own, at a
// backend without requests to join: what happened and who can let them in, and no request the
// org would refuse)
#[cfg(unix)]
#[test]
fn a_repository_of_an_org_the_person_isnt_in_says_who_can_invite_them() {
    let (_server, fake) = Fake::start(true);
    let initech = format!("{}/o/initech", fake.url.get().unwrap());
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{initech}\"}}\n}}\n"
    );
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(fake.logins.lock().unwrap()[0]["org"], "initech");
    assert!(
        out.stderr.contains(
            "Signed in to acme as ana@acme.dev, but this repository records to initech, and ana@acme.dev isn't a member of initech."
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(
            "init sets up the files, but can't add the repository to initech: ask an admin of initech to invite ana@acme.dev, then run npx hushrun init again."
        ),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("warning:"), "{}", out.stderr);
    // Asked who the login is to initech, and whether this backend takes requests to join (it
    // doesn't), and nothing else of it.
    let initech_requests: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|(_, path)| path.starts_with("/o/initech"))
        .collect();
    assert_eq!(
        initech_requests,
        vec![
            ("GET".to_string(), "/o/initech/v0".to_string()),
            (
                "GET".to_string(),
                "/o/initech/v0/access-requests".to_string()
            ),
        ]
    );
    assert_eq!(r.read("package.json"), package);
    assert!(
        out.stdout.contains("Once an admin of initech invites you, run npx hushrun init again: it adds the repository to initech, and CI records there."),
        "{}",
        out.stdout
    );
}

/// A repository that records to initech, already set up for hushrun.
#[cfg(unix)]
fn initech_repo(fake: &Fake) -> (Repo, String) {
    let initech = format!("{}/o/initech", fake.url.get().unwrap());
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{initech}\"}}\n}}\n"
    );
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    (r, package)
}

/// initech's requests init made, beyond asking who the login is and how its request stands.
#[cfg(unix)]
fn initech_writes(fake: &Fake) -> Vec<(String, String)> {
    fake.requests()
        .into_iter()
        .filter(|(method, path)| {
            path.starts_with("/o/initech")
                && !(method == "GET" || path.ends_with("/access-requests"))
        })
        .collect()
}

// covers: C-15 (a repository of an org the person isn't in: init asks the org's admins to let them
// in, says so and who approves, writes nothing the org would refuse, and says to run init again
// once approved; the next run, still waiting, files nothing again; once approved, init adds the
// repository)
#[cfg(unix)]
#[test]
fn a_repository_of_an_org_the_person_isnt_in_asks_its_admins_to_let_them_in() {
    let (_server, fake) = Fake::start(true);
    *fake.initech.lock().unwrap() = Some(Initech::default());
    let (r, package) = initech_repo(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "Signed in to acme as ana@acme.dev, but this repository records to initech, and ana@acme.dev isn't a member of initech."
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("Asked the admins of initech to let ana@acme.dev in."),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(
            "init sets up the files, but can't add the repository to initech until one of them approves, on initech's members page: http://dashboard.invalid/initech/-/members"
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stdout.contains("Once an admin of initech approves your request, run npx hushrun init again: it adds the repository to initech, and CI records there."),
        "{}",
        out.stdout
    );
    assert!(!out.stderr.contains("warning:"), "{}", out.stderr);
    // The request names the repository whose setup asked; nothing else went to initech.
    assert_eq!(
        fake.asked_to_join(),
        vec![json!({"repository": "github.com/acme/shop"})]
    );
    assert_eq!(initech_writes(&fake), Vec::<(String, String)>::new());
    assert_eq!(r.read("package.json"), package);

    // Again, before an admin answers: signed in already, init sees the request waiting, and files
    // no other.
    let again = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(again.code, 0, "{}\n{}", again.stdout, again.stderr);
    assert_eq!(fake.logins.lock().unwrap().len(), 1);
    assert!(
        again.stderr.contains(
            "Signed in as ana@acme.dev, but this repository records to initech, and ana@acme.dev isn't a member of initech."
        ),
        "{}",
        again.stderr
    );
    assert!(
        again.stderr.contains(
            "ana@acme.dev asked to join initech already; none of its admins has answered yet."
        ),
        "{}",
        again.stderr
    );
    assert!(
        again
            .stderr
            .contains("init sets up the files, but can't add the repository to initech until one of them approves."),
        "{}",
        again.stderr
    );
    assert_eq!(fake.asked_to_join().len(), 1);
    assert!(
        again
            .stdout
            .contains("Once an admin of initech approves your request")
    );

    // Approved: init adds the repository to initech.
    fake.initech.lock().unwrap().as_mut().unwrap().member = true;
    let approved = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(approved.code, 0, "{}\n{}", approved.stdout, approved.stderr);
    assert!(
        approved
            .stdout
            .contains("Signed in to initech as ana@acme.dev."),
        "{}",
        approved.stdout
    );
    assert!(
        !approved.stderr.contains("isn't a member"),
        "{}",
        approved.stderr
    );
    // The repository's ledger in initech, which a repository set up already gets (its CI is
    // connected by whoever set it up).
    let asked = made(&fake);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["repository"], "github.com/acme/shop");
    assert!(
        approved.stderr.is_empty() || !approved.stderr.contains("Once an admin"),
        "{}",
        approved.stderr
    );
    assert_eq!(fake.asked_to_join().len(), 1);
}

// covers: C-15 (the approval page asked to join already: init says the request waits, and files
// none)
#[cfg(unix)]
#[test]
fn a_request_from_the_approval_page_is_not_filed_again() {
    let (_server, fake) = Fake::start(true);
    *fake.initech.lock().unwrap() = Some(Initech {
        pending: true,
        ..Initech::default()
    });
    let (r, package) = initech_repo(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "ana@acme.dev asked to join initech already; none of its admins has answered yet."
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("on initech's members page: http://dashboard.invalid/initech/-/members"),
        "{}",
        out.stderr
    );
    assert!(fake.asked_to_join().is_empty());
    assert_eq!(initech_writes(&fake), Vec::<(String, String)>::new());
    assert_eq!(r.read("package.json"), package);
    assert!(
        out.stdout
            .contains("Once an admin of initech approves your request")
    );
}

/// ana's repository of an owner with no org of its name, signed in, with `acme` saved as the last
/// org, in a backend that lists acme and initech.
#[cfg(unix)]
fn two_orgs(remote: &str) -> (Server, Arc<Fake>, Repo) {
    let (server, fake) = Fake::start(false);
    fake.in_two_orgs();
    let r = shop();
    r.sign_in(&fake);
    git::run(&r.repo, &["remote", "set-url", "origin", remote], None);
    (server, fake, r)
}

/// What init wrote and asked of the org it took.
#[cfg(unix)]
fn assert_set_up_in(r: &Repo, fake: &Fake, org: &str, out: &Ran) {
    let backend = format!("{}/o/{org}", fake.url.get().unwrap());
    assert!(
        r.read("package.json").contains(&backend),
        "{}\n{}",
        out.stdout,
        r.read("package.json")
    );
    assert!(
        fake.requests()
            .contains(&("POST".to_string(), format!("/o/{org}/v0/repositories"))),
        "{:?}",
        fake.requests()
    );
    let other = if org == "acme" { "initech" } else { "acme" };
    assert!(
        !fake
            .requests()
            .iter()
            .any(|(method, path)| method == "POST" && path.starts_with(&format!("/o/{other}/"))),
        "{:?}",
        fake.requests()
    );
}

// covers: C-15 (ADR 0041: with several orgs and no terminal, init never takes the last one)
#[cfg(unix)]
#[test]
fn several_orgs_and_no_way_to_ask_change_nothing() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "github.com/stark/shop is for which of your organizations? init can't ask here: add --org <slug>, with one of acme, initech"
        ),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), PACKAGE);
    assert!(
        !fake.requests().iter().any(|(method, _)| method == "POST"),
        "{:?}",
        fake.requests()
    );
    assert!(fake.logins.lock().unwrap().is_empty());
}

// covers: C-15 (ADR 0041: --org chooses among the person's orgs, whatever the last one was)
#[cfg(unix)]
#[test]
fn org_chooses_among_a_persons_orgs() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("Signed in to initech as ana@acme.dev."),
        "{}",
        out.stdout
    );
    assert_set_up_in(&r, &fake, "initech", &out);
    // The repository's choice isn't the machine's: the last org stays for repositories naming none.
    assert_eq!(r.saved()["backend"], fake.backend());
}

// covers: C-15 (ADR 0041: a repository's owner named like one of the person's orgs says which)
#[cfg(unix)]
#[test]
fn the_org_named_like_the_owner_needs_no_asking() {
    let (_server, fake, r) = two_orgs("git@github.com:Initech/shop.git");
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains(
            "Using initech, the organization named like the repository's owner: --org <slug> picks another of acme, initech."
        ),
        "{}",
        out.stdout
    );
    assert_set_up_in(&r, &fake, "initech", &out);
}

// covers: C-15 (ADR 0041: one org leaves nothing to ask, whichever was the last)
#[cfg(unix)]
#[test]
fn one_org_needs_no_asking() {
    let (_server, fake) = Fake::start(false);
    *fake.initech.lock().unwrap() = Some(Initech {
        member: true,
        ..Initech::default()
    });
    *fake.my_orgs.lock().unwrap() = Some((
        200,
        json!({"orgs": [{"slug": "initech", "name": "Initech", "role": "admin", "capabilities": []}]}),
    ));
    let r = shop();
    r.sign_in(&fake);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_set_up_in(&r, &fake, "initech", &out);
}

// covers: C-15 (ADR 0041: a login the backend no longer takes, or a person in no org, signs in)
#[cfg(unix)]
#[test]
fn a_login_with_no_org_to_use_signs_in_again() {
    for answer in [
        (
            401,
            json!({"error": {"code": "unauthenticated", "message": "this token is unknown or revoked"}}),
        ),
        (200, json!({"orgs": []})),
    ] {
        let (_server, fake) = Fake::start(true);
        *fake.my_orgs.lock().unwrap() = Some(answer);
        let r = shop();
        r.sign_in(&fake);
        let out = ran(
            r.hushrun(&fake),
            &["init", "--yes", "--no-browser", "--no-run"],
        );
        assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
        assert_eq!(fake.logins.lock().unwrap().len(), 1, "{}", out.stdout);
        assert_set_up_in(&r, &fake, "acme", &out);
    }
}

// covers: C-15 (ADR 0041: a list that fails is no reason to take the last org)
#[cfg(unix)]
#[test]
fn a_list_that_fails_stops_init_instead_of_taking_the_last_org() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    *fake.my_orgs.lock().unwrap() = Some((
        503,
        json!({"error": {"code": "unavailable", "message": "the database is down"}}),
    ));
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 6, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "the database is down: init can't tell which of your organizations this repository is for, and nothing changed. Try again, or add --org <slug>"
        ),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), PACKAGE);
    assert!(
        !fake.requests().iter().any(|(method, _)| method == "POST"),
        "{:?}",
        fake.requests()
    );
    // A plan names no org it can't be sure of, and still changes nothing.
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("<your org's URL, once you choose it>"),
        "{}",
        out.stdout
    );
}

// covers: C-15, C-18 (ADR 0041: --org with a login that stopped working signs in again, and init
// registers the machine in the org it chose, before anything writes a result)
#[cfg(unix)]
#[test]
fn org_signs_in_again_for_a_dead_login_and_registers_the_machine() {
    let (_server, fake) = Fake::start(true);
    fake.in_two_orgs();
    *fake.my_orgs.lock().unwrap() = Some((
        401,
        json!({"error": {"code": "unauthenticated", "message": "this token is unknown or revoked"}}),
    ));
    let r = shop();
    r.sign_in(&fake);
    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(fake.logins.lock().unwrap().len(), 1);
    assert_eq!(fake.logins.lock().unwrap()[0]["org"], "initech");
    assert_set_up_in(&r, &fake, "initech", &out);
    assert!(
        fake.requests()
            .contains(&("POST".to_string(), "/o/initech/v0/machines".to_string())),
        "{:?}",
        fake.requests()
    );
}

// covers: C-15, C-17, C-18 (ADR 0046: with an agent set, no login speaks for it: init is as if
// signed out, which it can't fix, so it stops, sends nothing and says so)
#[cfg(unix)]
#[test]
fn init_uses_no_login_beside_a_string() {
    let key = hushrun_attest::SigningKey::generate_p256().unwrap();
    let string = hushrun_core::agent::AgentString::encode(
        "https://agents.example/o/acme",
        "hra_0123456789abcdefghijABCDEFGHIJ0123456789",
        key.to_pem().as_str(),
    );
    for (name, value) in [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_AGENT", "not a string"),
        ("HUSHRUN_TOKEN", string.as_str()),
    ] {
        let (_server, fake) = Fake::start(true);
        fake.in_two_orgs();
        let r = shop();
        r.sign_in(&fake);
        let before = r.read("package.json");
        for org in [&["--org", "initech"][..], &[]] {
            let mut cmd = r.hushrun(&fake);
            cmd.env(name, value);
            let mut args = vec!["init", "--yes", "--no-browser", "--no-run"];
            args.extend_from_slice(org);
            let out = ran(cmd, &args);
            assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
            assert!(
                out.stderr
                    .contains("hushrun: HUSHRUN_AGENT is set, so init uses no login")
                    && out.stderr.contains("signing in is for people"),
                "{}",
                out.stderr
            );
        }
        // A plan is what a signed-out init would show, and says why.
        let mut cmd = r.hushrun(&fake);
        cmd.env(name, value);
        let plan = ran(cmd, &["init", "--dry-run"]);
        assert_eq!(plan.code, 0, "{}\n{}", plan.stdout, plan.stderr);
        assert!(
            plan.stderr
                .contains("hushrun: HUSHRUN_AGENT is set, so init uses no login"),
            "{}",
            plan.stderr
        );
        // A repository that names its org needs no list of them, and the login is no use there too.
        let origin = fake.url.get().unwrap().clone();
        let named = format!(
            "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{origin}/o/initech\"}}\n}}\n"
        );
        r.write("package.json", &named);
        git::commit_all(&r.repo, "set up");
        let mut cmd = r.hushrun(&fake);
        cmd.env(name, value);
        let out = ran(cmd, &["init", "--yes", "--no-browser", "--no-run"]);
        assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
        assert!(
            out.stderr.contains("signing in is for people"),
            "{}",
            out.stderr
        );
        assert_eq!(r.read("package.json"), named);
        r.write("package.json", &before);
        git::commit_all(&r.repo, "back");
        // Not one request: no list of orgs, repository, ledger or machine, with the login or not.
        assert!(fake.requests().is_empty(), "{:?}", fake.requests());
        assert_eq!(r.read("package.json"), before);
    }
}

// covers: C-15 (ADR 0041: a move that can't happen is refused before sign-in, and by a plan)
#[cfg(unix)]
#[test]
fn a_backend_that_isnt_an_org_can_only_move_by_hand() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let origin = fake.url.get().unwrap().clone();
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{origin}/team\"}}\n}}\n"
    );
    r.write("package.json", &package);
    git::commit_all(&r.repo, "set up");
    for flag in ["--dry-run", "--yes"] {
        let requests = fake.requests().len();
        let out = ran(r.hushrun(&fake), &["init", flag, "--org", "initech"]);
        assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
        assert!(
            out.stderr.contains("init can't move this repository"),
            "{}",
            out.stderr
        );
        assert_eq!(fake.requests().len(), requests);
        assert_eq!(r.read("package.json"), package);
    }
}

// covers: C-15 (ADR 0041: a move the new org would refuse changes nothing)
#[cfg(unix)]
#[test]
fn a_move_the_new_org_refuses_leaves_the_repository_alone() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let origin = fake.url.get().unwrap().clone();
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{origin}/o/initech\"}}\n}}\n"
    );
    let yaml = format!(
        "version: 1\nrepository: github.com/stark/shop\nrepository_url: {origin}/l/initech/shop\n"
    );
    r.write("package.json", &package);
    r.write("hushrun.yaml", &yaml);
    git::commit_all(&r.repo, "set up");
    // acme's ledgers take a person who may add it; here, one who may not.
    *fake.ledger_refusal.lock().unwrap() = Some((
        403,
        json!({"error": {"code": "forbidden", "message": "adding a repository takes an org admin"}}),
    ));
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run", "--org", "acme"],
    );
    assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "the repository stays in initech, and nothing changed: adding a repository takes an org admin"
        ),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), package);
    assert_eq!(r.read("hushrun.yaml"), yaml);
    assert!(fake.connected.lock().unwrap().is_empty());
}

// covers: C-15 (ADR 0041: a backend from before the list keeps the last org)
#[cfg(unix)]
#[test]
fn a_backend_without_the_list_keeps_the_last_org() {
    let (_server, fake) = Fake::start(false);
    let r = shop();
    r.sign_in(&fake);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_set_up_in(&r, &fake, "acme", &out);
}

// covers: C-15 (ADR 0041: --org for an org the person isn't in changes nothing)
#[cfg(unix)]
#[test]
fn org_refuses_an_org_that_isnt_theirs_and_writes_nothing() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    *fake.my_orgs.lock().unwrap() = Some((
        200,
        json!({"orgs": [
            {"slug": "acme", "name": "Acme", "role": "owner", "capabilities": []},
            {"slug": "umbrella", "name": "Umbrella", "role": "owner", "capabilities": []},
        ]}),
    ));
    *fake.initech.lock().unwrap() = None;
    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr
            .contains("ana@acme.dev isn't a member of initech: yours are acme, umbrella"),
        "{}",
        out.stderr
    );
    assert_eq!(r.read("package.json"), PACKAGE);
    assert!(
        !fake.requests().iter().any(|(method, _)| method == "POST"),
        "{:?}",
        fake.requests()
    );

    // A slug that can't be one never reaches a URL.
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--org", "../acme"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("an organization's slug"),
        "{}",
        out.stderr
    );
    // Nor does it go with the pipeline's flags.
    let out = ran(r.hushrun(&fake), &["init", "--pipeline", "--org", "acme"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}

// covers: C-15 (ADR 0041: a plan with several orgs says that init asks, and changes nothing)
#[cfg(unix)]
#[test]
fn the_plan_says_when_init_will_ask() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains(
            "You belong to 2 organizations: init asks which one this repository records to, or takes it from --org <slug>, one of acme, initech."
        ),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("<your org's URL, once you choose it>"),
        "{}",
        out.stdout
    );
    // --org says which; nothing is written or sent either way.
    let out = ran(r.hushrun(&fake), &["init", "--dry-run", "--org", "initech"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("/o/initech"), "{}", out.stdout);
    assert_eq!(r.read("package.json"), PACKAGE);
    assert!(
        !fake.requests().iter().any(|(method, _)| method == "POST"),
        "{:?}",
        fake.requests()
    );
}

// covers: C-15 (ADR 0041: --org moves a repository set up in another org, with its URL and key)
#[cfg(unix)]
#[test]
fn org_moves_a_repository_set_up_in_another() {
    let (_server, fake, r) = two_orgs("git@github.com:stark/shop.git");
    let origin = fake.url.get().unwrap().clone();
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{origin}/o/acme\"}}\n}}\n"
    );
    let yaml = format!(
        "version: 1\nrepository: github.com/stark/shop\nrepository_url: {origin}/l/acme/shop\n\ntransparency_log:\n  origin: example.test/l/acme/shop\n  key: \"ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n"
    );
    r.write("package.json", &package);
    r.write("hushrun.yaml", &yaml);
    git::commit_all(&r.repo, "set up in the wrong org");

    // A plan changes nothing.
    let out = ran(r.hushrun(&fake), &["init", "--dry-run", "--org", "initech"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains(
            "init signs in, moves it to initech, and adds github.com/stark/shop there unless it is there already."
        ),
        "{}",
        out.stdout
    );
    assert_eq!(r.read("package.json"), package);

    // Without a terminal, it needs --yes, before anything happens.
    let out = ran(r.hushrun(&fake), &["init", "--org", "initech"]);
    assert_eq!(out.code, 2, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(r.read("package.json"), package);

    // Running it again without --org keeps what the repository names: nothing asks or moves it.
    let stay = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(stay.code, 0, "{}\n{}", stay.stdout, stay.stderr);
    let stayed = r.read("package.json");
    assert!(stayed.contains(&format!("{origin}/o/acme")), "{stayed}");
    fake.connected.lock().unwrap().clear();

    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("This moves the repository from acme to initech:"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "acme keeps what it recorded, and CI's results go to initech once you push this."
        ),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Commit and push"), "{}", out.stdout);
    assert_eq!(
        r.read("package.json"),
        stayed.replace("/o/acme", "/o/initech")
    );
    let yaml = r.read("hushrun.yaml");
    assert!(
        yaml.contains(&format!("repository_url: {origin}/l/initech/shop\n")),
        "{yaml}"
    );
    assert!(!yaml.contains("/l/acme/"), "{yaml}");
    // CI records there from now on: the new org connected the repository.
    assert_eq!(
        fake.connected.lock().unwrap().clone(),
        [json!({"repository": "github.com/stark/shop"})]
    );

    // Done: --org with the org it names moves nothing.
    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(!out.stdout.contains("This moves"), "{}", out.stdout);
}

// covers: C-15 (ADR 0041: a repository with no package.json names its org in hushrun.yaml)
#[cfg(unix)]
#[test]
fn org_moves_a_repository_named_in_hushrun_yaml() {
    let (_server, fake) = Fake::start(false);
    fake.in_two_orgs();
    let origin = fake.url.get().unwrap().clone();
    let yaml = format!(
        "# Ours.\nversion: 1\nrepository: github.com/acme/tool\nrepository_url: {origin}/l/acme/tool\nbackend: {origin}/o/acme\n"
    );
    let workflow = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: hushrun run test -- cargo test\n";
    let r = Repo::new(&[
        ("hushrun.yaml", &yaml),
        (".github/workflows/ci.yml", workflow),
    ]);
    r.sign_in(&fake);
    let out = ran(
        r.hushrun(&fake),
        &[
            "init",
            "--yes",
            "--no-browser",
            "--no-run",
            "--org",
            "initech",
        ],
    );
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    let after = r.read("hushrun.yaml");
    assert!(after.starts_with("# Ours.\n"), "{after}");
    assert!(
        after.contains(&format!("backend: {origin}/o/initech\n")),
        "{after}"
    );
    assert!(
        after.contains(&format!("repository_url: {origin}/l/initech/shop\n")),
        "{after}"
    );
    assert!(
        !after.contains("/o/acme") && !after.contains("/l/acme"),
        "{after}"
    );
    assert!(
        !fake
            .requests()
            .iter()
            .any(|(method, path)| method == "POST" && path.starts_with("/o/acme/")),
        "{:?}",
        fake.requests()
    );
}

// covers: C-15 (someone invited to the org: init says to accept the invitation, not to ask)
#[cfg(unix)]
#[test]
fn someone_invited_to_the_org_is_told_to_accept_the_invitation() {
    let (_server, fake) = Fake::start(true);
    *fake.initech.lock().unwrap() = Some(Initech {
        refusal: Some((
            409,
            json!({"error": {"code": "invited", "message": "ana@acme.dev is invited to Initech already: accept the invitation instead"}}),
        )),
        ..Initech::default()
    });
    let (r, _) = initech_repo(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "ana@acme.dev is invited to initech: accept the invitation at http://dashboard.invalid/account/invitations, then run npx hushrun init again."
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stdout
            .contains("Once you accept the invitation to initech, run npx hushrun init again"),
        "{}",
        out.stdout
    );
}

// covers: C-15 (an admin declined lately: the backend's words, and who can still let them in)
#[cfg(unix)]
#[test]
fn a_declined_request_says_to_ask_for_an_invitation() {
    let (_server, fake) = Fake::start(true);
    *fake.initech.lock().unwrap() = Some(Initech {
        refusal: Some((
            409,
            json!({"error": {"code": "declined", "message": "an admin of Initech declined ana@acme.dev's request to join in the last 7 days: ask one of its admins to invite you"}}),
        )),
        ..Initech::default()
    });
    let (r, _) = initech_repo(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr
            .contains("declined ana@acme.dev's request to join in the last 7 days"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(
            "init sets up the files, but can't add the repository to initech: ask an admin of initech to invite ana@acme.dev, then run npx hushrun init again."
        ),
        "{}",
        out.stderr
    );
    assert!(out.stdout.contains("Once an admin of initech invites you"));
}

// covers: C-15 (without --yes and without a terminal to ask on, init files no request, and says
// how to)
#[cfg(unix)]
#[test]
fn without_a_terminal_init_asks_no_one_to_let_the_person_in() {
    let (_server, fake) = Fake::start(true);
    *fake.initech.lock().unwrap() = Some(Initech::default());
    let (r, _) = initech_repo(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--no-browser"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains(
            "init sets up the files, but can't add the repository to initech: run npx hushrun init --yes to ask its admins to let ana@acme.dev in, or ask one of them for an invitation."
        ),
        "{}",
        out.stderr
    );
    assert!(fake.asked_to_join().is_empty());
}

// --- ADR 0037: any language --------------------------------------------------------------------

/// A workflow that runs `command` at the root.
fn one_step(command: &str) -> String {
    format!(
        "name: CI\non: [push, pull_request]\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: {command}\n"
    )
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_in_any_language_gets_its_checks_wrapped_in_ci() {
    for (manifest, command, name) in [
        ("Cargo.toml", "cargo test --locked", "test"),
        ("go.mod", "go test ./...", "test"),
        ("pyproject.toml", "pytest -q", "test"),
        ("pyproject.toml", "uv run ruff check .", "ruff"),
        ("pom.xml", "./mvnw -B verify", "verify"),
        ("build.gradle.kts", "./gradlew test", "test"),
        ("Gemfile", "bundle exec rspec", "test"),
        ("composer.json", "vendor/bin/phpunit", "test"),
        ("App.csproj", "dotnet test --no-restore", "test"),
        ("mix.exs", "mix test", "test"),
        ("Package.swift", "swift test", "test"),
        ("Makefile", "make check", "check"),
        ("justfile", "just lint", "lint"),
        ("Taskfile.yml", "task test", "test"),
    ] {
        let (_server, fake) = Fake::start(true);
        let workflow = one_step(command);
        let r = Repo::new(&[(manifest, ""), (".github/workflows/ci.yml", &workflow)]);
        let out = ran(
            r.hushrun(&fake),
            &["init", "--yes", "--no-browser", "--no-run"],
        );
        assert_eq!(out.code, 0, "{command}: {}", out.stderr);
        assert!(
            out.stdout.contains(&format!(
                "job check: {command} → hushrun run {name} -- {command}"
            )),
            "{command}: {}",
            out.stdout
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            r.read(".github/workflows/ci.yml"),
            workflow
                .replace(
                    "    runs-on: ubuntu-latest\n",
                    "    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n",
                )
                .replace(
                    &format!("      - run: {command}\n"),
                    &format!(
                        "      - uses: hushrun/hushrun/action/setup@v{version}\n      - run: hushrun run {name} -- {command}\n"
                    ),
                ),
            "{command}"
        );
        // No variable to set anywhere, and no package.json, npm or lockfile involved.
        let policy = r.read("hushrun.yaml");
        assert!(
            policy.contains(&format!("backend: {}\n", fake.backend())),
            "{policy}"
        );
        assert!(!policy.contains("scopes:"), "{policy}");
        assert!(r.log("npm.log").is_empty());
        assert!(!r.repo.join("package.json").exists());

        // Run again, it is set up: nothing to wrap twice.
        git::commit_all(&r.repo, "hushrun");
        let again = ran(
            r.hushrun(&fake),
            &["init", "--yes", "--no-browser", "--no-run"],
        );
        assert_eq!(again.code, 0, "{}", again.stderr);
        assert!(
            again
                .stdout
                .contains("This repository already uses hushrun"),
            "{command}: {}",
            again.stdout
        );
        assert_eq!(
            r.read(".github/workflows/ci.yml")
                .matches("hushrun run")
                .count(),
            1
        );
    }
}

// --- ADR 0050: what CI runs, wherever it is written ---------------------------------------------

/// The plan of a dry run in a repository of `files`, with `command` the step of a CI job.
#[cfg(unix)]
fn planned_in(files: &[(&str, &str)], command: &str) -> Ran {
    let (_server, fake) = Fake::start(true);
    let workflow = one_step(command);
    let mut files = files.to_vec();
    files.push((".github/workflows/ci.yml", &workflow));
    let r = Repo::new(&files);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{command}: {}", out.stderr);
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
    assert!(fake.requests().is_empty(), "{command}");
    out
}

// covers: C-15
#[cfg(unix)]
#[test]
fn each_new_runner_wraps_its_example_in_ci() {
    for (manifest, command, name) in [
        (
            "pyproject.toml",
            "python -m unittest discover -s tests",
            "test",
        ),
        ("build.gradle.kts", "./gradlew build", "build"),
        ("build.gradle.kts", "./gradlew clean test", "test"),
        ("pom.xml", "mvn -B clean install", "install"),
        ("pom.xml", "./mvnw clean verify", "verify"),
        ("build.sbt", "sbt test", "test"),
        ("Gemfile", "bundle exec rake", "test"),
        ("CMakeLists.txt", "ctest --output-on-failure", "test"),
        ("meson.build", "meson test -C build", "test"),
        ("pubspec.yaml", "dart test", "test"),
        ("pubspec.yaml", "flutter test", "test"),
        ("deno.json", "deno test --allow-read", "test"),
    ] {
        let out = planned_in(&[(manifest, "")], command);
        assert!(
            out.stdout.contains(&format!(
                "job check: {command} → hushrun run {name} -- {command}\n"
            )),
            "{command}: {}",
            out.stdout
        );
    }
    // What only looks like one: the tests turned off, another task after the build, a list of
    // the tests, and the shell's own `test`.
    for command in [
        "mvn -B clean install -DskipTests",
        "./gradlew build publish",
        "ctest -N",
        "bundle exec rake release",
        "python -m test",
        "test -f build/app",
    ] {
        let out = planned_in(&[], command);
        assert!(
            !out.stdout.contains(&format!("{command} →")),
            "{command}: {}",
            out.stdout
        );
    }
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_make_target_whose_recipe_runs_a_check_is_wrapped_and_one_that_pushes_stays() {
    let makefile = "\
PYTEST ?= pytest

test-ci: | lint-ci
\t$(PYTEST) -q $(PYTEST_ARGS)

lint-ci:
\truff check .

test-deploy: test-ci
\tdocker push ghcr.io/acme/shop:latest

test-env: lint-ci
\t$(DEPLOY_TOOL) --all

docs:
\tmkdocs build
";
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: make test-ci\n      - run: make test-deploy\n      - run: make test-env\n      - run: make docs\n";
    let r = Repo::new(&[
        ("Makefile", makefile),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for line in [
        "    job test: make test-ci → hushrun run test-ci -- make test-ci\n",
        "It leaves these commands in CI's files as they are:\n  .github/workflows/ci.yml, job test: make test-deploy: its recipe pushes an image (docker push)\n  .github/workflows/ci.yml, job test: make test-env: its recipe can't be read\n",
    ] {
        assert!(out.stdout.contains(line), "{line}: {}", out.stdout);
    }
    // A target whose recipe runs no check says nothing, as any command the table doesn't know.
    assert!(!out.stdout.contains("make docs"), "{}", out.stdout);
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
    assert!(fake.requests().is_empty());

    // The plan says the same, a line of the job for each.
    let out = ran(r.hushrun(&fake), &["init", "--dry-run", "--format", "json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let plan: Value = serde_json::from_str(&out.stdout).expect("JSON");
    let commands = &plan["jobs"][0]["commands"];
    let of = |before: &str| -> Value {
        commands
            .as_array()
            .expect("commands")
            .iter()
            .find(|command| command["before"] == before)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(of("make test-ci")["status"], "wrapped");
    assert_eq!(of("make test-ci")["check"], "test-ci");
    assert_eq!(of("make test-deploy")["status"], "left");
    assert_eq!(
        of("make test-deploy")["why"],
        "its recipe pushes an image (docker push)"
    );
    assert_eq!(of("make test-env")["why"], "its recipe can't be read");
    assert_eq!(of("make docs"), Value::Null);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn just_and_task_targets_and_make_elsewhere_are_read_by_their_recipe_too() {
    for (files, command, name) in [
        (
            &[(
                "justfile",
                "test-unit: lint\n    cargo nextest run --workspace\n\nlint:\n    cargo clippy -- -D warnings\n",
            )][..],
            "just test-unit",
            "test-unit",
        ),
        (
            &[(
                "Taskfile.yml",
                "version: '3'\ntasks:\n  test-unit:\n    deps: [lint]\n    cmds:\n      - go test ./...\n  lint: golangci-lint run\n",
            )],
            "task test-unit",
            "test-unit",
        ),
        (
            &[(
                "web/Makefile",
                "ui-test:\n\tcd ui && CI=true pnpm run test\n",
            )],
            "make -C web ui-test",
            "ui-test",
        ),
        // A name the table knows, wherever make starts, needs no recipe.
        (&[], "make -C build -j 4 check", "check"),
    ] {
        let out = planned_in(files, command);
        assert!(
            out.stdout.contains(&format!(
                "job check: {command} → hushrun run {name} -- {command}\n"
            )),
            "{command}: {}",
            out.stdout
        );
    }
}

// --- ADR 0040: bun test ---------------------------------------------------------------------

/// A Bun repository with `package` and `workflows`, as CI's files, and a fake `bun`.
#[cfg(unix)]
fn bun_repo(package: &str, workflows: &[(&str, &str)]) -> Repo {
    use std::os::unix::fs::PermissionsExt as _;
    let mut files = vec![("package.json", package), ("bun.lock", "")];
    files.extend_from_slice(workflows);
    let r = Repo::new(&files);
    let bun = r.bin.join("bun");
    std::fs::write(&bun, "#!/bin/sh\nexit 0\n").expect("write");
    std::fs::set_permissions(&bun, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    r
}

/// `bun test` in a job `test`, as a repository's staging and production workflows have it.
const BUN_WORKFLOW: &str = "name: CI\non: push\npermissions:\n  contents: read\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: bun install --frozen-lockfile\n      - name: Run tests\n        run: bun test\n";

// covers: C-15
#[cfg(unix)]
#[test]
fn bun_test_in_ci_goes_through_hushrun_under_the_scripts_name() {
    let (_server, fake) = Fake::start(true);
    let r = bun_repo(
        "{\n  \"name\": \"shop\",\n  \"scripts\": {\"test\": \"bun test\"}\n}\n",
        &[
            (".github/workflows/staging.yml", BUN_WORKFLOW),
            (".github/workflows/production.yml", BUN_WORKFLOW),
        ],
    );
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        !out.stdout.contains("none run these checks"),
        "{}",
        out.stdout
    );
    assert!(
        r.read("package.json")
            .contains("\"test\": \"hushrun run -- bun test\""),
        "{}",
        r.read("package.json")
    );
    // Two jobs, one check: `bun run test` here and `hushrun run test -- bun test` in CI have one
    // key, so the pass of either skips the other.
    for file in ["staging", "production"] {
        let workflow = r.read(&format!(".github/workflows/{file}.yml"));
        assert!(
            workflow.contains("        run: hushrun run test -- bun test\n"),
            "{workflow}"
        );
        assert!(workflow.contains("id-token: write"), "{workflow}");
        assert!(
            workflow.contains(&format!(
                "uses: hushrun/hushrun/action/setup@v{}",
                env!("CARGO_PKG_VERSION")
            )),
            "{workflow}"
        );
        assert_eq!(workflow.matches("bun install").count(), 1);
    }
    assert!(
        out.stdout.contains(
            "Run your tests with bun run test: bun test is Bun's own runner, which never reads package.json, so it can't skip them."
        ),
        "{}",
        out.stdout
    );

    // Run again, nothing is wrapped twice.
    git::commit_all(&r.repo, "hushrun");
    let again = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    let workflow = r.read(".github/workflows/production.yml");
    assert_eq!(workflow.matches("hushrun run").count(), 1, "{workflow}");
}

// covers: C-15
#[cfg(unix)]
#[test]
fn bun_test_in_ci_goes_through_hushrun_with_no_script_to_share() {
    let (_server, fake) = Fake::start(true);
    let r = bun_repo(
        "{\n  \"name\": \"shop\"\n}\n",
        &[(".github/workflows/ci.yml", BUN_WORKFLOW)],
    );
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let workflow = r.read(".github/workflows/ci.yml");
    assert!(
        workflow.contains("        run: hushrun run test -- bun test\n"),
        "{workflow}"
    );
    assert!(
        !out.stdout.contains("Run your tests with"),
        "{}",
        out.stdout
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_already_set_up_gets_what_its_scripts_run_in_ci() {
    let (_server, fake) = Fake::start(true);
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- bun test\"}},\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    // Its people left this one out of hushrun: it stays their choice.
    let workflow = format!("{BUN_WORKFLOW}      - run: cargo test --locked\n");
    let r = bun_repo(&package, &[(".github/workflows/ci.yml", &workflow)]);

    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        dry.stdout
            .contains("job test: bun test → hushrun run test -- bun test"),
        "{}",
        dry.stdout
    );
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);

    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("This repository already uses hushrun"),
        "{}",
        out.stdout
    );
    let written = r.read(".github/workflows/ci.yml");
    assert!(
        written.contains("        run: hushrun run test -- bun test\n"),
        "{written}"
    );
    assert!(written.contains("id-token: write"), "{written}");
    assert!(
        written.contains("      - run: cargo test --locked\n"),
        "{written}"
    );
    assert!(
        out.stdout.contains("Run your tests with bun run test"),
        "{}",
        out.stdout
    );
}

/// \[ADR 0049\] A repository an older hushrun set up gets what tells its variants apart, once.
// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn a_repository_set_up_before_gets_its_variants_told_apart_once() {
    let (_server, fake) = Fake::start(true);
    let package = format!(
        "{{\n  \"scripts\": {{\"test\": \"hushrun run -- vitest run\"}},\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    let workflow = "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: [20, 22]\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("This repository already uses hushrun"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Each variant of test gets its own pass (HUSHRUN_VARIANT in its env)."),
        "{}",
        out.stdout
    );
    let written = r.read(".github/workflows/ci.yml");
    assert_eq!(
        written,
        workflow.replace(
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-latest\n    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n",
        )
    );
    // Again: nothing more.
    let again = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(again.code, 0, "{}{}", again.stdout, again.stderr);
    assert!(
        !again.stdout.contains("Each variant of test"),
        "{}",
        again.stdout
    );
    assert_eq!(r.read(".github/workflows/ci.yml"), written);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_dry_run_shows_the_commands_scopes_and_what_it_leaves() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push\njobs:\n  back:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: back\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n      - run: cargo deny check\n";
    let r = Repo::new(&[
        ("back/Cargo.toml", "[package]\nname = \"back\"\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for line in [
        "  checks      test\n",
        "  workflows   .github/workflows/ci.yml runs test (job back)\n",
        "    job back: cargo test → hushrun run --scope back test -- cargo test\n",
        "    job back: installs hushrun first\n",
        "    scope back: back/**, *, .github/workflows/**, leaving out *.md, LICENSE*, NOTICE*\n",
        "    hushrun can't see a path built at run time, such as path.join(__dirname, \"..\", \"landing\"): add what such a check reads to its scope's include\n",
        "It leaves these commands in CI's files as they are:\n  .github/workflows/ci.yml, job back: cargo deny check: its answer changes with an advisory database, not only with the code\n",
    ] {
        assert!(out.stdout.contains(line), "{line}: {}", out.stdout);
    }
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
    assert!(!r.repo.join("hushrun.yaml").exists());
    assert!(fake.requests().is_empty());
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_workspace_stays_in_one_scope_and_the_plan_says_why() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n        working-directory: a\n";
    let r = Repo::new(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
        ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert!(
        out.stdout
            .contains("One scope for the whole repository: Cargo.toml ties its projects together."),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("job a: cargo test → hushrun run test -- cargo test\n"),
        "{}",
        out.stdout
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_gitlab_project_gets_its_checks_wrapped_and_what_installs_hushrun() {
    let (_server, fake) = Fake::start(true);
    let pipeline = "test:\n  script:\n    - cargo fetch\n    - cargo test --locked\n";
    let r = Repo::new(&[("Cargo.toml", ""), (".gitlab-ci.yml", pipeline)]);
    git::run(
        &r.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:acme/shop.git",
        ],
        None,
    );
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        r.read(".gitlab-ci.yml"),
        "test:\n  script:\n    - export PATH=\"$(dirname \"$(sh ci/install-hushrun.sh)\"):$PATH\"\n    - cargo fetch\n    - hushrun run test -- cargo test --locked\n"
    );
    assert!(r.read("ci/install-hushrun.sh").starts_with("#!"));
    assert!(
        out.stdout
            .contains("hushrun token create --agent gitlab-ci"),
        "{}",
        out.stdout
    );
}

/// \[ADR 0049\] GitLab names each instance of a job apart, so a pass counts for its own: init
/// wraps their lines, and they skip there.
// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn a_gitlab_jobs_instances_are_wrapped_and_skip_each_on_its_own_pass() {
    let (_server, fake) = Fake::start(true);
    let pipeline = "test:\n  parallel: 3\n  script:\n    - cargo test\nlint:\n  parallel:\n    matrix:\n      - OS: [linux, mac]\n  script:\n    - cargo clippy\n";
    let r = Repo::new(&[("Cargo.toml", ""), (".gitlab-ci.yml", pipeline)]);
    git::run(
        &r.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:acme/shop.git",
        ],
        None,
    );
    let plan = json_plan(r.hushrun(&fake));
    for (job, check) in [("test", "test"), ("lint", "clippy")] {
        let found = plan_job(&plan, job);
        assert_eq!(found["variants"], "several", "{found:#}");
        assert_eq!(found["variantIdentity"], "named", "{found:#}");
        let command = &found["commands"][0];
        assert_eq!(command["status"], "wrapped", "{found:#}");
        assert_eq!(command["check"], check, "{found:#}");
        assert_eq!(command["ci"], "skips", "{found:#}");
    }
}

// covers: C-9, C-11, C-15, C-22, C-23
#[cfg(unix)]
#[test]
fn from_init_to_a_skip_in_ci_in_a_repository_of_projects() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_server, fake) = Fake::start(true);
    const WORKFLOW: &str = "name: CI
on: [push, pull_request]
permissions:
  contents: read
jobs:
  back:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: shop/back
    steps:
      - uses: actions/checkout@v5
        with:
          path: shop
      - run: cargo test --locked
  ui:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: shop/ui
    steps:
      - uses: actions/checkout@v5
        with:
          path: shop
      - run: pnpm test
";
    let r = Repo::new(&[
        (
            "back/Cargo.toml",
            "[package]\nname = \"back\"\n\n[dependencies]\ncore = { path = \"../../cli/crates/core\" }\n",
        ),
        ("back/main.rs", "one\n"),
        ("ui/package.json", "{\"name\": \"ui\"}\n"),
        ("ui/app.tsx", "one\n"),
        ("docs/guide.md", "one\n"),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    for tool in ["cargo", "pnpm"] {
        let path = r.bin.join(tool);
        std::fs::write(&path, "#!/bin/sh\necho ran\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    // The checkout back builds against, beside the repository.
    let cli = r.root.join("cli");
    std::fs::create_dir_all(cli.join("crates/core")).expect("mkdir");
    git::init(&cli, "sha1");
    std::fs::write(cli.join("crates/core/lib.rs"), "one\n").expect("write");
    git::commit_all(&cli, "init");

    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    // Each project gets its scope, and back the checkout it builds against.
    let policy = r.read("hushrun.yaml");
    assert!(
        policy.contains("  back:\n    include:\n      - \"back/**\"\n"),
        "{policy}"
    );
    assert!(
        policy.contains("    external:\n      - path: ../cli\n"),
        "{policy}"
    );
    assert!(
        policy.contains("  ui:\n    include:\n      - \"ui/**\"\n"),
        "{policy}"
    );
    let ci = r.read(".github/workflows/ci.yml");
    assert!(
        ci.contains("      - run: hushrun run --scope back test -- cargo test --locked\n"),
        "{ci}"
    );
    assert!(ci.contains("  id-token: write\n"), "{ci}");
    git::commit_all(&r.repo, "hushrun");

    // `sh -c line`, with everything hushrun needs to find, as hushrun's own command would.
    let sh = |line: &str, vars: &[(&str, &str)]| {
        let mut cmd = Command::new("sh");
        let like = r.hushrun(&fake);
        for (name, value) in like.get_envs() {
            match value {
                Some(value) => cmd.env(name, value),
                None => cmd.env_remove(name),
            };
        }
        cmd.current_dir(&r.repo).args(["-c", line]);
        for (name, value) in vars {
            cmd.env(name, value);
        }
        cmd.output().expect("sh")
    };
    let ready = || {
        let out = ran(r.hushrun(&fake), &["ready", "--format", "json"]);
        let value: Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}{}", out.stdout, out.stderr));
        (out.code, value)
    };
    let states = |report: &Value| -> Vec<String> {
        report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .map(|item| {
                format!(
                    "{} {}",
                    item["check"].as_str().unwrap(),
                    item["state"].as_str().unwrap()
                )
            })
            .collect()
    };

    let (code, report) = ready();
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(states(&report), ["back/test missing", "ui/test missing"]);

    // The hook init installed refuses an agent's push, with the commands to run.
    let bare = r.root.join("origin.git");
    std::fs::create_dir_all(&bare).expect("mkdir");
    git::init(&bare, "sha1");
    git::run(&bare, &["config", "core.bare", "true"], None);
    let push = format!("git push {} HEAD:refs/heads/fix", bare.display());
    let refused = sh(&push, &[("CLAUDECODE", "1")]);
    assert!(!refused.status.success(), "{refused:?}");
    let text = String::from_utf8_lossy(&refused.stderr);
    assert!(text.contains("push refused"), "{text}");
    assert!(
        text.contains("cd back && hushrun run --scope back test -- cargo test --locked"),
        "{text}"
    );

    // Doing what it says ends.
    for line in report["next"].as_array().expect("next") {
        let out = sh(line.as_str().expect("a command"), &[]);
        assert!(out.status.success(), "{out:?}");
    }
    let (code, report) = ready();
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), ["back/test passed", "ui/test passed"]);
    let pushed = sh(&push, &[("CLAUDECODE", "1")]);
    assert!(pushed.status.success(), "{pushed:?}");

    // CI, in its own directory, with the job's words, skips what passed here. It takes no
    // person's login (C-13): its backend and token are the job's.
    let backend = fake.backend();
    let out = sh(
        "cd back && hushrun run --scope back test -- cargo test --locked",
        &[
            ("HUSHRUN_BACKEND", &backend),
            ("HUSHRUN_TOKEN", TOKEN),
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "acme/shop"),
            (
                "GITHUB_WORKFLOW_REF",
                "acme/shop/.github/workflows/ci.yml@refs/heads/main",
            ),
            ("GITHUB_JOB", "back"),
        ],
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && text.contains("test skipped"),
        "{text}"
    );

    // A file no scope has doesn't run anything again.
    r.write("docs/guide.md", "two\n");
    git::commit_all(&r.repo, "docs");
    assert_eq!(ready().0, 0);

    // One project's change runs its checks and no one else's.
    r.write("ui/app.tsx", "two\n");
    git::commit_all(&r.repo, "ui");
    assert_eq!(states(&ready().1), ["back/test passed", "ui/test missing"]);
    r.write("ui/app.tsx", "one\n");
    git::commit_all(&r.repo, "back to it");

    // A change in the checkout beside it, with no file of this repository changed, runs back's.
    std::fs::write(cli.join("crates/core/lib.rs"), "two\n").expect("write");
    let (code, report) = ready();
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(states(&report), ["back/test missing", "ui/test passed"]);
    let out = sh(
        "cd back && hushrun run --scope back test -- cargo test --locked",
        &[],
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("no pass yet for this exact code"), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ran"),
        "{out:?}"
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_with_a_package_json_keeps_its_javascript_where_it_is() {
    // What a root package.json runs is its scripts' to wrap, and a command in CI stays as it was:
    // but another language's check in the same workflow is wrapped.
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npx eslint .\n      - run: npx vitest run\n      - run: npm test\n      - run: cargo test\n";
    let r = Repo::new(&[
        (
            "package.json",
            "{\n  \"scripts\": {\n    \"test\": \"eslint .\"\n  }\n}\n",
        ),
        ("Cargo.toml", ""),
        (".github/workflows/ci.yml", workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("job test: cargo test → hushrun run test -- cargo test\n"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("npx eslint . →"), "{}", out.stdout);
    assert!(!out.stdout.contains("npx vitest run →"), "{}", out.stdout);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_hushrun_yaml_that_doesnt_parse_stops_nothing_and_changes_nothing() {
    let (_server, fake) = Fake::start(true);
    let workflow = one_step("cargo test");
    let broken = "version: 2\n";
    let r = Repo::new(&[
        ("Cargo.toml", ""),
        ("hushrun.yaml", broken),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("hushrun.yaml doesn't parse, so init leaves it"),
        "{}",
        out.stdout
    );
    assert_eq!(r.read("hushrun.yaml"), broken);
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
}

// covers: C-9, C-15, C-22 (review cases C and D, through a real init)
#[cfg(unix)]
#[test]
fn a_change_to_what_a_project_reads_outside_its_directory_runs_its_checks_again() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_server, fake) = Fake::start(true);
    const WORKFLOW: &str = "on: push
jobs:
  back:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: back
    steps:
      - uses: actions/checkout@v5
      - run: cargo test
  ui:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: ui
    steps:
      - uses: actions/checkout@v5
      - run: pnpm test
";
    let r = Repo::new(&[
        ("back/Cargo.toml", "[package]\nname = \"back\"\n"),
        ("back/NOTES.md", "one\n"),
        (
            "back/tests/limits.rs",
            "let path = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../site/data/prices.json\");\n",
        ),
        ("site/data/prices.json", "{}\n"),
        ("site/data/site.ts", "one\n"),
        ("ui/package.json", "{\"name\": \"ui\"}\n"),
        (
            "ui/tsconfig.json",
            "{\"extends\": \"../tsconfig.base.json\"}\n",
        ),
        ("tsconfig.base.json", "{}\n"),
        ("CHANGELOG.md", "one\n"),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    for tool in ["cargo", "pnpm"] {
        let path = r.bin.join(tool);
        std::fs::write(&path, "#!/bin/sh\necho ran\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let policy = r.read("hushrun.yaml");
    assert!(
        policy.contains("      - \"site/data/prices.json\"\n"),
        "{policy}"
    );
    assert!(!policy.contains("\"site/**\""), "{policy}");
    assert!(
        policy.contains("    exclude:\n      - \"*.md\"\n"),
        "{policy}"
    );
    git::commit_all(&r.repo, "hushrun");

    let sh = |line: &str| {
        let mut cmd = Command::new("sh");
        for (name, value) in r.hushrun(&fake).get_envs() {
            match value {
                Some(value) => cmd.env(name, value),
                None => cmd.env_remove(name),
            };
        }
        let out = cmd
            .current_dir(&r.repo)
            .args(["-c", line])
            .output()
            .expect("sh");
        assert!(out.status.success(), "{out:?}");
    };
    let states = || {
        let out = ran(r.hushrun(&fake), &["ready", "--format", "json"]);
        let report: Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}{}", out.stdout, out.stderr));
        for line in report["next"].as_array().expect("next") {
            sh(line.as_str().expect("a command"));
        }
        report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .map(|item| match item["state"].as_str().expect("state") {
                // Nothing here expects `ci`: when it comes, the failure says why.
                "ci" => format!("{item}"),
                state => format!("{} {state}", item["check"].as_str().expect("check")),
            })
            .collect::<Vec<_>>()
    };
    let commit = |path: &str, text: &str| {
        r.write(path, text);
        git::commit_all(&r.repo, path);
    };

    // Everything runs once, and then has passed.
    assert_eq!(states(), ["back/test missing", "ui/test missing"]);
    assert_eq!(states(), ["back/test passed", "ui/test passed"]);

    // D: the file its test opens, in a directory that is no scope of its own.
    commit("site/data/prices.json", "{\"changed\": true}\n");
    assert_eq!(states(), ["back/test missing", "ui/test passed"]);
    assert_eq!(states(), ["back/test passed", "ui/test passed"]);
    // The rest of that directory is none of its business.
    commit("site/data/site.ts", "two\n");
    assert_eq!(states(), ["back/test passed", "ui/test passed"]);

    // C: a config at the root, which every project may read.
    commit("tsconfig.base.json", "{\"changed\": true}\n");
    assert_eq!(states(), ["back/test missing", "ui/test missing"]);
    assert_eq!(states(), ["back/test passed", "ui/test passed"]);

    // The root's prose runs nothing, and a project's own docs are its own.
    commit("CHANGELOG.md", "two\n");
    assert_eq!(states(), ["back/test passed", "ui/test passed"]);
    commit("back/NOTES.md", "two\n");
    assert_eq!(states(), ["back/test missing", "ui/test passed"]);
}

// --- ADR 0043: hushrun itself, for a repository with no package.json -------------------------

/// What a test asked GitHub for.
#[cfg(unix)]
type Asked = Arc<Mutex<Vec<String>>>;

/// One release of hushrun for this machine at GitHub's addresses, whose binary is a script that
/// says `hushrun <version>`. A `broken` one's checksum doesn't match its archive.
#[cfg(unix)]
fn github(version: &str, broken: bool) -> (Server, Asked) {
    use sha2::Digest as _;
    use std::os::unix::fs::PermissionsExt as _;

    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        (os, arch) => panic!("no release for {arch} on {os}"),
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let name = format!("hushrun-{target}");
    let inner = dir.path().join(&name);
    std::fs::create_dir_all(&inner).expect("mkdir");
    let script = inner.join("hushrun");
    std::fs::write(&script, format!("#!/bin/sh\necho 'hushrun {version}'\n")).expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let file = format!("{name}.tar.xz");
    let status = Command::new("tar")
        .arg("-cJf")
        .arg(dir.path().join(&file))
        .arg("-C")
        .arg(dir.path())
        .arg(&name)
        .status()
        .expect("tar");
    assert!(status.success(), "tar -cJf");
    let archive = std::fs::read(dir.path().join(&file)).expect("read");
    let sum = hex::encode(sha2::Sha256::digest(if broken {
        b"something else".as_slice()
    } else {
        archive.as_slice()
    }));
    let sums = format!("{sum} *{file}\n");

    let asked: Asked = Arc::default();
    let seen = asked.clone();
    let version = version.to_string();
    let server = Server::start(move |request| {
        seen.lock().unwrap().push(request.path.clone());
        let at = format!("/hushrun/hushrun/releases/download/v{version}/{file}");
        Reply::Send(if request.path == "/hushrun/hushrun/releases/latest" {
            Response::new(302).header(
                "location",
                format!("https://github.com/hushrun/hushrun/releases/tag/v{version}"),
            )
        } else if request.path == at {
            Response::bytes(200, archive.clone())
        } else if request.path == format!("{at}.sha256") {
            Response::bytes(200, sums.clone().into_bytes())
        } else {
            Response::new(404)
        })
    });
    (server, asked)
}

/// A person who runs `npx hushrun`, in a repository's clone: their home, what is on their `PATH`,
/// and npm's copy of hushrun in npx's cache. No hushrun of the machine's is on `PATH`.
#[cfg(unix)]
struct Npx {
    home: PathBuf,
    binary: PathBuf,
    path: Vec<PathBuf>,
}

#[cfg(unix)]
impl Npx {
    /// `on_path` are directories under the home that `PATH` lists; none is made.
    fn new(r: &Repo, on_path: &[&str]) -> Npx {
        let home = r.root.join("home");
        let npx = r.root.join("npm/_npx/0a1b2c/node_modules");
        let package = npx.join("@hushrun/cli-this-machine/bin");
        std::fs::create_dir_all(&package).expect("mkdir");
        std::fs::create_dir_all(npx.join(".bin")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let binary = package.join("hushrun");
        std::fs::copy(env!("CARGO_BIN_EXE_hushrun"), &binary).expect("copy");
        let tool = npx.join(".bin/hushrun");
        let _ = std::os::unix::fs::symlink(&binary, tool);
        // The repository's fake tools, but for the npm package's hushrun.
        let _ = std::fs::remove_file(r.bin.join("hushrun"));
        let mut path = vec![npx.join(".bin")];
        path.extend(on_path.iter().map(|dir| home.join(dir)));
        path.extend([r.bin.clone(), "/usr/bin".into(), "/bin".into()]);
        Npx { home, binary, path }
    }

    fn command(&self, r: &Repo, fake: &Fake, github: &Server) -> Command {
        self.running(&self.binary, r, fake, github)
    }

    fn running(&self, binary: &std::path::Path, r: &Repo, fake: &Fake, github: &Server) -> Command {
        let mut cmd = r.program(binary, fake);
        cmd.env("PATH", std::env::join_paths(&self.path).expect("PATH"))
            .env("HOME", &self.home)
            .env("HUSHRUN_INSTALLER_GITHUB_BASE_URL", github.url());
        for var in [
            "XDG_CONFIG_HOME",
            "XDG_BIN_HOME",
            "CARGO_HOME",
            "GOBIN",
            "GOPATH",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn receipt(&self) -> PathBuf {
        self.home.join(".config/hushrun/hushrun-receipt.json")
    }
}

#[cfg(unix)]
const INSTALL_LINE: &str = "Install hushrun where you push from, so that the hook runs:\n  curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh\n";

// covers: C-15
#[cfg(unix)]
#[test]
fn under_npx_init_puts_hushrun_on_path_and_update_keeps_it() {
    let (_server, fake) = Fake::start(true);
    let (release, asked) = github("9.9.9", false);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".local/bin"]);
    let binary = npx.home.join(".local/bin/hushrun");

    // A dry run shows it and does nothing: no directory, no receipt, nothing asked of GitHub.
    let out = ran(npx.command(&r, &fake, &release), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let row = format!(
        "  {}\n    hushrun's newest release, on your PATH, so that the hook runs on this machine: hushrun update keeps it up to date\n",
        binary.display()
    );
    assert!(out.stdout.contains(&row), "{}", out.stdout);
    assert!(out.stdout.contains("Nothing changed (--dry-run)."));
    assert!(!npx.home.join(".local").exists());
    assert!(!npx.receipt().exists());
    assert!(asked.lock().unwrap().is_empty());

    // Applied, it is there before the closing lines, which then have no installer to mention.
    let out = ran(
        npx.command(&r, &fake, &release),
        &["init", "--yes", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(&row), "{}", out.stdout);
    assert!(
        out.stdout
            .contains(&format!("Installed hushrun in {}.\n", binary.display())),
        "{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("Install hushrun where you push from"),
        "{}",
        out.stdout
    );
    let said = Command::new(&binary)
        .arg("--version")
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&said.stdout), "hushrun 9.9.9\n");
    assert_eq!(
        std::fs::read_dir(binary.parent().expect("dir"))
            .expect("read")
            .count(),
        1,
        "nothing else is left there"
    );
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(npx.receipt()).expect("receipt")).expect("JSON");
    assert_eq!(receipt["version"], "9.9.9");
    assert_eq!(receipt["install_layout"], "flat");
    assert_eq!(receipt["modify_path"], false);
    assert_eq!(
        receipt["install_prefix"],
        json!(binary.parent().expect("dir").to_str().expect("utf-8"))
    );

    // `hushrun update` takes that copy for an installer's. A real binary stands in for the
    // script, older than the release.
    std::fs::remove_file(&binary).expect("rm");
    std::fs::copy(env!("CARGO_BIN_EXE_hushrun"), &binary).expect("copy");
    let (newer, _) = github("9.9.10", false);
    let out = ran(npx.running(&binary, &r, &fake, &newer), &["update"]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains(&format!("to 9.9.10 in {}.", binary.display())),
        "{}",
        out.stdout
    );
    let said = Command::new(&binary)
        .arg("--version")
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&said.stdout), "hushrun 9.9.10\n");
}

// covers: C-15
#[cfg(unix)]
#[test]
fn it_picks_the_installers_directory_when_that_is_on_path() {
    let (_server, fake) = Fake::start(true);
    let (release, _) = github("9.9.9", false);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".local/bin", "go/bin", ".cargo/bin"]);
    let out = ran(npx.command(&r, &fake, &release), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains(&format!(
            "  {}\n",
            npx.home.join(".cargo/bin/hushrun").display()
        )),
        "{}",
        out.stdout
    );

    // A Go developer's own directory counts, a version manager's doesn't.
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &["go/bin"]);
    let out = ran(npx.command(&r, &fake, &release), &["init", "--dry-run"]);
    assert!(
        out.stdout.contains(&format!(
            "  {}\n",
            npx.home.join("go/bin/hushrun").display()
        )),
        "{}",
        out.stdout
    );
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".nvm/versions/node/v24/bin", ".rbenv/shims"]);
    let out = ran(npx.command(&r, &fake, &release), &["init", "--dry-run"]);
    assert!(
        !out.stdout.contains("hushrun's newest release"),
        "{}",
        out.stdout
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_failed_install_is_never_a_failed_init() {
    let (_server, fake) = Fake::start(true);

    // A release whose checksum doesn't match.
    let (release, _) = github("9.9.9", true);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".local/bin"]);
    let out = ran(
        npx.command(&r, &fake, &release),
        &["init", "--yes", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("warning: hushrun isn't installed on this machine:"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("SHA-256"), "{}", out.stderr);
    assert!(out.stdout.contains(INSTALL_LINE), "{}", out.stdout);
    assert!(
        r.read("hushrun.yaml").contains("backend:"),
        "init went on with everything else"
    );
    assert!(!npx.home.join(".local/bin/hushrun").exists());
    assert!(!npx.receipt().exists());
    assert_eq!(
        std::fs::read_dir(npx.home.join(".local/bin"))
            .expect("read")
            .count(),
        0,
        "nothing is left there"
    );

    // GitHub out of reach: nothing listens there.
    let (release, _) = github("9.9.9", false);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".local/bin"]);
    let mut cmd = npx.command(&r, &fake, &release);
    cmd.env("HUSHRUN_INSTALLER_GITHUB_BASE_URL", "http://127.0.0.1:9");
    let out = ran(cmd, &["init", "--yes", "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(INSTALL_LINE), "{}", out.stdout);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn without_a_directory_for_it_init_says_how_to_install_hushrun() {
    let (_server, fake) = Fake::start(true);
    let (release, asked) = github("9.9.9", false);
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[]);
    let out = ran(
        npx.command(&r, &fake, &release),
        &["init", "--yes", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(INSTALL_LINE), "{}", out.stdout);
    assert!(
        out.stdout.contains(
            "in each clone, an agent's sandbox included, the first hushrun ready or hushrun run installs it"
        ),
        "{}",
        out.stdout
    );
    assert!(asked.lock().unwrap().is_empty());
    assert!(!npx.receipt().exists());
}

// covers: C-15
#[cfg(unix)]
#[test]
fn init_installs_hushrun_only_where_nothing_else_will() {
    let (_server, fake) = Fake::start(true);
    let (release, _) = github("9.9.9", false);
    let offered = |out: &Ran| out.stdout.contains("hushrun's newest release");

    // The dry run says so, here where it would.
    let r = Repo::new(&[("go.mod", "module shop\n")]);
    let npx = Npx::new(&r, &[".local/bin"]);
    let dry = ["init", "--dry-run"];
    assert!(offered(&ran(npx.command(&r, &fake, &release), &dry)));

    // In CI.
    let mut cmd = npx.command(&r, &fake, &release);
    cmd.env("CI", "true");
    assert!(!offered(&ran(cmd, &dry)));

    // A hushrun that isn't npm's, such as one built here.
    let built = env!("CARGO_BIN_EXE_hushrun").as_ref();
    assert!(!offered(&ran(
        npx.running(built, &r, &fake, &release),
        &dry
    )));

    // hushrun on PATH already, in any directory.
    let there = npx.home.join("tools");
    std::fs::create_dir_all(&there).expect("mkdir");
    std::fs::write(there.join("hushrun"), "").expect("write");
    let mut cmd = npx.command(&r, &fake, &release);
    let mut path = npx.path.clone();
    path.push(there.clone());
    cmd.env("PATH", std::env::join_paths(path).expect("PATH"));
    assert!(!offered(&ran(cmd, &dry)));

    // Another install that isn't on PATH, whose receipt update needs.
    let elsewhere = npx.home.join(".cargo/bin");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    std::fs::write(elsewhere.join("hushrun"), "").expect("write");
    std::fs::create_dir_all(npx.receipt().parent().expect("dir")).expect("mkdir");
    let receipt = json!({
        "binaries": ["hushrun"],
        "install_layout": "cargo-home",
        "install_prefix": npx.home.join(".cargo").to_str().expect("utf-8"),
        "source": {"app_name": "hushrun", "name": "hushrun", "owner": "hushrun", "release_type": "github"},
        "version": "0.1.0",
    });
    std::fs::write(npx.receipt(), receipt.to_string()).expect("write");
    assert!(!offered(&ran(npx.command(&r, &fake, &release), &dry)));
    // Whose binary is gone: then it is only an old file.
    std::fs::remove_file(elsewhere.join("hushrun")).expect("rm");
    assert!(offered(&ran(npx.command(&r, &fake, &release), &dry)));

    // With package.json, the hook runs the dependency.
    let r = shop();
    let npx = Npx::new(&r, &[".local/bin"]);
    assert!(!offered(&ran(npx.command(&r, &fake, &release), &dry)));
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_set_up_already_gets_it_from_the_same_plan() {
    let (_server, fake) = Fake::start(true);
    let (release, _) = github("9.9.9", false);
    let r = Repo::new(&[
        ("go.mod", "module shop\n"),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    permissions:\n      id-token: write\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- go test ./...\n",
        ),
    ]);
    r.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/shop\nbackend: {}\n",
            fake.backend()
        ),
    );
    git::commit_all(&r.repo, "set up");
    let npx = Npx::new(&r, &[".local/bin"]);
    let binary = npx.home.join(".local/bin/hushrun");

    let out = ran(npx.command(&r, &fake, &release), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("already uses hushrun"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("  {}\n", binary.display())),
        "{}",
        out.stdout
    );
    assert!(!binary.exists());

    let out = ran(
        npx.command(&r, &fake, &release),
        &["init", "--yes", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains(&format!("Installed hushrun in {}.\n", binary.display())),
        "{}",
        out.stdout
    );
    assert!(binary.exists());

    // When it fails there, the same line says how.
    let (broken, _) = github("9.9.9", true);
    std::fs::remove_file(&binary).expect("rm");
    std::fs::remove_file(npx.receipt()).expect("rm");
    let out = ran(
        npx.command(&r, &fake, &broken),
        &["init", "--yes", "--no-browser"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(INSTALL_LINE), "{}", out.stdout);
}

// --- ADR 0045: the rule init writes for Apple's tools -------------------------------------------

/// What the plan says of a repository of one scope.
const RULE_LINE: &str = "    counts_from: {os: macos}: every check hushrun runs here is on macOS in CI, with Xcode or Swift, so a pass on another system won't count\n";
/// What it says of the scope `ios`.
const IOS_RULE_LINE: &str = "    counts_from: {os: macos} in scope ios: every check hushrun runs there is on macOS in CI, with Xcode or Swift, so a pass on another system won't count\n";

/// A job `id`, on `runs_on`, whose steps run `commands`.
fn job(id: &str, runs_on: &str, commands: &[&str]) -> String {
    let mut text =
        format!("  {id}:\n    runs-on: {runs_on}\n    steps:\n      - uses: actions/checkout@v5\n");
    for command in commands {
        text.push_str(&format!("      - run: {command}\n"));
    }
    text
}

/// A workflow of `jobs`.
fn workflow(jobs: &[String]) -> String {
    format!(
        "name: CI\non: [push, pull_request]\njobs:\n{}",
        jobs.concat()
    )
}

/// The workflow of an iOS app: `swiftlint` and `xcodebuild test`, on macOS.
fn ios_workflow() -> String {
    workflow(&[
        job("lint", "macos-15", &["swiftlint --strict"]),
        job("test", "macos-15", &["xcodebuild test -scheme App"]),
    ])
}

/// `hushrun.yaml`'s rules at each level, as the file reads.
fn rules(r: &Repo) -> hushrun_core::counts_from::Rules {
    let text = r.read("hushrun.yaml");
    hushrun_core::policy::Policy::parse("hushrun.yaml", text.as_bytes())
        .expect("hushrun.yaml parses")
        .counts_from
}

/// `{os: macos}`.
fn macos() -> hushrun_core::counts_from::Level {
    hushrun_core::counts_from::Level::Rule(hushrun_core::counts_from::Rule {
        os: vec!["macos".to_string()],
        ..Default::default()
    })
}

/// The rule of the scope `name`.
fn scope_rule(r: &Repo, name: &str) -> Option<hushrun_core::counts_from::Level> {
    let scope = hushrun_core::names::ScopeName::new(name).expect("a name");
    rules(r).scopes.get(&scope).cloned()
}

/// A fake `tool` on the repository's PATH.
#[cfg(unix)]
fn with_tool(r: &Repo, tool: &str) {
    tool_doing(r, tool, "echo ran");
}

/// The same, doing `body`.
#[cfg(unix)]
fn tool_doing(r: &Repo, tool: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let path = r.bin.join(tool);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// What `init --yes` says, once it succeeded.
#[cfg(unix)]
fn init(r: &Repo, fake: &Fake) -> Ran {
    let out = ran(
        r.hushrun(fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    out
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn from_init_to_a_pass_in_an_ios_repository() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("App.xcodeproj/project.pbxproj", "// !$*UTF8*$!\n"),
        (".swiftlint.yml", "strict: true\n"),
        (".github/workflows/ci.yml", &ios_workflow()),
    ]);
    with_tool(&r, "swiftlint");
    with_tool(&r, "xcodebuild");

    let out = init(&r, &fake);
    assert!(
        out.stdout.contains(&format!(
            "  hushrun.yaml\n    a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log\n    backend: {}, where CI, teammates and agents share results\n{RULE_LINE}",
            fake.backend()
        )),
        "{}",
        out.stdout
    );
    assert_eq!(rules(&r).top, Some(macos()));
    assert!(
        r.read("hushrun.yaml")
            .contains("counts_from:\n  os: macos\n"),
        "{}",
        r.read("hushrun.yaml")
    );
    let ci = r.read(".github/workflows/ci.yml");
    assert!(
        ci.contains("run: hushrun run test -- xcodebuild test -scheme App\n"),
        "{ci}"
    );
    git::commit_all(&r.repo, "hushrun");

    // The rule is the machine's to meet: a Mac's run counts, and any other's doesn't.
    let run = |line: &[&str]| {
        let mut args = vec!["run"];
        args.extend_from_slice(line);
        ran(r.hushrun(&fake), &args)
    };
    let swiftlint: &[&str] = &["swiftlint", "--", "swiftlint", "--strict"];
    let test: &[&str] = &["test", "--", "xcodebuild", "test", "-scheme", "App"];
    for line in [swiftlint, test] {
        let first = run(line);
        assert_eq!(first.code, 0, "{}", first.stderr);
        let last = first.stderr.lines().last().expect("a verdict");
        let name = line[0];
        let again = run(line);
        if cfg!(target_os = "macos") {
            assert!(
                last.starts_with(&format!("hushrun: {name} passed in ")),
                "{last}"
            );
            assert!(last.ends_with(" on macos; it counts"), "{last}");
            assert!(
                again.stderr.contains(&format!("{name} skipped")),
                "{}",
                again.stderr
            );
        } else {
            assert!(
                last.contains("it won't count: needs macos, this is"),
                "{last}"
            );
            assert!(!again.stderr.contains("skipped"), "{}", again.stderr);
        }
    }
    let recorded = fake.results.lock().unwrap().len();
    assert_eq!(recorded > 0, cfg!(target_os = "macos"));

    // Run again, init changes nothing: the rule stays as it was written.
    let before = r.read("hushrun.yaml");
    let again = init(&r, &fake);
    assert!(
        again
            .stdout
            .contains("This repository already uses hushrun"),
        "{}",
        again.stdout
    );
    assert!(!again.stdout.contains("counts_from"), "{}", again.stdout);
    assert_eq!(r.read("hushrun.yaml"), before);
    assert_eq!(git::text(&r.repo, &["status", "--porcelain"]), "");
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_dry_run_shows_the_rule_and_writes_nothing() {
    let (_server, fake) = Fake::start(true);
    let workflow = ios_workflow();
    let r = Repo::new(&[(".github/workflows/ci.yml", &workflow)]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(RULE_LINE), "{}", out.stdout);
    assert!(out.stdout.contains("Nothing changed (--dry-run)."));
    assert!(!r.repo.join("hushrun.yaml").exists());
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
    assert!(fake.requests().is_empty());
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_node_job_on_a_mac_gets_no_rule() {
    for files in [
        // The scripts are wrapped at the root.
        vec![
            (
                "package.json",
                "{\n  \"scripts\": {\n    \"test\": \"eslint .\"\n  }\n}\n",
            ),
            ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        ],
        // CI runs them in a project.
        vec![("web/package.json", "{\"name\": \"web\"}\n")],
    ] {
        let (_server, fake) = Fake::start(true);
        let dir = if files.len() == 1 { "web" } else { "." };
        let workflow = format!(
            "{}        working-directory: {dir}\n",
            workflow(&[job("test", "macos-latest", &["npm test"])])
        );
        let mut all = files.clone();
        all.push((".github/workflows/ci.yml", &workflow));
        let r = Repo::new(&all);
        let out = init(&r, &fake);
        assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_script_of_package_json_keeps_the_rule_out_of_the_repository() {
    // `test` is Node's, and comes under a rule at the top with the rest, whether CI runs it or only
    // people do: nothing in CI's files says where a script runs.
    for commands in [
        &["npm test", "xcodebuild test -scheme App"][..],
        &["xcodebuild test -scheme App"],
    ] {
        let (_server, fake) = Fake::start(true);
        let workflow = workflow(&[job("ios", "macos-15", commands)]);
        let r = Repo::new(&[
            (
                "package.json",
                "{\n  \"scripts\": {\n    \"test\": \"eslint .\"\n  }\n}\n",
            ),
            ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
            (".github/workflows/ci.yml", &workflow),
        ]);
        let out = init(&r, &fake);
        assert!(
            out.stdout.contains(" -- xcodebuild test -scheme App"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_swift_package_tested_on_linux_too_gets_no_rule() {
    let (_server, fake) = Fake::start(true);
    let workflow = workflow(&[
        job("linux", "ubuntu-latest", &["swift test"]),
        job("mac", "macos-latest", &["swift test"]),
    ]);
    let r = Repo::new(&[
        ("Package.swift", ""),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = init(&r, &fake);
    assert!(
        out.stdout
            .contains("job mac: swift test → hushrun run mac-test")
    );
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
    assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_system_nothing_can_tell_gets_no_rule() {
    let matrix = "on: push\njobs:\n  lint:\n    runs-on: macos-15\n    steps:\n      - uses: actions/checkout@v5\n      - run: swiftlint\n  test:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: swift test\n";
    let expression = workflow(&[job("lint", "${{ vars.RUNNER }}", &["swiftlint"])]);
    for text in [matrix, &expression] {
        let (_server, fake) = Fake::start(true);
        let r = Repo::new(&[(".github/workflows/ci.yml", text)]);
        let out = init(&r, &fake);
        assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_gitlab_project_gets_no_rule_whatever_its_runner_is_called() {
    let (_server, fake) = Fake::start(true);
    let pipeline = "lint:\n  tags: [saas-macos-medium-m1]\n  script:\n    - swiftlint --strict\n";
    let r = Repo::new(&[(".gitlab-ci.yml", pipeline)]);
    let out = init(&r, &fake);
    assert!(r.read(".gitlab-ci.yml").contains("hushrun run swiftlint"));
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
    assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn what_a_file_says_of_counts_from_stays_exactly_as_written() {
    for rule in [
        "counts_from: {os: linux, go: \"1.25\"}\n",
        "counts_from: anywhere\n",
        "checks:\n  swiftlint:\n    counts_from: anywhere\n",
        "scopes:\n  ios:\n    include: [\"**\"]\n    counts_from: {os: macos}\n",
        "scopes:\n  ios:\n    include: [\"**\"]\n    counts_from: anywhere\n",
    ] {
        let (_server, fake) = Fake::start(true);
        let file = format!("version: 1\nrepository: github.com/acme/shop\n{rule}");
        let r = Repo::new(&[
            ("hushrun.yaml", &file),
            (".github/workflows/ci.yml", &ios_workflow()),
        ]);
        let before = rules(&r);
        let out = init(&r, &fake);
        assert!(
            !out.stdout.contains("counts_from"),
            "{rule}: {}",
            out.stdout
        );
        let after = r.read("hushrun.yaml");
        assert!(after.contains(rule), "{rule}: {after}");
        assert_eq!(after.matches("counts_from").count(), 1, "{after}");
        assert_eq!(rules(&r), before, "{after}");
    }
}

// covers: C-8, C-15, C-25
#[cfg(unix)]
#[test]
fn a_file_with_stages_gets_no_rule_and_no_word_of_one() {
    // With checks, and with stages alone, which nothing else keeps the rule out of.
    for stages in [
        "checks:\n  unit: {kind: source}\nstages:\n  staging:\n    require: [unit]\n",
        "stages:\n  staging:\n    branch: main\n",
    ] {
        let (_server, fake) = Fake::start(true);
        let file = format!("version: 1\nrepository: github.com/acme/shop\n{stages}");
        let r = Repo::new(&[
            ("hushrun.yaml", &file),
            (".github/workflows/ci.yml", &ios_workflow()),
        ]);
        let out = init(&r, &fake);
        assert!(
            out.stdout.contains("hushrun run swiftlint"),
            "{}",
            out.stdout
        );
        let said = format!("{}{}", out.stdout, out.stderr);
        assert!(!said.contains("counts_from"), "{said}");
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
        // ADR 0062: they're to move to `deploy`, which couldn't hold a rule (C-8).
        assert!(said.contains("leaves `stages` for you to move"), "{said}");
        assert!(r.read("hushrun.yaml").contains("stages:\n  staging:\n"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_repository_set_up_keeps_what_it_has() {
    // A rule would change its checks' keys, and it may be gone on purpose.
    let (_server, fake) = Fake::start(true);
    let file = format!(
        "version: 1\nrepository: github.com/acme/shop\nbackend: {}\n",
        fake.backend()
    );
    let workflow = workflow(&[job(
        "lint",
        "macos-15",
        &["hushrun run swiftlint -- swiftlint --strict"],
    )]);
    let r = Repo::new(&[
        ("hushrun.yaml", &file),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = init(&r, &fake);
    assert!(
        out.stdout.contains("This repository already uses hushrun"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
    assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
}

/// A monorepo's workflow: an iOS project and a Go backend.
fn monorepo_workflow() -> String {
    monorepo_with("go test ./...")
}

/// The same, with `api` for what the backend's job runs.
fn monorepo_with(api: &str) -> String {
    format!(
        "name: CI\non: [push, pull_request]\njobs:\n  ios:\n    runs-on: macos-15\n    defaults:\n      run:\n        working-directory: ios\n    steps:\n      - uses: actions/checkout@v5\n      - run: swiftlint --strict\n      - run: xcodebuild test -scheme App\n  api:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: api\n    steps:\n      - uses: actions/checkout@v5\n      - run: {api}\n"
    )
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_monorepos_rule_goes_in_the_scope_of_its_ios_project() {
    let (_server, fake) = Fake::start(true);
    let file = "version: 1\nrepository: github.com/acme/shop\nscopes:\n  ios:\n    include: [\"ios/**\"]\n  api:\n    include: [\"api/**\"]\n";
    let r = Repo::new(&[
        ("hushrun.yaml", file),
        ("ios/Package.swift", ""),
        ("api/go.mod", "module api\n"),
        (".github/workflows/ci.yml", &monorepo_workflow()),
    ]);
    with_tool(&r, "swiftlint");
    with_tool(&r, "go");
    let out = init(&r, &fake);
    assert!(out.stdout.contains(IOS_RULE_LINE), "{}", out.stdout);
    assert_eq!(scope_rule(&r, "ios"), Some(macos()));
    assert_eq!(scope_rule(&r, "api"), None);
    assert_eq!(rules(&r).top, None);
    assert!(
        r.read("hushrun.yaml")
            .contains("  ios:\n    include: [\"ios/**\"]\n    counts_from: {os: macos}\n  api:\n"),
        "{}",
        r.read("hushrun.yaml")
    );
    git::commit_all(&r.repo, "hushrun");

    // Each project's run says what it always would, but the rule's.
    let mut cmd = r.hushrun(&fake);
    cmd.current_dir(r.repo.join("ios"));
    let ios = ran(
        cmd,
        &[
            "run",
            "--scope",
            "ios",
            "swiftlint",
            "--",
            "swiftlint",
            "--strict",
        ],
    );
    let last = ios.stderr.lines().last().expect("a verdict");
    match cfg!(target_os = "macos") {
        true => assert!(last.ends_with(" on macos; it counts"), "{last}"),
        false => assert!(last.contains("it won't count: needs macos"), "{last}"),
    }
    let mut cmd = r.hushrun(&fake);
    cmd.current_dir(r.repo.join("api"));
    let api = ran(
        cmd,
        &["run", "--scope", "api", "test", "--", "go", "test", "./..."],
    );
    let last = api.stderr.lines().last().expect("a verdict");
    assert!(last.ends_with("it won't run again on this code"), "{last}");
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_scope_init_makes_holds_its_projects_rule() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("ios/package.json", "{\"name\": \"ios\"}\n"),
        ("api/go.mod", "module api\n"),
        (".github/workflows/ci.yml", &monorepo_workflow()),
    ]);
    let out = init(&r, &fake);
    assert!(out.stdout.contains(IOS_RULE_LINE), "{}", out.stdout);
    assert_eq!(scope_rule(&r, "ios"), Some(macos()));
    assert_eq!(scope_rule(&r, "api"), None);
    assert_eq!(rules(&r).top, None);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_check_outside_every_project_keeps_the_rule_out() {
    // It is left as it is, at the root, in no scope: nothing says what a rule would mean for it.
    let (_server, fake) = Fake::start(true);
    let workflow = format!(
        "{}{}",
        monorepo_workflow(),
        job("root", "ubuntu-latest", &["cargo test | tee log"])
    );
    let r = Repo::new(&[
        ("ios/package.json", "{\"name\": \"ios\"}\n"),
        ("api/go.mod", "module api\n"),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = init(&r, &fake);
    assert!(out.stdout.contains("scope ios: ios/**"), "{}", out.stdout);
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
    assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_monorepo_of_one_scope_gets_no_rule() {
    // Go's check shares the level, so nothing goes at the top, and hushrun reads no Swift project
    // into a scope of its own.
    for ios in ["ios/Package.swift", "ios/App.xcodeproj/project.pbxproj"] {
        let (_server, fake) = Fake::start(true);
        let r = Repo::new(&[
            (ios, ""),
            ("api/go.mod", "module api\n"),
            (".github/workflows/ci.yml", &monorepo_workflow()),
        ]);
        let out = init(&r, &fake);
        assert!(
            out.stdout.contains("One scope for the whole repository"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_script_in_a_scope_keeps_the_rule_out_of_it() {
    let (_server, fake) = Fake::start(true);
    let file = "version: 1\nrepository: github.com/acme/shop\nscopes:\n  ios:\n    include: [\"ios/**\"]\n  api:\n    include: [\"api/**\"]\n";
    let package =
        "{\n  \"scripts\": {\n    \"lint\": \"hushrun run --scope ios lint -- eslint .\"\n  }\n}\n";
    let r = Repo::new(&[
        ("hushrun.yaml", file),
        ("package.json", package),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        ("ios/Package.swift", ""),
        ("api/go.mod", "module api\n"),
        (".github/workflows/ci.yml", &monorepo_workflow()),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("hushrun run --scope ios swiftlint"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_scope_with_no_line_for_a_rule_is_a_persons_to_change() {
    let (_server, fake) = Fake::start(true);
    let file = "version: 1\nrepository: github.com/acme/shop\nscopes:\n  ios: {include: [\"ios/**\"]}\n  api: {include: [\"api/**\"]}\n";
    let r = Repo::new(&[
        ("hushrun.yaml", file),
        ("ios/Package.swift", ""),
        ("api/go.mod", "module api\n"),
        (".github/workflows/ci.yml", &monorepo_workflow()),
    ]);
    let out = init(&r, &fake);
    assert!(
        out.stdout.contains("Change by hand:\n  counts_from: {os: macos} in scope ios fits, as these checks run on macOS in CI, with Xcode or Swift, but hushrun.yaml has no line for it: add it if a pass on another system shouldn't count\n"),
        "{}",
        out.stdout
    );
    assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    assert!(
        r.read("hushrun.yaml")
            .contains("  ios: {include: [\"ios/**\"]}\n")
    );
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_rule_is_worth_a_file_of_its_own() {
    // package.json names the backend and nothing else asks for hushrun.yaml, and the org won't
    // give a ledger, so the file holds what it must, and the rule.
    let (_server, fake) = Fake::start(true);
    *fake.ledger_refusal.lock().unwrap() = Some((403, refusal("forbidden", "not an admin")));
    let package = format!(
        "{{\n  \"name\": \"app\",\n  \"hushrun\": {{\"backend\": \"{}\"}}\n}}\n",
        fake.backend()
    );
    let r = Repo::new(&[
        ("package.json", &package),
        (".github/workflows/ci.yml", &ios_workflow()),
    ]);
    let out = init(&r, &fake);
    assert!(out.stdout.contains(RULE_LINE), "{}", out.stdout);
    let text = r.read("hushrun.yaml");
    assert!(
        text.starts_with(
            "# hushrun.yaml: where this repository's events go.\nversion: 1\nrepository: github.com/acme/shop\n"
        ),
        "{text}"
    );
    assert_eq!(rules(&r).top, Some(macos()));
    assert!(!text.contains("repository_url"), "{text}");
    assert!(!text.contains("backend"), "{text}");
    assert!(
        out.stdout
            .contains("hushrun.yaml doesn't name the repository's URL on hushrun yet"),
        "{}",
        out.stdout
    );
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn with_no_origin_there_is_no_file_for_a_rule() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("package.json", "{\"name\": \"app\"}\n"),
        (".github/workflows/ci.yml", &ios_workflow()),
    ]);
    git::run(&r.repo, &["remote", "remove", "origin"], None);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("Change by hand:\n  counts_from: {os: macos} fits, as these checks run on macOS in CI, with Xcode or Swift, but no origin remote names this repository, so init can't write hushrun.yaml: add it if a pass on another system shouldn't count\n"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains(RULE_LINE), "{}", out.stdout);
    assert!(!r.repo.join("hushrun.yaml").exists());

    // With no package.json either, init wraps nothing, so it has nothing to say of a rule.
    std::fs::remove_file(r.repo.join("package.json")).expect("remove");
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_file_that_gets_a_rule_isnt_kept_as_it_is() {
    let (_server, fake) = Fake::start(true);
    let file = format!(
        "version: 1\nrepository: github.com/acme/shop\nrepository_url: {}/l/acme/shop\n\ntransparency_log:\n  origin: a.example/l/acme/shop\n  key: \"ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n",
        fake.url.get().unwrap()
    );
    let r = Repo::new(&[
        ("hushrun.yaml", &file),
        (".github/workflows/ci.yml", &ios_workflow()),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(RULE_LINE), "{}", out.stdout);
    assert!(!out.stdout.contains("keeps it as it is"), "{}", out.stdout);
}

// covers: C-15, C-17, C-25 (ADR 0046: an agent has no login to apply a plan with, and a plan is
// what signing out would show)
#[cfg(unix)]
#[test]
fn an_agent_sees_the_rule_in_a_plan_and_applies_none() {
    let (_server, fake) = Fake::start(true);
    let key = hushrun_attest::SigningKey::generate_p256().unwrap();
    let string = hushrun_core::agent::AgentString::encode(
        &fake.backend(),
        "hra_0123456789abcdefghijABCDEFGHIJ0123456789",
        key.to_pem().as_str(),
    );
    let workflow = ios_workflow();
    let r = Repo::new(&[(".github/workflows/ci.yml", &workflow)]);
    let as_agent = || {
        let mut cmd = r.hushrun(&fake);
        cmd.env("HUSHRUN_AGENT", &string);
        cmd
    };
    let plan = ran(as_agent(), &["init", "--dry-run"]);
    assert_eq!(plan.code, 0, "{}", plan.stderr);
    assert!(plan.stdout.contains(RULE_LINE), "{}", plan.stdout);
    assert!(
        plan.stderr
            .contains("HUSHRUN_AGENT is set, so init uses no login"),
        "{}",
        plan.stderr
    );
    let applied = ran(as_agent(), &["init", "--yes", "--no-browser", "--no-run"]);
    assert_eq!(applied.code, 2, "{}", applied.stderr);
    assert!(!r.repo.join("hushrun.yaml").exists());
    assert_eq!(r.read(".github/workflows/ci.yml"), workflow);
}

/// What `init --dry-run` plans for a workflow of one job on `macos-15` that runs `step`.
#[cfg(unix)]
fn planned_for(step: &str) -> (Repo, Ran) {
    let (_server, fake) = Fake::start(true);
    let text = format!(
        "on: push\njobs:\n  ios:\n    runs-on: macos-15\n    steps:\n      - uses: actions/checkout@v5\n{step}"
    );
    let r = Repo::new(&[(".github/workflows/ci.yml", &text)]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    (r, out)
}

/// A step of one line.
fn one_line(command: &str) -> String {
    format!("      - run: {command}\n")
}

/// The ways an iOS project's CI runs its tests.
const IOS_FORMS: &[&str] = &[
    "xcodebuild -scheme App -destination 'platform=iOS Simulator,name=iPhone 16' test",
    "xcodebuild -workspace App.xcworkspace -scheme App test",
    "xcodebuild clean test -scheme App",
    "xcodebuild test -scheme App",
    "xcodebuild test-without-building -scheme App",
    "xcodebuild test -scheme App | xcpretty",
    "xcodebuild test -scheme App | xcbeautify",
    "set -o pipefail && xcodebuild test -scheme App | xcpretty",
    "swift test",
];

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn the_ways_ios_projects_run_their_tests_get_a_rule() {
    for form in IOS_FORMS {
        let (_r, out) = planned_for(&one_line(form));
        assert!(out.stdout.contains(RULE_LINE), "{form}: {}", out.stdout);
        assert!(
            out.stdout.contains(" → hushrun run "),
            "{form}: {}",
            out.stdout
        );
    }
    // Over several lines, as most projects write it.
    let (_r, out) = planned_for(
        "      - run: |\n          set -o pipefail\n          xcodebuild test \\\n            -workspace App.xcworkspace \\\n            -scheme App | xcbeautify\n",
    );
    assert!(out.stdout.contains(RULE_LINE), "{}", out.stdout);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn what_doesnt_run_the_tests_stays_as_it_is() {
    for form in [
        "xcodebuild build -scheme App",
        "xcodebuild -scheme App build",
        "xcodebuild archive -scheme App",
        "xcodebuild -scheme App archive",
        "xcodebuild -showBuildSettings",
        "xcodebuild -showBuildSettings -scheme App",
        "xcodebuild -scheme App -showBuildSettings test",
        "xcodebuild -scheme test build",
        "xcodebuild -derivedDataPath test build",
        "fastlane test",
        "bundle exec fastlane test",
    ] {
        let (_r, out) = planned_for(&one_line(form));
        assert!(
            !out.stdout.contains(" → hushrun run "),
            "{form}: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("counts_from"),
            "{form}: {}",
            out.stdout
        );
    }
}

// covers: C-15, C-25, C-26
#[cfg(unix)]
#[test]
fn a_line_that_writes_a_report_is_wrapped_and_ci_runs_it_where_a_step_reads_it() {
    let codecov = "      - uses: codecov/codecov-action@v5\n";
    for form in [
        "xcodebuild test -scheme App -resultBundlePath r.xcresult",
        "xcodebuild test -scheme App -enableCodeCoverage YES",
        "swift test --enable-code-coverage",
    ] {
        // Nothing reads its report: CI skips it on a pass, as a laptop does.
        let (_r, out) = planned_for(&one_line(form));
        assert!(
            out.stdout
                .contains(&format!(": {form} → hushrun run test -- {form}\n")),
            "{form}: {}",
            out.stdout
        );
        assert!(out.stdout.contains(RULE_LINE), "{form}: {}", out.stdout);
        assert!(!out.stdout.contains("every time"), "{form}: {}", out.stdout);
        // A Codecov upload reads it: CI runs it every time, and init says so.
        let (_r, out) = planned_for(&format!("{}{codecov}", one_line(form)));
        assert!(
            out.stdout
                .contains(&format!(": {form} → hushrun run test -- {form}\n")),
            "{form}: {}",
            out.stdout
        );
        assert!(
            out.stdout.contains(
                "\nCI runs test every time: a later step reads its reports (codecov/codecov-action). Laptops and agents skip it on a pass.\n"
            ),
            "{form}: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("It leaves these"),
            "{form}: {}",
            out.stdout
        );
    }
    // A formatter's report is the same.
    let (_r, out) = planned_for(&one_line("xcodebuild test -scheme App | xcpretty -r junit"));
    assert!(
        out.stdout.contains("it pipes one command into another (|)"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains(" → hushrun run "), "{}", out.stdout);
}

// covers: C-15, C-26
#[cfg(unix)]
#[test]
fn init_says_how_to_let_ci_skip_a_check_codecov_reads() {
    let codecov = "      - uses: codecov/codecov-action@v5\n        with:\n          flags: ios\n";
    // The step that runs it has no id: init names one.
    let (_r, out) = planned_for(&format!(
        "{}{codecov}",
        one_line("swift test --enable-code-coverage")
    ));
    assert!(
        out.stdout.contains(
            "\nTo let CI skip test too, set reports.ci: skip for it in hushrun.yaml and give the Codecov step if: steps.test.outputs.hushrun-skipped != 'true', with id: test on the step that runs test. That is safe when Codecov carries the step's flag forward (carryforward: true) and after_n_builds doesn't count its upload.\n"
        ),
        "{}",
        out.stdout
    );
    // It has one: init uses it.
    let step = "      - id: unit\n        run: swift test --enable-code-coverage\n";
    let (_r, out) = planned_for(&format!("{step}{codecov}"));
    assert!(
        out.stdout
            .contains("if: steps.unit.outputs.hushrun-skipped != 'true'. That is safe when"),
        "{}",
        out.stdout
    );
    // init never writes the opt-in itself.
    assert!(!out.stdout.contains("ci: skip\n"), "{}", out.stdout);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn fastlanes_scan_stays_and_says_why() {
    for form in ["fastlane scan", "bundle exec fastlane scan"] {
        let (_r, out) = planned_for(&one_line(form));
        assert!(
            out.stdout.contains(&format!(
                "It leaves these commands in CI's files as they are:\n  .github/workflows/ci.yml, job ios: {form}: it writes reports to fastlane/test_output, which a skip wouldn't\n"
            )),
            "{form}: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains(" → hushrun run "),
            "{form}: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("counts_from"),
            "{form}: {}",
            out.stdout
        );
    }
    // It stops nothing beside what earns a rule.
    let (_r, out) = planned_for(&format!(
        "{}{}",
        one_line("xcodebuild test -scheme App"),
        one_line("bundle exec fastlane scan")
    ));
    assert!(out.stdout.contains(RULE_LINE), "{}", out.stdout);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_package_json_with_nothing_to_wrap_still_says_why_ci_stays() {
    let (_server, fake) = Fake::start(true);
    let workflow = workflow(&[job("ios", "macos-15", &["swift test | tee test.log"])]);
    let r = Repo::new(&[
        ("package.json", "{\"name\": \"tools\"}\n"),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("job ios: swift test: it pipes one command into another (|)\n"),
        "{}",
        out.stdout
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_repository_of_scripts_hears_nothing_new_of_what_ci_leaves() {
    // Its checks are wrapped, so what CI's files leave is no reason it wraps nothing.
    let (_server, fake) = Fake::start(true);
    let workflow = workflow(&[job(
        "test",
        "ubuntu-latest",
        &["npm test", "cargo deny check"],
    )]);
    let r = Repo::new(&[
        (
            "package.json",
            "{\n  \"scripts\": {\n    \"test\": \"eslint .\"\n  }\n}\n",
        ),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (".github/workflows/ci.yml", &workflow),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        !out.stdout.contains("It leaves these commands"),
        "{}",
        out.stdout
    );
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_package_json_doesnt_hide_a_javascript_check_from_the_rule() {
    let tools = "{\"name\": \"tools\", \"devDependencies\": {\"husky\": \"^9\"}}\n";
    let web = format!(
        "{}{}        working-directory: web\n",
        workflow(&[job("ios", "macos-15", &["xcodebuild test -scheme App"])]),
        job("web", "ubuntu-latest", &["npm test"]),
    );
    let jest = workflow(&[
        job("ios", "macos-15", &["xcodebuild test -scheme App"]),
        job("js", "ubuntu-latest", &["npx jest"]),
    ]);
    let left = workflow(&[
        job("ios", "macos-15", &["xcodebuild test -scheme App"]),
        job("js", "ubuntu-latest", &["npm test"]),
    ]);
    let left_script = "{\n  \"scripts\": {\n    \"test\": \"jest > out.txt\"\n  }\n}\n";
    for (package, workflow) in [(tools, &web), (tools, &jest), (left_script, &left)] {
        let (_server, fake) = Fake::start(true);
        let r = Repo::new(&[
            ("package.json", package),
            ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
            (".github/workflows/ci.yml", workflow),
        ]);
        let out = init(&r, &fake);
        assert!(
            out.stdout.contains("hushrun run test -- xcodebuild test"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("counts_from"), "{}", out.stdout);
        assert!(rules(&r).is_empty(), "{}", r.read("hushrun.yaml"));
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn what_a_job_hands_off_keeps_the_rule_out() {
    let xcode = job("test", "macos-15", &["xcodebuild test -scheme App"]);
    let reusable = "on: workflow_call\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: swift test\n";
    let composite = "name: t\nruns:\n  using: composite\n  steps:\n    - run: hushrun run unit -- go test ./...\n      shell: bash\n";
    let local = workflow(&[
        xcode.clone(),
        "  linux:\n    uses: ./.github/workflows/l.yml\n".into(),
    ]);
    let remote = workflow(&[
        xcode.clone(),
        "  linux:\n    uses: acme/ci/.github/workflows/go.yml@v1\n".into(),
    ]);
    let action = workflow(&[
        xcode,
        "  linux:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/t\n".into(),
    ]);
    for (ci, path, text) in [
        (&local, ".github/workflows/l.yml", reusable),
        (&remote, "README", "no\n"),
        (&action, ".github/actions/t/action.yml", composite),
    ] {
        let (_server, fake) = Fake::start(true);
        let r = Repo::new(&[(".github/workflows/ci.yml", ci), (path, text)]);
        let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stdout.contains(" → hushrun run "), "{}", out.stdout);
        assert!(!out.stdout.contains("counts_from"), "{ci}: {}", out.stdout);
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn swiftlint_earns_no_rule_of_its_own() {
    for step in [
        one_line("swiftlint --strict"),
        // What would earn one isn't going through hushrun.
        format!(
            "{}{}",
            one_line("swiftlint --strict"),
            one_line("xcodebuild test -scheme App | tee test.log")
        ),
        format!(
            "{}{}",
            one_line("swiftlint --strict"),
            one_line("bundle exec fastlane test")
        ),
    ] {
        let (_r, out) = planned_for(&step);
        assert!(
            out.stdout.contains("hushrun run swiftlint"),
            "{}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("counts_from"),
            "{step}: {}",
            out.stdout
        );
    }
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_check_left_in_another_declared_scope_stays_there() {
    // `go test` is left where it is, in `api`, whose rule that keeps out. `ios` is its own level.
    let (_server, fake) = Fake::start(true);
    let file = "version: 1\nrepository: github.com/acme/shop\nscopes:\n  ios:\n    include: [\"ios/**\"]\n  api:\n    include: [\"api/**\"]\n";
    let r = Repo::new(&[
        ("hushrun.yaml", file),
        ("ios/Package.swift", ""),
        ("api/go.mod", "module api\n"),
        (
            ".github/workflows/ci.yml",
            &monorepo_with("go test -coverprofile=c.out ./..."),
        ),
    ]);
    let out = init(&r, &fake);
    assert!(out.stdout.contains(IOS_RULE_LINE), "{}", out.stdout);
    assert_eq!(scope_rule(&r, "ios"), Some(macos()));
    assert_eq!(scope_rule(&r, "api"), None);
}

// covers: C-15, C-25
#[cfg(unix)]
#[test]
fn a_javascript_check_stays_in_its_own_scope() {
    // A package.json at the root keeps `npm test` out of what init wraps. Where it runs is known:
    // `api`'s level, and no reason to keep the rule from `ios`.
    let (_server, fake) = Fake::start(true);
    let file = "version: 1\nrepository: github.com/acme/shop\nscopes:\n  ios:\n    include: [\"ios/**\"]\n  api:\n    include: [\"api/**\"]\n";
    let r = Repo::new(&[
        ("hushrun.yaml", file),
        ("package.json", "{\"name\": \"tools\"}\n"),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        ("ios/Package.swift", ""),
        ("api/package.json", "{\"name\": \"api\"}\n"),
        (".github/workflows/ci.yml", &monorepo_with("npm test")),
    ]);
    let out = init(&r, &fake);
    assert!(out.stdout.contains(IOS_RULE_LINE), "{}", out.stdout);
    assert_eq!(scope_rule(&r, "ios"), Some(macos()));
    assert_eq!(scope_rule(&r, "api"), None);
}

// covers: C-9, C-15, C-22
#[cfg(unix)]
#[test]
fn a_step_over_several_lines_and_a_pipe_has_one_key() {
    // What the shell runs from the file, and what `hushrun ready` reads of it, are one key.
    let (_server, fake) = Fake::start(true);
    let block = "          set -o pipefail\n          cargo fetch && \\\n            cargo test \\\n            --locked | xcbeautify\n";
    let workflow = format!(
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: |\n{block}"
    );
    let r = Repo::new(&[("Cargo.toml", ""), (".github/workflows/ci.yml", &workflow)]);
    with_tool(&r, "cargo");
    tool_doing(&r, "xcbeautify", "cat");
    init(&r, &fake);
    let wrapped = "          set -o pipefail\n          cargo fetch && \\\n            hushrun run test -- cargo test \\\n            --locked | xcbeautify\n";
    let ci = r.read(".github/workflows/ci.yml");
    assert!(ci.contains(wrapped), "{ci}");
    git::commit_all(&r.repo, "hushrun");

    let ready = || {
        let out = ran(r.hushrun(&fake), &["ready", "--format", "json"]);
        serde_json::from_str::<Value>(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}{}", out.stdout, out.stderr))
    };
    let before = ready();
    assert_eq!(before["checks"][0]["state"], "missing", "{before:#}");
    assert_eq!(
        before["next"],
        json!(["hushrun run test -- cargo test --locked"]),
        "{before:#}"
    );

    let lines: String = wrapped.lines().map(|l| format!("{}\n", &l[10..])).collect();
    let step = || {
        let mut cmd = Command::new("bash");
        let like = r.hushrun(&fake);
        for (name, value) in like.get_envs() {
            match value {
                Some(value) => cmd.env(name, value),
                None => cmd.env_remove(name),
            };
        }
        cmd.current_dir(&r.repo)
            .args(["-eo", "pipefail", "-c", &lines])
            .output()
            .expect("bash")
    };
    let first = step();
    assert!(first.status.success(), "{first:?}");
    assert_eq!(ready()["checks"][0]["state"], "passed");
    // Through the pipe, a skip is a step that passes and prints nothing to it.
    let again = step();
    let said = String::from_utf8_lossy(&again.stderr);
    assert!(
        again.status.success() && said.contains("test skipped"),
        "{said}"
    );
}

// ---------------------------------------------------------------- the plan as JSON (C-15)

/// The repository the plan's tests read: package.json's scripts, wrapped, through hushrun already
/// and left; a job init wraps a command in and leaves another; a job with variants that runs a
/// wrapped script; and a job that runs hushrun itself.
const PLAN_PACKAGE: &str = r#"{
  "name": "shop",
  "scripts": {
    "test": "vitest run",
    "lint": "hushrun run -- eslint .",
    "typecheck": "tsc --watch"
  }
}
"#;

const PLAN_WORKFLOW: &str = "on: push
jobs:
  back:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: cargo test
      - run: cargo deny check
  web:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [20, 22]
    steps:
      - uses: actions/checkout@v5
      - run: npm test
  done:
    runs-on: ubuntu-latest
    steps:
      - uses: hushrun/hushrun/action/setup@v0
      - run: hushrun run unit -- go test ./...
";

#[cfg(unix)]
fn plan_repo() -> Repo {
    Repo::new(&[
        ("package.json", PLAN_PACKAGE),
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (".github/workflows/ci.yml", PLAN_WORKFLOW),
    ])
}

/// `hushrun init --dry-run --format json` in `r`: it succeeds, and prints one document, on one
/// line, that its schema takes.
#[cfg(unix)]
fn json_plan(cmd: Command) -> Value {
    let out = ran(cmd, &["init", "--dry-run", "--format", "json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    let plan: Value =
        serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout));
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("init-plan.v1.schema.json"),
        &plan,
    );
    plan
}

/// The job `id` of the plan.
fn plan_job<'p>(plan: &'p Value, id: &str) -> &'p Value {
    plan["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job"] == id)
        .unwrap_or_else(|| panic!("no job {id}: {plan:#}"))
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_dry_run_in_json_is_one_document_and_changes_nothing() {
    let (_server, fake) = Fake::start(true);
    let r = plan_repo();
    let text = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(plan["schema"], "hushrun.init-plan/v1");
    // HUSHRUN_FORMAT says it as well as --format does.
    let mut cmd = r.hushrun(&fake);
    cmd.env("HUSHRUN_FORMAT", "json");
    let out = ran(cmd, &["init", "--dry-run"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out.stdout).unwrap(),
        plan,
        "{}",
        out.stdout
    );
    // Nothing is written, installed, asked or sent, as with the text.
    assert_eq!(r.read("package.json"), PLAN_PACKAGE);
    assert_eq!(r.read(".github/workflows/ci.yml"), PLAN_WORKFLOW);
    assert!(!r.repo.join("hushrun.yaml").exists() && !r.repo.join("AGENTS.md").exists());
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
    assert!(r.log("npm.log").is_empty() && r.log("gh.args").is_empty());
    // The text stays as it was without --format.
    assert!(text.stdout.starts_with("Looking at "), "{}", text.stdout);
    assert!(
        text.stdout.ends_with("Nothing changed (--dry-run).\n"),
        "{}",
        text.stdout
    );
    // Without --dry-run, --format json changes nothing of init: it still asks first.
    let asks = ran(r.hushrun(&fake), &["init", "--format", "json"]);
    assert_eq!(asks.code, 2, "{}", asks.stdout);
    assert!(asks.stdout.starts_with("Looking at "), "{}", asks.stdout);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn the_plan_names_the_tool_the_repository_and_its_scopes() {
    let (_server, fake) = Fake::start(true);
    let plan = json_plan(plan_repo().hushrun(&fake));
    assert_eq!(plan["tool"], format!("hushrun {VERSION}"));
    assert_eq!(plan["repository"], "github.com/acme/shop");
    assert_eq!(plan["scopes"], json!([{"name": "default", "why": null}]));
    assert_eq!(plan["suggestions"], json!([]));

    // A project in a directory gets its own scope, as the text says.
    let workflow = "on: push\njobs:\n  back:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: back\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n";
    let r = Repo::new(&[
        ("back/Cargo.toml", "[package]\nname = \"back\"\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    let scope = &plan["scopes"][0];
    assert_eq!(scope["name"], "back", "{plan:#}");
    assert_eq!(scope["dir"], "back");
    assert!(
        scope["include"]
            .as_array()
            .unwrap()
            .contains(&json!("back/**")),
        "{scope}"
    );
    assert_eq!(
        plan_job(&plan, "back")["commands"][0]["after"],
        "hushrun run --scope back test -- cargo test"
    );

    // A workspace stays one scope, and says why.
    let r = Repo::new(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
        ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n        working-directory: a\n",
        ),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(
        plan["scopes"],
        json!([{"name": "default", "why": "Cargo.toml ties its projects together"}])
    );
    assert!(
        plan["notes"].as_array().unwrap().contains(&json!(
            "One scope for the whole repository: Cargo.toml ties its projects together. Any file's change runs every check; add scopes to hushrun.yaml by hand to narrow that."
        )),
        "{plan:#}"
    );
    // No origin remote: no id.
    git::run(&r.repo, &["remote", "remove", "origin"], None);
    assert_eq!(json_plan(r.hushrun(&fake))["repository"], Value::Null);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn the_plan_lists_the_scripts_it_wraps_finds_wrapped_and_leaves() {
    let (_server, fake) = Fake::start(true);
    let plan = json_plan(plan_repo().hushrun(&fake));
    assert_eq!(
        plan["scripts"],
        json!({
            "wrapped": [{
                "package": "package.json", "script": "test", "check": "test",
                "before": "vitest run", "after": "hushrun run -- vitest run",
            }],
            "already": [{
                "package": "package.json", "script": "lint", "check": "lint",
                "command": "hushrun run -- eslint .",
            }],
            "left": [{
                "package": "package.json", "script": "typecheck",
                "why": "it watches or serves (--watch)",
            }],
        }),
        "{plan:#}"
    );
}

// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn the_plan_says_what_each_job_runs_and_what_ci_will_decide() {
    let (_server, fake) = Fake::start(true);
    let plan = json_plan(plan_repo().hushrun(&fake));
    let back = plan_job(&plan, "back");
    assert_eq!(back["file"], ".github/workflows/ci.yml");
    assert_eq!(back["provider"], "github");
    assert_eq!(back["variants"], "one");
    assert_eq!(back["variantIdentity"], "none");
    assert_eq!(back["calledBy"], json!([]));
    assert_eq!(back["setup"], "added");
    assert_eq!(back["scripts"], json!([]));
    assert_eq!(
        back["commands"],
        json!([
            {
                "line": 7, "dir": "", "check": "test", "status": "wrapped",
                "before": "cargo test", "after": "hushrun run test -- cargo test", "why": null,
                "ci": "skips", "ciWhy": null, "ciText": null,
            },
            {
                "line": 8, "dir": null, "check": null, "status": "left",
                "before": "cargo deny check", "after": null,
                "why": "its answer changes with an advisory database, not only with the code",
                "ci": null, "ciWhy": null, "ciText": null,
            },
        ]),
        "{back:#}"
    );
    // [ADR 0049] A job with variants gets HUSHRUN_VARIANT in its env, so that the wrapped script
    // skips per variant.
    let web = plan_job(&plan, "web");
    assert_eq!(web["variants"], "several");
    assert_eq!(web["variantIdentity"], "env");
    assert_eq!(web["setup"], "none");
    assert_eq!(web["commands"], json!([]));
    assert_eq!(
        web["scripts"],
        json!([{"script": "test", "check": "test", "ci": "skips", "ciWhy": null, "ciText": null}]),
        "{web:#}"
    );
    // A line through hushrun already, in a job that installs it and checks nothing out: the run
    // there can't tell which code it is, so it runs, in words of its own.
    let done = plan_job(&plan, "done");
    assert_eq!(done["setup"], "present");
    assert_eq!(
        done["commands"],
        json!([{
            "line": 21, "dir": null, "check": "unit", "status": "already", "before": null, "after": null,
            "why": "its job doesn't check this repository out", "command": "go test ./...",
            "ci": "runs", "ciWhy": "cant-tell", "ciText": null,
        }]),
        "{done:#}"
    );
}

// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn the_plan_names_variants_named_per_job_and_the_files_that_call_a_workflow() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: [push, workflow_call]
jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        os: [a, b]
    steps:
      - uses: hushrun/hushrun/action/setup@v0
      - run: hushrun run test-${{ strategy.job-index }} -- cargo test
";
    let caller = "on: push\njobs:\n  call:\n    uses: ./.github/workflows/test.yml\n";
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (".github/workflows/test.yml", workflow),
        (".github/workflows/release.yml", caller),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    let test = plan_job(&plan, "test");
    assert_eq!(test["variants"], "several");
    // [ADR 0049] init writes HUSHRUN_VARIANT there too, and with both, the variable decides.
    assert_eq!(test["variantIdentity"], "env");
    assert_eq!(
        test["calledBy"],
        json!([{"file": ".github/workflows/release.yml", "job": "call"}])
    );
    // A name made per variant is one CI fills in: only the run can tell its key.
    assert_eq!(
        test["commands"],
        json!([{
            "line": 10, "dir": null, "check": null, "status": "already", "before": null, "after": null,
            "why": "its key can't be told here: the shell makes its words what they are",
            "ci": null, "ciWhy": null, "ciText": null,
        }]),
        "{test:#}"
    );
    // An env init doesn't edit leaves the name to tell the variants apart.
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (
            ".github/workflows/test.yml",
            &workflow.replace("    steps:\n", "    env: {CI: \"1\"}\n    steps:\n"),
        ),
        (".github/workflows/release.yml", caller),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(
        plan_job(&plan, "test")["variantIdentity"],
        "named",
        "{plan:#}"
    );
    // A file that doesn't read as YAML: its variants are unknown, and CI can't tell.
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo test\n  bad: [\n",
        ),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    let test = plan_job(&plan, "test");
    assert_eq!(test["variants"], "unknown", "{plan:#}");
}

// covers: C-15
#[cfg(unix)]
#[test]
fn the_plans_edits_are_what_init_writes() {
    let (_server, fake) = Fake::start(true);
    let dry = plan_repo();
    let plan = json_plan(dry.hushrun(&fake));
    let paths: Vec<&str> = plan["edits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edit| edit["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["package.json", ".github/workflows/ci.yml"],
        "{plan:#}"
    );
    // Each applies with git, as a person would review it.
    for edit in plan["edits"].as_array().unwrap() {
        let mut apply = Command::new("git");
        apply
            .current_dir(&dry.repo)
            .args(["apply", "-"])
            .stdin(Stdio::piped());
        let mut child = apply.spawn().expect("git apply");
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(edit["diff"].as_str().unwrap().as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "{}", edit["diff"]);
    }
    // And give what init writes once applied, the org's URL apart, which a dry run doesn't know.
    let applied = plan_repo();
    init(&applied, &fake);
    assert_eq!(
        dry.read(".github/workflows/ci.yml"),
        applied.read(".github/workflows/ci.yml")
    );
    assert_eq!(
        dry.read("package.json")
            .replace("<your org's URL, once you sign in>", &fake.backend()),
        applied.read("package.json")
    );
    // The files for agents are listed, not diffed.
    let agents: Vec<&str> = plan["agentFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        agents,
        [
            ".claude/skills/hushrun/SKILL.md",
            ".agents/skills/hushrun/SKILL.md",
            "AGENTS.md"
        ]
    );
    assert_eq!(
        plan["agentFiles"][2]["change"],
        "a new file: what agents do before they push"
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn the_plans_notes_say_what_its_text_says_beside_its_lists() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: ./build.sh\n",
    )]);
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(plan["jobs"], json!([]));
    // hushrun.yaml still names where results go, for a check someone wraps by hand.
    assert_eq!(
        plan["edits"],
        json!([{
            "path": "hushrun.yaml",
            "diff": "--- /dev/null\n+++ b/hushrun.yaml\n@@ -0,0 +1,4 @@\n+# hushrun.yaml: where this repository's events go.\n+version: 1\n+repository: github.com/acme/shop\n+backend: <your org's URL, once you sign in>\n",
        }]),
        "{plan:#}"
    );
    assert_eq!(
        plan["notes"][0],
        "There is no package.json here, and no check in CI's files that init knows, so init wraps no checks. Put hushrun run NAME -- in front of a check's command wherever it runs, CI included, such as\n  hushrun run test -- go test ./..."
    );
    // A repository set up already says so first, and lists what it runs.
    let r = Repo::new(&[
        (
            "package.json",
            r#"{"name": "shop", "scripts": {"test": "hushrun run -- vitest run"}, "hushrun": {"backend": "https://api.hushrun.com/o/acme"}}"#,
        ),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n",
        ),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    // As the text says it, word for word.
    assert_eq!(
        plan["notes"],
        json!([
            "This repository already uses hushrun: its checks share results at https://api.hushrun.com/o/acme.",
            "init signs in, and adds github.com/acme/shop to acme unless it is there already.",
        ]),
        "{plan:#}"
    );
    let text = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert!(
        text.stdout.contains(
            "This repository already uses hushrun: its checks share results at https://api.hushrun.com/o/acme.\ninit signs in, and adds github.com/acme/shop to acme unless it is there already.\n"
        ),
        "{}",
        text.stdout
    );
    assert_eq!(plan["scripts"]["already"][0]["script"], "test", "{plan:#}");
    assert_eq!(
        plan_job(&plan, "test")["scripts"],
        json!([{"script": "test", "check": "test", "ci": "skips", "ciWhy": null, "ciText": null}])
    );
}

// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn a_called_workflows_wrapped_script_is_predicted_as_ci_names_it() {
    let (_server, fake) = Fake::start(true);
    let called = "on: workflow_call\njobs:\n  unit:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";
    let caller = "on: push\njobs:\n  tests:\n    uses: ./.github/workflows/unit.yml\n";
    let r = Repo::new(&[
        (
            "package.json",
            r#"{"name": "shop", "scripts": {"test": "vitest run"}}"#,
        ),
        (".github/workflows/unit.yml", called),
        (".github/workflows/ci.yml", caller),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    let unit = plan_job(&plan, "unit");
    assert_eq!(
        unit["calledBy"],
        json!([{"file": ".github/workflows/ci.yml", "job": "tests"}])
    );
    // [ADR 0049] CI names the caller's file there, so the job names its own in its env, and the
    // run finds it: the script skips on that job's pass.
    assert_eq!(unit["variantIdentity"], "env", "{unit:#}");
    assert_eq!(
        unit["scripts"],
        json!([{"script": "test", "check": "test", "ci": "skips", "ciWhy": null, "ciText": null}]),
        "{unit:#}"
    );
    let diff = |path: &str| {
        plan["edits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|edit| edit["path"] == path)
            .and_then(|edit| edit["diff"].as_str())
            .unwrap_or_else(|| panic!("no edit of {path}: {plan:#}"))
            .to_string()
    };
    assert!(
        diff(".github/workflows/unit.yml")
            .contains("+    env:\n+      HUSHRUN_WORKFLOW: .github/workflows/unit.yml\n"),
        "{plan:#}"
    );
    // A called workflow has no more permissions than its caller gives it.
    assert!(
        diff(".github/workflows/ci.yml").contains("+      id-token: write\n"),
        "{plan:#}"
    );
    // The text says it.
    let text = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert!(
        text.stdout.contains(
            ".github/workflows/unit.yml is called by .github/workflows/ci.yml: its jobs say which file they are, so CI can skip there too."
        ),
        "{}",
        text.stdout
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn what_init_cant_write_for_it_leaves_and_says_why() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push\njobs:\n  back:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n";
    // No origin remote and no package.json: nothing can name where results go, so init wraps
    // nothing, and the plan doesn't say it would.
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    git::run(&r.repo, &["remote", "remove", "origin"], None);
    let plan = json_plan(r.hushrun(&fake));
    let back = plan_job(&plan, "back");
    assert_eq!(back["setup"], "none");
    assert_eq!(
        back["commands"],
        json!([{
            "line": 7, "dir": null, "check": "test", "status": "left", "before": "cargo test",
            "after": null,
            "why": "no origin remote names this repository, so init can't write hushrun.yaml, and wraps no checks",
            "ci": null, "ciWhy": null, "ciText": null,
        }]),
        "{back:#}"
    );
    assert_eq!(plan["edits"], json!([]));
    // A hushrun.yaml that doesn't parse is the person's to fix: CI's files stay as they are.
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        ("hushrun.yaml", "version: [\n"),
        (".github/workflows/ci.yml", workflow),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(
        plan_job(&plan, "back")["commands"][0]["why"],
        "hushrun.yaml doesn't parse, so init leaves it and CI's files as they are"
    );
    assert_eq!(plan["edits"], json!([]), "{plan:#}");
}

// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn the_plan_follows_where_a_run_is_and_what_starts_it() {
    let (_server, fake) = Fake::start(true);
    let workflow = "on: push
jobs:
  web:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: |
          cd web
          hushrun run unit -- cargo test
  bare:
    runs-on: ubuntu-latest
    steps:
      - run: npm test
  each:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [20, 22]
    steps:
      - uses: actions/checkout@v5
      - run: hushrun run ci -- npm test
";
    let r = Repo::new(&[
        (
            "package.json",
            r#"{"name": "shop", "scripts": {"test": "vitest run"}}"#,
        ),
        (".github/workflows/ci.yml", workflow),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    // A `cd` on one line holds on the next, as the shell reads the step.
    let web = &plan_job(&plan, "web")["commands"][0];
    assert_eq!(
        (&web["line"], &web["dir"]),
        (&json!(9), &json!("web")),
        "{web:#}"
    );
    // A script in a job that checks nothing out can't run where hushrun would see it.
    assert_eq!(
        plan_job(&plan, "bare")["scripts"],
        json!([{
            "script": "test", "check": "test", "ci": "runs", "ciWhy": "cant-tell", "ciText": null,
            "why": "its job doesn't check this repository out",
        }])
    );
    // A script a run starts is inside it, and the run that decides says so.
    assert_eq!(
        plan_job(&plan, "each")["scripts"][0]["ciText"],
        "job each of .github/workflows/ci.yml has several variants, which a run's key doesn't tell apart, and this run is inside another, so no pass skips it"
    );
}

// covers: C-14, C-15
#[cfg(unix)]
#[test]
fn a_reusable_workflow_is_predicted_as_its_top_caller_names_it() {
    let (_server, fake) = Fake::start(true);
    // [ADR 0049] Its env is on one line, which init doesn't edit: the run names its job as before
    // it said which file it is.
    let leaf = "on: workflow_call\njobs:\n  unit:\n    runs-on: ubuntu-latest\n    env: {CI: \"1\"}\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";
    let r = Repo::new(&[
        (
            "package.json",
            r#"{"name": "shop", "scripts": {"test": "vitest run"}}"#,
        ),
        (".github/workflows/leaf.yml", leaf),
        (
            ".github/workflows/mid.yml",
            "on: workflow_call\njobs:\n  tests:\n    uses: ./.github/workflows/leaf.yml\n",
        ),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  all:\n    uses: ./.github/workflows/mid.yml\n",
        ),
        // Called from elsewhere only: its caller can't be read here.
        (
            ".github/workflows/shared.yml",
            &leaf.replace("unit:", "shared:"),
        ),
        // Called, and pushed too: when called, it runs.
        (
            ".github/workflows/both.yml",
            &leaf
                .replace("on: workflow_call", "on: [push, workflow_call]")
                .replace("unit:", "both:"),
        ),
        (
            ".github/workflows/calls-both.yml",
            "on: push\njobs:\n  b:\n    uses: ./.github/workflows/both.yml\n",
        ),
    ]);
    let plan = json_plan(r.hushrun(&fake));
    let script = |job: &str| plan_job(&plan, job)["scripts"][0].clone();
    // GitHub names the top-level workflow, two calls up.
    assert_eq!(
        script("unit")["ciText"],
        ".github/workflows/ci.yml has no job unit, as with a reusable workflow's, so hushrun can't tell its variants, and no pass skips it"
    );
    assert_eq!(
        script("shared"),
        json!({"script": "test", "check": "test", "ci": "runs", "ciWhy": "reusable", "ciText": null})
    );
    assert_eq!(script("both")["ciWhy"], "reusable", "{plan:#}");
    // What a person adds instead.
    assert!(
        plan.to_string().contains(
            "its env is written on one line: add HUSHRUN_WORKFLOW: .github/workflows/leaf.yml to it"
        ),
        "{plan:#}"
    );
}

// covers: C-15
#[cfg(unix)]
#[test]
fn the_plans_notes_say_what_the_org_and_hushrun_yaml_get_once_signed_in() {
    let (_server, fake) = Fake::start(true);
    let plan = json_plan(plan_repo().hushrun(&fake));
    let notes: Vec<&str> = plan["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(
        notes,
        [
            "your org: connects github.com/acme/shop, so that its CI records results with its own ID token",
            "your org: adds github.com/acme/shop to your org's dashboard, unless it is there already",
            "hushrun.yaml: a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log",
        ],
        "{plan:#}"
    );
    // And the text says the same, under the same headings.
    let text = ran(plan_repo().hushrun(&fake), &["init", "--dry-run"]);
    for line in [
        "  your org\n    connects github.com/acme/shop, so that its CI records results with its own ID token\n    adds github.com/acme/shop to your org's dashboard, unless it is there already\n",
        "  hushrun.yaml\n    a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log\n",
    ] {
        assert!(text.stdout.contains(line), "{line}: {}", text.stdout);
    }
}

// --- ADR 0050: what init could also wrap -------------------------------------------------------

/// Test jobs that run a check init knows, and test scripts it doesn't.
const TEST_SCRIPTS: &str = "name: CI\non: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test --locked\n      - run: ./scripts/test.sh\n  unit:\n    name: Unit tests\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: bash scripts/ci/test.sh\n";

// covers: C-15
#[cfg(unix)]
#[test]
fn a_test_script_is_suggested_with_its_line_and_never_applied() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        ("scripts/test.sh", "#!/bin/sh\necho ok\n"),
        ("scripts/ci/test.sh", "#!/bin/sh\necho ok\n"),
        (".github/workflows/ci.yml", TEST_SCRIPTS),
    ]);
    // The text names each with the line to use, and says what a job without hushrun needs.
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        dry.stdout.contains(concat!(
            "\nCould also wrap these scripts, if they only run checks: a script can deploy or write reports, so init leaves them to you.\n",
            "  .github/workflows/ci.yml, job test: ./scripts/test.sh → hushrun run test-test -- ./scripts/test.sh\n",
            "  .github/workflows/ci.yml, job unit: bash scripts/ci/test.sh → hushrun run unit-test -- bash scripts/ci/test.sh\n",
            "    install hushrun in its job first, as init does in the jobs it wraps\n",
        )),
        "{}",
        dry.stdout
    );
    assert!(
        dry.stdout.contains(
            "    job test: cargo test --locked → hushrun run test -- cargo test --locked\n"
        ),
        "{}",
        dry.stdout
    );
    assert!(
        !dry.stdout.contains("\n    job test: ./scripts"),
        "{}",
        dry.stdout
    );
    assert!(!dry.stdout.contains("\n    job unit:"), "{}", dry.stdout);
    // The plan as JSON lists them, and no edit holds them.
    let plan = json_plan(r.hushrun(&fake));
    let why = "it runs a script, which can deploy or write reports: wrap it if it only runs checks";
    assert_eq!(
        plan["suggestions"],
        json!([
            {"file": ".github/workflows/ci.yml", "job": "test", "line": 9, "dir": "", "check": "test-test",
             "before": "./scripts/test.sh", "after": "hushrun run test-test -- ./scripts/test.sh",
             "setup": "added", "why": why},
            {"file": ".github/workflows/ci.yml", "job": "unit", "line": 15, "dir": "", "check": "unit-test",
             "before": "bash scripts/ci/test.sh", "after": "hushrun run unit-test -- bash scripts/ci/test.sh",
             "setup": "none", "why": why},
        ]),
        "{plan:#}"
    );
    let added: Vec<&str> = plan["edits"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|edit| edit["diff"].as_str().unwrap().lines())
        .filter(|line| line.starts_with('+') && line.contains("scripts/"))
        .collect();
    assert!(added.is_empty(), "{added:?}");
    // Applied, init wraps what it knows and leaves each script as it is.
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("\nCould also wrap these scripts"),
        "{}",
        out.stdout
    );
    let workflow = r.read(".github/workflows/ci.yml");
    assert!(
        workflow.contains(
            "      - run: hushrun run test -- cargo test --locked\n      - run: ./scripts/test.sh\n"
        ),
        "{workflow}"
    );
    assert!(
        workflow.contains("      - run: bash scripts/ci/test.sh\n"),
        "{workflow}"
    );
    assert_eq!(workflow.matches("hushrun run").count(), 1, "{workflow}");
}

// --- ADR 0050: what a tool's own remote cache keeps --------------------------------------------

/// Why init leaves what runs through Turborepo, once its remote cache is set up.
const TURBO_WHY: &str = "Turborepo reuses its own results across machines (remote cache), so init leaves it to Turborepo";

/// A Rust repository whose job `test` runs Turborepo and `cargo test`, with `env` for the job's.
fn turbo_repo(env: &str) -> Repo {
    let workflow = format!(
        "name: CI\non: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-latest\n{env}    steps:\n      - uses: actions/checkout@v5\n      - run: npx turbo run test\n      - run: cargo test --locked\n"
    );
    Repo::new(&[
        ("Cargo.toml", "[package]\nname = \"shop\"\n"),
        (".github/workflows/ci.yml", &workflow),
    ])
}

// covers: C-15
#[cfg(unix)]
#[test]
fn turborepo_with_a_token_is_stepped_aside_with_the_line() {
    let (_server, fake) = Fake::start(true);
    // The token comes from a secret, in the job's own env.
    let r = turbo_repo("    env:\n      TURBO_TOKEN: ${{ secrets.TURBO_TOKEN }}\n");
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        dry.stdout.contains(&format!(
            "\nIt leaves these commands in CI's files as they are:\n  .github/workflows/ci.yml, job test: npx turbo run test: {TURBO_WHY}\n"
        )),
        "{}",
        dry.stdout
    );
    // What no remote cache keeps is wrapped as before, in the same job.
    assert!(
        dry.stdout.contains(
            "    job test: cargo test --locked → hushrun run test -- cargo test --locked\n"
        ),
        "{}",
        dry.stdout
    );
    let plan = json_plan(r.hushrun(&fake));
    let job = plan_job(&plan, "test");
    assert_eq!(
        job["commands"][0],
        json!({
            "line": 10, "dir": null, "check": null, "status": "left",
            "before": "npx turbo run test", "after": null, "why": TURBO_WHY,
            "ci": null, "ciWhy": "stepped-aside", "ciText": null,
        }),
        "{plan:#}"
    );
    assert_eq!(job["commands"][1]["status"], "wrapped", "{plan:#}");
    // Applied, the line stays as it is.
    let out = ran(
        r.hushrun(&fake),
        &["init", "--yes", "--no-browser", "--no-run"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let written = r.read(".github/workflows/ci.yml");
    assert!(
        written.contains("      - run: npx turbo run test\n"),
        "{written}"
    );
    assert!(
        written.contains("      - run: hushrun run test -- cargo test --locked\n"),
        "{written}"
    );
    assert_eq!(written.matches("hushrun run").count(), 1, "{written}");
    // Without a remote cache nothing changes: init knows no Turborepo, and says nothing of it.
    let r = turbo_repo("");
    let plan = json_plan(r.hushrun(&fake));
    let commands = plan_job(&plan, "test")["commands"].as_array().unwrap();
    assert!(
        commands.iter().all(|c| c["before"] != "npx turbo run test"),
        "{plan:#}"
    );
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert!(!dry.stdout.contains("Turborepo"), "{}", dry.stdout);
}

// covers: C-15
#[cfg(unix)]
#[test]
fn a_script_through_turborepo_with_a_remote_cache_stays_as_it_is() {
    let (_server, fake) = Fake::start(true);
    let package = "{\n  \"name\": \"shop\",\n  \"scripts\": {\n    \"test\": \"turbo run test\",\n    \"lint\": \"eslint .\"\n  }\n}\n";
    let workflow = "name: CI\non: [push, pull_request]\nenv:\n  TURBO_TEAM: acme\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm ci\n      - run: npm test\n      - run: npm run lint\n";
    let repo = |workflow: &str| {
        Repo::new(&[
            ("package.json", package),
            ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
            (".github/workflows/ci.yml", workflow),
        ])
    };
    let r = repo(workflow);
    let dry = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        dry.stdout.contains(&format!(
            "\nIt leaves these scripts as they are:\n  \"test\": {TURBO_WHY}\n"
        )),
        "{}",
        dry.stdout
    );
    assert!(
        dry.stdout
            .contains("    \"lint\": \"eslint .\" → \"hushrun run -- eslint .\"\n"),
        "{}",
        dry.stdout
    );
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(
        plan["scripts"]["left"],
        json!([{"package": "package.json", "script": "test", "why": TURBO_WHY, "ciWhy": "stepped-aside"}]),
        "{plan:#}"
    );
    let wrapped = |plan: &Value| -> Vec<String> {
        plan["scripts"]["wrapped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["script"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(wrapped(&plan), ["lint"]);
    // Without a remote cache, init wraps it as before.
    let r = repo(&workflow.replace("env:\n  TURBO_TEAM: acme\n", ""));
    let plan = json_plan(r.hushrun(&fake));
    assert_eq!(wrapped(&plan), ["test", "lint"]);
    assert_eq!(plan["scripts"]["left"], json!([]), "{plan:#}");
}

// covers: C-15, C-35 (ADR 0071: init proposes the line Vitest's config needs, and writes none)
#[test]
fn init_proposes_the_line_vitest_needs_for_a_check_that_looks_at_its_test_files() {
    let (_server, fake) = Fake::start(true);
    let r = Repo::new(&[
        ("package.json", PACKAGE),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  test:\n    tests: reuse\n",
        ),
        ("vitest.config.ts", "export default { test: {} }\n"),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let line = "vitest.config.ts: add `plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]`, so that hushrun can look at Vitest's test files for test (tests in hushrun.yaml)";
    assert!(
        out.stdout.contains("Change by hand:") && out.stdout.contains(line),
        "{}",
        out.stdout
    );
    assert_eq!(r.read("vitest.config.ts"), "export default { test: {} }\n");
    // With the line there, or no check that looks at its test files, it proposes nothing.
    r.write(
        "vitest.config.ts",
        "export default { plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())] }\n",
    );
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert!(!out.stdout.contains("HUSHRUN_VITEST"), "{}", out.stdout);
    r.write("vitest.config.ts", "export default { test: {} }\n");
    r.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  test:\n    tests: off\n",
    );
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert!(!out.stdout.contains("HUSHRUN_VITEST"), "{}", out.stdout);
}

// covers: C-15, C-44 (ADR 0082: init proposes the line Jest's config needs where a script replaces
// NODE_OPTIONS, and writes none)
#[test]
fn init_proposes_the_line_jest_needs_where_a_script_replaces_node_options() {
    let (_server, fake) = Fake::start(true);
    let package = r#"{
  "name": "shop",
  "version": "1.0.0",
  "private": true,
  "scripts": {"test": "NODE_OPTIONS=--experimental-vm-modules jest"},
  "devDependencies": {"jest": "^30.5.2"}
}
"#;
    let r = Repo::new(&[
        ("package.json", package),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  test:\n    tests: reuse\n",
        ),
        ("jest.config.cjs", "module.exports = {};\n"),
    ]);
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let line = "jest.config.cjs: add `process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);` as its first line, so that hushrun can look at Jest's test files for test (tests in hushrun.yaml): the script test sets NODE_OPTIONS, which drops hushrun's adapter";
    assert!(
        out.stdout.contains("Change by hand:") && out.stdout.contains(line),
        "{}",
        out.stdout
    );
    assert_eq!(r.read("jest.config.cjs"), "module.exports = {};\n");
    // With the line there, or a script that keeps what NODE_OPTIONS had, it proposes nothing.
    r.write(
        "jest.config.cjs",
        "process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);\nmodule.exports = {};\n",
    );
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert!(!out.stdout.contains("HUSHRUN_JEST"), "{}", out.stdout);
    r.write("jest.config.cjs", "module.exports = {};\n");
    r.write(
        "package.json",
        &package
            .replace("NODE_OPTIONS=--", "NODE_OPTIONS=\\\"$NODE_OPTIONS --")
            .replace("modules jest", "modules\\\" jest"),
    );
    let out = ran(r.hushrun(&fake), &["init", "--local", "--dry-run"]);
    assert!(!out.stdout.contains("HUSHRUN_JEST"), "{}", out.stdout);
}

/// \[C-38\] What init says a GitHub job needs, for a backend that keeps no hints.
const CACHE_STEP_GITHUB: &str = "GitHub Actions: in each job that runs a check with `tests` on, add a step `uses: actions/cache@v4` before hushrun's, with `path: ~/.cache/hushrun/units/*.sqlite`";

/// A shop whose `test` check reuses test files.
fn shop_with_tests() -> Repo {
    Repo::new(&[
        ("package.json", PACKAGE),
        ("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        (".github/workflows/ci.yml", WORKFLOW),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  test:\n    tests: reuse\n",
        ),
    ])
}

fn asked_for_hints(fake: &Fake) -> usize {
    fake.requests()
        .iter()
        .filter(|(_, path)| path == "/o/acme/v0/units/hints/lookup")
        .count()
}

// covers: C-15, C-38 (ADR 0075: CI's cache step, for a backend that keeps no hints)
#[test]
fn init_says_the_cache_step_ci_needs_when_the_backend_keeps_no_hints() {
    let (_server, fake) = Fake::start(true);
    let r = shop_with_tests();
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(asked_for_hints(&fake), 1, "{:?}", fake.requests());
    assert!(
        out.stdout.contains("Change by hand:") && out.stdout.contains(CACHE_STEP_GITHUB),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("key: hushrun-units-${{ runner.os }}-${{ github.sha }}"),
        "{}",
        out.stdout
    );
    // A repository with GitHub workflows and no GitLab pipeline: GitHub's line only.
    assert!(!out.stdout.contains("GitLab CI:"), "{}", out.stdout);
    // init edits no CI file for it.
    assert!(!r.read(".github/workflows/ci.yml").contains("actions/cache"));
    // Set up already, it says so again, as it reads the backend once more.
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains(CACHE_STEP_GITHUB), "{}", out.stdout);
}

// covers: C-15, C-38
#[test]
fn init_says_no_cache_step_where_the_backend_keeps_hints_nor_with_local_or_a_dry_run() {
    // The backend keeps hints: the step would carry what it gives already.
    let (_server, fake) = Fake::start(true);
    fake.hints.store(true, Ordering::SeqCst);
    let r = shop_with_tests();
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(asked_for_hints(&fake), 1, "{:?}", fake.requests());
    assert!(!out.stdout.contains("actions/cache"), "{}", out.stdout);

    // --local: CI keeps no results, so the step would spare nothing; nothing is asked.
    let (_server, fake) = Fake::start(true);
    let r = shop_with_tests();
    let out = ran(r.hushrun(&fake), &["init", "--local", "--yes", "--no-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(fake.requests(), []);
    assert!(!out.stdout.contains("actions/cache"), "{}", out.stdout);

    // --dry-run sends nothing, and says nothing of the step.
    let (_server, fake) = Fake::start(true);
    let r = shop_with_tests();
    r.sign_in(&fake);
    let out = ran(r.hushrun(&fake), &["init", "--dry-run"]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(asked_for_hints(&fake), 0, "{:?}", fake.requests());
    assert!(!out.stdout.contains("actions/cache"), "{}", out.stdout);
}

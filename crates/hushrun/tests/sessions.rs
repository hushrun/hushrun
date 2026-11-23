//! \[ADR 0105\] A sandbox's credentials (C-51, C-52): `HUSHRUN_AUTH=proxy`, where the network adds
//! the credential; a placeholder in `HUSHRUN_TOKEN` that the network replaces; `HUSHRUN_AUTH=bearer`;
//! and session tokens, which `hushrun login --session` swaps an agent token, a Cursor cloud agent's
//! ID token or a GitHub Actions job's ID token for, and which runs then read from `sessions.json`.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::Value;

/// What a test must never inherit: CI's, hushrun's, and what says an agent or a sandbox runs it.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_AUTH",
    "HUSHRUN_BACKEND",
    "HUSHRUN_HOST",
    "HUSHRUN_TOKEN",
    "HUSHRUN_MACHINE_KEY",
    "HUSHRUN_MACHINE_KEY_FILE",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_SANDBOX",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CLAUDE_CODE_REMOTE",
    "CURSOR_AGENT",
    "CURSOR_AGENT_SOCKET",
    "CURSOR_EXTENSION_HOST_ROLE",
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "CODEX_PROXY_CERT",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
];

/// A repository whose package.json names the fake's org, and a home of its own.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    backend: String,
}

impl Repo {
    fn new(fake: &FakeResults) -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = root.join("shop");
        std::fs::create_dir_all(&repo).unwrap();
        let backend = fake.url();
        std::fs::write(
            repo.join("package.json"),
            format!(r#"{{"name": "shop", "hushrun": {{"backend": "{backend}"}}}}"#),
        )
        .unwrap();
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/shop.git",
            ],
            None,
        );
        git::commit_all(&repo, "one");
        Repo {
            _dir: dir,
            root,
            repo,
            backend,
        }
    }

    fn origin(&self) -> String {
        self.backend.trim_end_matches("/o/acme").to_string()
    }

    fn sessions(&self) -> PathBuf {
        self.root.join("config/sessions.json")
    }

    fn hushrun(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env(
                "HUSHRUN_CREDENTIALS",
                self.root.join("config/credentials.json"),
            )
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.root)
            .args(args);
        for (name, value) in vars {
            cmd.env(name, value);
        }
        cmd.output().unwrap()
    }

    fn run(&self, vars: &[(&str, &str)]) -> Output {
        self.hushrun(&["run", "unit", "--force", "--", "git", "--version"], vars)
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The bearer each request to record a result carried, and whether it was signed.
fn posts(fake: &FakeResults) -> Vec<(Option<String>, bool)> {
    fake.raw_requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path.contains("/v0/results/"))
        .map(|r| {
            (
                r.header("authorization").map(str::to_string),
                r.header("hushrun-signature").is_some(),
            )
        })
        .collect()
}

// covers: C-51
#[test]
fn with_proxy_auth_requests_carry_no_token_and_a_401_says_the_network_added_none() {
    let fake = FakeResults::start();
    fake.add_credentials_as_a_proxy();
    let r = Repo::new(&fake);
    let vars = [
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_AUTH", "proxy"),
        ("CLAUDE_CODE_REMOTE", "true"),
    ];
    let run = r.run(&vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(fake.recorded().len(), 1, "{}", stderr(&run));
    assert_eq!(posts(&fake), [(None, false)]);
    let whoami = r.hushrun(&["whoami", "--format", "json"], &vars);
    let json: Value = serde_json::from_slice(&whoami.stdout).unwrap();
    assert_eq!(json["credential"], "proxy", "{json}");

    // A network that adds nothing: the check runs, and the run says what is missing.
    let bare = FakeResults::start();
    let r = Repo::new(&bare);
    let run = r.run(&[
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_AUTH", "proxy"),
    ]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        stderr(&run).contains("this network added no credential for 127.0.0.1"),
        "{}",
        stderr(&run)
    );
    assert!(bare.recorded().is_empty());
}

// covers: C-51
#[test]
fn a_placeholder_goes_to_hushruns_host_only_in_a_hosted_sandbox_and_unsigned() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let origin = r.origin();
    // In Codex cloud's sandbox, the repository's backend on hushrun's host gets it.
    let run = r.run(&[
        ("HUSHRUN_HOST", origin.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::PLACEHOLDER),
        ("HUSHRUN_SANDBOX", "codex-cloud"),
        ("CI", "true"),
    ]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        posts(&fake),
        [(Some(format!("Bearer {}", FakeResults::PLACEHOLDER)), false)],
        "{}",
        stderr(&run)
    );

    // On a person's machine it is a token like any other, which a repository's file never gets.
    let other = FakeResults::start();
    let r = Repo::new(&other);
    let origin = r.origin();
    let run = r.run(&[
        ("HUSHRUN_HOST", origin.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::PLACEHOLDER),
    ]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        other.requests().iter().all(|seen| !seen
            .authorization
            .as_deref()
            .is_some_and(|a| a.contains(FakeResults::PLACEHOLDER))),
        "{:?}",
        other.requests()
    );
}

// covers: C-51
#[test]
fn a_bearer_agent_token_records_with_no_key_when_auth_says_so() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let run = r.run(&[
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::BEARER_TOKEN),
        ("HUSHRUN_AUTH", "bearer"),
    ]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        posts(&fake),
        [(Some(format!("Bearer {}", FakeResults::BEARER_TOKEN)), false)]
    );
    // Without it, an agent token wants its key, and nothing is sent.
    let before = posts(&fake).len();
    let run = r.run(&[
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::BEARER_TOKEN),
    ]);
    assert!(
        stderr(&run).contains("HUSHRUN_MACHINE_KEY"),
        "{}",
        stderr(&run)
    );
    assert_eq!(posts(&fake).len(), before);
}

// covers: C-52
#[test]
fn login_session_swaps_an_agent_token_and_runs_use_the_saved_session() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let login = r.hushrun(
        &["login", "--session", "--hours", "6", &r.backend],
        &[("HUSHRUN_TOKEN", FakeResults::BEARER_TOKEN)],
    );
    assert!(login.status.success(), "{}", stderr(&login));
    let said = String::from_utf8_lossy(&login.stdout).into_owned();
    assert!(
        said.contains("Session for github.com/acme/shop as agent:fleet-a"),
        "{said}"
    );
    assert!(
        !said.contains(FakeResults::SESSION_TOKEN),
        "a token is never printed"
    );
    let swapped = fake.sessions();
    assert_eq!(swapped.len(), 1);
    assert_eq!(swapped[0].0, FakeResults::BEARER_TOKEN);
    assert_eq!(swapped[0].1["hours"], 6);
    assert_eq!(swapped[0].1["repository"], "github.com/acme/shop");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(r.sessions())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // A run with nothing else reads it, and records unsigned.
    let run = r.run(&[]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        posts(&fake),
        [(
            Some(format!("Bearer {}", FakeResults::SESSION_TOKEN)),
            false
        )],
        "{}",
        stderr(&run)
    );
    let whoami = r.hushrun(&["whoami", "--format", "json"], &[]);
    let json: Value = serde_json::from_slice(&whoami.stdout).unwrap();
    assert_eq!(json["credential"], "session", "{json}");

    // Nothing to swap: it says what would do, and saves nothing new.
    let bare = FakeResults::start();
    let r = Repo::new(&bare);
    let login = r.hushrun(&["login", "--session", &r.backend], &[]);
    assert!(!login.status.success());
    assert!(
        stderr(&login).contains("nothing here to swap"),
        "{}",
        stderr(&login)
    );
    assert!(!r.sessions().exists());
}

/// A unix socket that answers Cursor's `POST /v1/tokens/oidc` with `token`, once per connection;
/// what each request asked.
#[cfg(unix)]
fn cursor_socket(dir: &Path, token: &'static str) -> (PathBuf, std::sync::mpsc::Receiver<String>) {
    use std::os::unix::net::UnixListener;
    let path = dir.join("cursor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            let body = format!(r#"{{"token":"{token}"}}"#);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (path, rx)
}

// covers: C-52
#[cfg(unix)]
#[test]
fn a_cursor_cloud_agent_gets_a_session_by_itself_and_keeps_it() {
    // exp in 2099: the cache keeps it.
    const ID_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQwNzA5MDg4MDB9.signature";
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let (socket, asked) = cursor_socket(&r.root, ID_TOKEN);
    let socket = socket.to_str().unwrap().to_string();
    let origin = r.origin();
    // The repository names a backend on hushrun's host.
    let vars = [
        ("CURSOR_AGENT_SOCKET", socket.as_str()),
        ("CI", "true"),
        ("HUSHRUN_HOST", origin.as_str()),
    ];
    let run = r.run(&vars);
    assert!(run.status.success(), "{}", stderr(&run));
    let request = asked.recv().unwrap();
    assert!(request.starts_with("POST /v1/tokens/oidc"), "{request}");
    assert!(
        request.contains(&format!(r#"{{"audience":"{}"}}"#, r.origin())),
        "{request}"
    );
    assert_eq!(fake.sessions()[0].0, ID_TOKEN);
    assert_eq!(
        posts(&fake),
        [(
            Some(format!("Bearer {}", FakeResults::SESSION_TOKEN)),
            false
        )],
        "{}",
        stderr(&run)
    );
    // The next run reads the saved session: no new token, no new session.
    let run = r.run(&vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(fake.sessions().len(), 1);
    assert!(asked.try_recv().is_err());
}

/// GitHub's ID token endpoint: answers `{"value": token}`; what each request asked.
fn github_endpoint(token: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/token?api-version=2.0",
        listener.local_addr().unwrap()
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            let body = format!(r#"{{"value":"{token}"}}"#);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, rx)
}

// covers: C-52
#[test]
fn copilots_setup_step_and_its_sandbox_swap_the_jobs_id_token_for_a_session() {
    const ID_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQwNzA5MDg4MDB9.github";
    let (url, asked) = github_endpoint(ID_TOKEN);
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    // In the agent's job, as hushrun knows it: the ID token is set aside for this only.
    let vars = [
        ("COPILOT_AGENT_SESSION_ID", "s-1"),
        ("GITHUB_ACTIONS", "true"),
        ("CI", "true"),
        ("ACTIONS_ID_TOKEN_REQUEST_URL", url.as_str()),
        ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request-token"),
    ];
    let login = r.hushrun(&["login", "--session", &r.backend], &vars);
    assert!(login.status.success(), "{}", stderr(&login));
    let request = asked.recv().unwrap();
    assert!(
        request.contains("authorization: bearer request-token")
            || request.contains("Authorization: bearer request-token")
            || request
                .to_ascii_lowercase()
                .contains("bearer request-token"),
        "{request}"
    );
    assert_eq!(fake.sessions()[0].0, ID_TOKEN);
    // Its run records with the session, never with the job's ID token.
    let run = r.run(&vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        posts(&fake),
        [(
            Some(format!("Bearer {}", FakeResults::SESSION_TOKEN)),
            false
        )],
        "{}",
        stderr(&run)
    );
}

// covers: C-53
#[test]
fn a_result_the_backend_couldnt_take_waits_skips_here_and_goes_with_the_next_run() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let vars = [
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::SESSION_TOKEN),
    ];
    let unit = ["run", "unit", "--", "git", "--version"];
    // The server fails the record: the result waits on this machine.
    fake.answer("POST", "v0/results", 503, serde_json::json!({}), 4);
    let run = r.hushrun(&unit, &vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        stderr(&run).contains("the result waits on this machine"),
        "{}",
        stderr(&run)
    );
    let outbox = r.root.join("cache/outbox");
    let entries: Vec<Value> = std::fs::read_dir(&outbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["state"], "pending");
    assert!(fake.recorded().is_empty());

    // The next run sends it first; with the server failing its lookup, this machine's pass skips.
    fake.answer("GET", "v0/results", 503, serde_json::json!({}), 4);
    let run = r.hushrun(&unit, &vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        stderr(&run).contains("sent 1 result that waited on this machine"),
        "{}",
        stderr(&run)
    );
    assert!(stderr(&run).contains("skipped"), "{}", stderr(&run));
    let passes = fake
        .recorded()
        .iter()
        .filter(|r| r["result"] == "pass")
        .count();
    assert_eq!(passes, 1, "{:?}", fake.recorded());
    // One file a key and server: `<key>-<server>.json`.
    let key = entries[0]["key"].as_str().unwrap();
    let file = std::fs::read_dir(&outbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{key}-"))
        })
        .unwrap();
    let entry: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(entry["state"], "sent");

    // Nothing waits now; and in CI nothing is kept at all.
    let flush = r.hushrun(&["flush", "--format", "json"], &vars);
    let json: Value = serde_json::from_slice(&flush.stdout).unwrap();
    assert_eq!(json["waiting"], 0, "{json}");
    assert_eq!(json["sent"], 0, "{json}");
    fake.answer("POST", "v0/results", 503, serde_json::json!({}), 10);
    let ci = r.hushrun(
        &["run", "unit", "--force", "--", "git", "--version"],
        &[
            ("HUSHRUN_BACKEND", r.backend.as_str()),
            ("HUSHRUN_TOKEN", FakeResults::TOKEN),
            ("CI", "true"),
        ],
    );
    assert!(
        !stderr(&ci).contains("waits on this machine"),
        "{}",
        stderr(&ci)
    );
}

// covers: C-13, C-52
#[cfg(unix)]
#[test]
fn a_backend_a_repositorys_file_chose_gets_no_sandbox_credential_for_a_session() {
    const ID_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQwNzA5MDg4MDB9.signature";
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    let (socket, asked) = cursor_socket(&r.root, ID_TOKEN);
    let socket = socket.to_str().unwrap().to_string();
    // package.json names a server that isn't hushrun's host, nor named by the environment.
    let vars = [
        ("CURSOR_AGENT_SOCKET", socket.as_str()),
        ("CI", "true"),
        ("HUSHRUN_TOKEN", FakeResults::BEARER_TOKEN),
    ];
    let run = r.run(&vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(fake.sessions().is_empty(), "{:?}", fake.sessions());
    assert!(asked.try_recv().is_err(), "Cursor's socket wasn't asked");
    assert!(
        fake.requests().iter().all(|seen| !seen
            .authorization
            .as_deref()
            .is_some_and(|a| a.contains(FakeResults::BEARER_TOKEN) || a.contains(ID_TOKEN))),
        "{:?}",
        fake.requests()
    );
    // Nor does login --session take it there unless someone names it.
    let login = r.hushrun(&["login", "--session"], &vars);
    assert_eq!(login.status.code(), Some(2), "{}", stderr(&login));
    assert!(
        stderr(&login).contains("isn't on hushrun's host"),
        "{}",
        stderr(&login)
    );
    assert!(fake.sessions().is_empty());
    // HUSHRUN_AGENT, which holds a key, is never swapped.
    let login = r.hushrun(
        &["login", "--session", &r.backend],
        &[("HUSHRUN_AGENT", "hushrun_agent_v1_not-a-real-string")],
    );
    assert!(!login.status.success());
    assert!(fake.sessions().is_empty());
}

// covers: C-54
#[test]
fn what_fetch_kept_skips_a_check_once_the_backend_is_out_of_reach() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake);
    std::fs::write(
        r.repo.join("package.json"),
        format!(
            r#"{{"name": "shop", "scripts": {{"test": "hushrun run unit -- git --version"}}, "hushrun": {{"backend": "{}"}}}}"#,
            r.backend
        ),
    )
    .unwrap();
    git::commit_all(&r.repo, "scripts");
    let vars = [
        ("HUSHRUN_BACKEND", r.backend.as_str()),
        ("HUSHRUN_TOKEN", FakeResults::SESSION_TOKEN),
    ];
    // Another machine passed it.
    let other = r.root.join("other-cache");
    let unit = ["run", "unit", "--", "git", "--version"];
    let mut with_other: Vec<(&str, &str)> = vars.to_vec();
    with_other.push(("HUSHRUN_CACHE_DIR", other.to_str().unwrap()));
    let run = r.hushrun(&unit, &with_other);
    assert!(run.status.success(), "{}", stderr(&run));
    // Setup fetches, then the network goes.
    let fetch = r.hushrun(&["fetch"], &vars);
    assert!(fetch.status.success(), "{}", stderr(&fetch));
    assert!(
        stderr(&fetch).contains("1 of 1 checks have a newest result"),
        "{}",
        stderr(&fetch)
    );
    fake.answer("GET", "v0/results", 503, serde_json::json!({}), 4);
    let run = r.hushrun(&unit, &vars);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(stderr(&run).contains("skipped"), "{}", stderr(&run));
    // `build fetch` still works as it did.
    let build = r.hushrun(&["build", "fetch", "--help"], &vars);
    assert!(build.status.success());
}

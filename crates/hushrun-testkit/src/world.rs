//! A repository, a fake HTTP ledger and deterministic keys, for end-to-end CLI tests.

use std::path::{Path, PathBuf};

use base64::Engine as _;

use hushrun_attest::SigningKey;

use crate::fake_ledger::FakeLedger;
use crate::git;

/// The key whose seed is 32 bytes of `n`. \[ADR 0031\] A [`World`]'s server names keys
/// [`MEMBERS`] as members' machines (T-15); fixtures may write the placeholder
/// `ed25519:<base64 of 32 bytes of n>`, which [`with_test_keys`] replaces with this key's public
/// key.
pub fn test_key(n: u8) -> SigningKey {
    SigningKey::from_seed(&[n; 32])
}

fn placeholder(n: u8) -> String {
    format!(
        "ed25519:{}",
        base64::engine::general_purpose::STANDARD.encode([n; 32])
    )
}

/// \[ADR 0031\] The test keys a world's server names as machines of members of the org (T-15):
/// `user:key<n>@acme.dev`. Key 1's owner is an admin, whose machine may sign GC statements.
/// Others sign as strangers until a test registers them.
pub const MEMBERS: [u8; 3] = [1, 2, 4];

/// The owner a world's server names for test key `n`'s machine.
pub fn member(n: u8) -> String {
    format!("user:key{n}@acme.dev")
}

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_LOG",
    "HUSHRUN_GIT_READER",
    "HUSHRUN_REPOSITORY_URL",
    "HUSHRUN_REPOSITORY_BASE",
    "HUSHRUN_SIGNING_KEY",
    "HUSHRUN_SIGNING_KEY_FILE",
    "HUSHRUN_CUSTODY",
    "HUSHRUN_FORCE_FULL",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_RUN_ID",
    "HUSHRUN_RUN_ATTEMPT",
    "HUSHRUN_RUN_URL",
    "HUSHRUN_PLAN_JSON",
    "HUSHRUN_NO_CI_OUTPUT",
    "HUSHRUN_REPOSITORY_TOKEN",
    "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
    "HUSHRUN_ORG_TOKEN",
    "HUSHRUN_ORG_TOKEN_COMMAND",
    // M10's signers, timestamp authorities and trust roots.
    "HUSHRUN_SIGNING",
    "HUSHRUN_FULCIO_URL",
    "HUSHRUN_TSA",
    "HUSHRUN_SIGSTORE_TRUSTED_ROOT",
    "HUSHRUN_SIGSTORE_TUF_URL",
    "HUSHRUN_SIGSTORE_TUF_ROOT",
    "SIGSTORE_ID_TOKEN",
    // M8's container runtime, exports, sign-in and invoker: a test never opens a browser or
    // runs a developer's containers.
    "BROWSER",
    "HUSHRUN_CONTAINER_RUNTIME",
    "HUSHRUN_RESTRICTED_NETWORK",
    "HUSHRUN_EXPORT_DIR",
    "HUSHRUN_OIDC_ISSUER",
    "HUSHRUN_OIDC_CLIENT_ID",
    "HUSHRUN_INVOKED_BY",
    // `hushrun run`'s backend and its credentials (C-11, C-13): doctor asks the backend who it
    // is, so a test never reaches a developer's own.
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_HOST",
    "HUSHRUN_CREDENTIALS",
    // ADR 0046: nor with what a sandbox's one variable holds.
    "HUSHRUN_AGENT",
    // ADR 0028: a test never signs with, or registers, the developer's machine key.
    "HUSHRUN_MACHINE_KEY",
    "HUSHRUN_MACHINE_KEY_FILE",
    "CI_PIPELINE_SOURCE",
    "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
    "GITHUB_BASE_REF",
    // A test on GitHub Actions must never request the runner's ID tokens.
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    // A test running on GitHub Actions must not write to the real runner or use its token.
    "GITHUB_API_URL",
    "GITHUB_EVENT_NAME",
    "GITHUB_EVENT_PATH",
    "GITHUB_JOB",
    "GITHUB_OUTPUT",
    "GITHUB_REF",
    "GITHUB_REPOSITORY",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_RUN_ID",
    "GITHUB_SERVER_URL",
    "GITHUB_SHA",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    // A test running on GitLab CI must not take the real pipeline's identity or use its token.
    "CI_API_V4_URL",
    "CI_COMMIT_BRANCH",
    "CI_COMMIT_SHA",
    "CI_COMMIT_TAG",
    "CI_JOB_ID",
    "CI_JOB_NAME",
    "CI_PIPELINE_ID",
    "CI_PIPELINE_SOURCE",
    "CI_PIPELINE_URL",
    "CI_PROJECT_ID",
    "CI_PROJECT_PATH",
    "CI_PROJECT_URL",
    "HUSHRUN_GITLAB_TOKEN",
    // The repository's id is the provider's word, never a developer's environment.
    "GITHUB_REPOSITORY_ID",
    "CI_DEFAULT_BRANCH",
];

/// A git repository with a policy, a fake HTTP server (L-10), and test keys. \[ADR 0031\] The
/// server names test keys [`MEMBERS`] as members' machines (T-15), and hushrun.yaml pins its log
/// key (`transparency_log`, T-9). The repository has a remote, `origin`, a bare repository whose
/// default branch, `main`, holds the first commit; and the CI providers' URLs for the repository
/// (`https://github.com/<owner>/<name>`, `https://gitlab.com/…`) lead there, so that no test
/// ever fetches from the network.
pub struct World {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub repo: PathBuf,
    pub cache: PathBuf,
    /// The world's server, on the loopback.
    pub fake: FakeLedger,
    /// The `transparency_log` lines hushrun.yaml gets.
    log_lines: String,
}

impl World {
    /// A repository on `develop` with `testdata/policies/<policy>` as hushrun.yaml and one
    /// source file, and a fake HTTP server (L-10) on the loopback. hushrun.yaml names the
    /// server's log key (`transparency_log`, T-9), so the CLI checks its transparency log
    /// (L-11). `hushrun()` names the server in `HUSHRUN_REPOSITORY_URL` and authenticates with
    /// its admin token.
    pub fn new(policy: &str) -> World {
        World::build(policy, FakeLedger::start(), true)
    }

    /// A world whose hushrun.yaml doesn't name the server's log key: T-9's warning.
    pub fn without_log_key(policy: &str) -> World {
        World::build(policy, FakeLedger::start(), false)
    }

    /// Pushes `HEAD` to `origin`'s `main`, the default branch: as merging a pull request does.
    pub fn push_default_branch(&self) {
        git::run(
            &self.repo,
            &["push", "-q", "--force", "origin", "HEAD:refs/heads/main"],
            None,
        );
    }

    /// The bare repository that is the world's `origin`.
    pub fn origin(&self) -> PathBuf {
        self.root.join("origin.git")
    }

    /// Makes `url` lead to `origin` too, for a CI run whose provider names another repository.
    pub fn serve_origin_as(&self, url: &str) {
        let origin = self.origin();
        git::run(
            &self.repo,
            &[
                "config",
                "--add",
                &format!("url.{}.insteadOf", origin.display()),
                url,
            ],
            None,
        );
    }

    fn build(policy: &str, fake: FakeLedger, log_key: bool) -> World {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        let cache = root.join("cache");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        git::run(&repo, &["checkout", "-q", "-b", "develop"], None);
        let mut log_lines = String::new();
        if log_key {
            let (origin, key) = fake.log_key();
            log_lines.push_str(&format!(
                "transparency_log:\n  origin: {origin}\n  key: \"{key}\"\n"
            ));
        }
        for n in MEMBERS {
            fake.register_machine(&test_key(n).public_key(), &member(n), n == 1);
        }
        let world = World {
            _dir: dir,
            root,
            repo,
            cache,
            fake,
            log_lines,
        };
        let policy = std::fs::read_to_string(crate::testdata(&format!("policies/{policy}")))
            .expect("policy fixture");
        let policy = rewrite_ledger(&policy, &world.ledger_url());
        world.write("hushrun.yaml", &policy);
        // A file for every scope the fixtures declare.
        world.write("main.go", "package main\n");
        world.write("go.mod", "module example.com/app\n");
        world.write("services/api/main.go", "package main\n");
        world.write("web/index.ts", "export {}\n");
        git::commit_all(&world.repo, "init");
        let repository = policy
            .lines()
            .find_map(|l| l.strip_prefix("repository: "))
            .map(|r| r.trim().to_string());
        world.add_origin(repository.as_deref());
        world
    }

    /// `origin`, a bare repository whose default branch, `main`, starts at the first commit. The
    /// repository's URL on its provider leads to `origin`, and every other URL on those hosts to
    /// nowhere, so that a test never reaches the network.
    fn add_origin(&self, repository: Option<&str>) {
        let origin = self.origin();
        let origin_text = origin.to_str().expect("UTF-8 path").to_string();
        git::run(
            &self.root,
            &["init", "-q", "--bare", "-b", "main", &origin_text],
            None,
        );
        git::run(&self.repo, &["remote", "add", "origin", &origin_text], None);
        self.push_default_branch();
        let nowhere = format!("{}/", self.root.join("nowhere").display());
        for host in ["https://github.com/", "https://gitlab.com/"] {
            git::run(
                &self.repo,
                &["config", "--add", &format!("url.{nowhere}.insteadOf"), host],
                None,
            );
        }
        if let Some((host, path)) = repository.and_then(|r| r.split_once('/')) {
            git::run(
                &self.repo,
                &[
                    "config",
                    "--add",
                    &format!("url.{origin_text}.insteadOf"),
                    &format!("https://{host}/{path}"),
                ],
                None,
            );
        }
    }

    /// The ledger's URL: `http://127.0.0.1:…`.
    pub fn ledger_url(&self) -> String {
        self.fake.url()
    }

    /// hushrun.yaml's line that names the server (C-3), for tests that write their own
    /// hushrun.yaml.
    pub fn ledger_lines(&self) -> String {
        format!("repository_url: {}", self.fake.url())
    }

    /// The `transparency_log` lines hushrun.yaml has, for tests that write their own.
    pub fn log_lines(&self) -> String {
        self.log_lines.clone()
    }

    /// An object in the ledger.
    pub fn object(&self, key: &str) -> Vec<u8> {
        self.fake
            .object(key)
            .unwrap_or_else(|| panic!("{key} isn't in the ledger"))
    }

    /// Stores an object in the ledger directly, as if a writer had.
    pub fn put_object(&self, key: &str, bytes: &[u8]) {
        self.fake.put_object(key, bytes);
    }

    /// Writes the ledger's object `key` to a file under the world's root, for commands that
    /// read files, such as `hushrun verify`, and returns its path.
    pub fn export(&self, key: &str) -> PathBuf {
        let path = self.root.join("objects").join(key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, self.object(key)).expect("write");
        path
    }

    /// Writes `path` in the repository. \[ADR 0031\] A hushrun.yaml gets the world's
    /// `transparency_log` lines unless it names its own, and the server answers for the
    /// repository it names, as a server answers for the repository it was made for.
    pub fn write(&self, path: &str, content: &str) {
        let mut content = content.to_string();
        if path == "hushrun.yaml" {
            if let Some(repository) = content.lines().find_map(|l| l.strip_prefix("repository: ")) {
                self.fake.set_repository(repository.trim());
            }
            if !self.log_lines.is_empty()
                && !content.lines().any(|l| l.starts_with("transparency_log:"))
            {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push('\n');
                content.push_str(&self.log_lines);
            }
        }
        let content = content.as_str();
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    pub fn commit(&self, message: &str) -> String {
        git::commit_all(&self.repo, message)
    }

    pub fn head(&self) -> String {
        git::text(&self.repo, &["rev-parse", "HEAD"])
    }

    /// `hushrun` in the repository, as a local run, with no inherited configuration. Events
    /// go out without timestamps (`HUSHRUN_TSA=none`): tests never reach Sigstore.
    pub fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            // An empty Docker configuration: tests never run the user's credential helpers.
            .env("DOCKER_CONFIG", self.root.join("docker"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.env("HUSHRUN_TSA", "none");
        // No login: results stay in the cache directory.
        cmd.env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"));
        // A sign-in never opens a browser in a test: `true` is the browser (M8).
        cmd.env("BROWSER", "true");
        // Tokens go only to a ledger named outside the repository (T-4).
        cmd.env("HUSHRUN_REPOSITORY_URL", self.fake.url())
            .env("HUSHRUN_REPOSITORY_TOKEN", FakeLedger::ADMIN);
        cmd
    }

    /// `hushrun` signing with test key `n`.
    pub fn hushrun_as(&self, n: u8) -> assert_cmd::Command {
        let mut cmd = self.hushrun();
        cmd.env("HUSHRUN_SIGNING_KEY", test_key(n).to_pem().as_str());
        cmd
    }

    /// Every key under `prefix` in the ledger, sorted.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .fake
            .keys()
            .into_iter()
            .filter(|k| k.starts_with(prefix))
            .collect();
        keys.sort();
        keys
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

/// Replaces the placeholder keys `ed25519:<base64 of 32 bytes of n>` with test key `n`.
pub fn with_test_keys(text: &str) -> String {
    let mut out = text.to_string();
    for n in 1..=9 {
        out = out.replace(&placeholder(n), &test_key(n).public_key().to_string());
    }
    out
}

fn rewrite_ledger(policy: &str, url: &str) -> String {
    policy
        .lines()
        .map(|line| {
            if line.starts_with("repository_url:") {
                format!("repository_url: {url}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Reads a file under `root`.
pub fn read(root: &Path, relative: &str) -> Vec<u8> {
    std::fs::read(root.join(relative)).expect("read")
}

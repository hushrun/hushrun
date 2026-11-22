//! A fake backend of the build protocol on the loopback, for scripts that measure a CI job's
//! build and bake without a real backend, such as the ones behind the measurements in hushrun's
//! ADRs: a repository's trusted scope is `protected/github.com/<owner>/<name>`, and a GitHub
//! Actions job on its default branch writes there with the fake's ID token.
//!
//! `cargo run -p hushrun-testkit --example fake_build -- DIR [OWNER/NAME]` writes `DIR/env`, the
//! variables of a GitHub Actions job of that repository (`acme/shop` by default) on `main` that
//! shares through it (`KEY='value'` lines, for `.`; the job's event file is `DIR/event.json`), and
//! serves until it is killed. Blocks are kept in memory.

use std::path::PathBuf;
use std::time::Duration;

use hushrun_testkit::fake_build::FakeBuild;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: fake_build DIR (writes DIR/env and DIR/event.json)"),
    );
    let repository = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "acme/shop".to_string());
    std::fs::create_dir_all(&dir).expect("create DIR");
    let fake = FakeBuild::start();
    let scope = format!("protected/github.com/{repository}");
    fake.set_scopes(Some(&scope), &[scope.as_str()]);
    let event = dir.join("event.json");
    std::fs::write(&event, r#"{"repository": {"default_branch": "main"}}"#)
        .expect("write DIR/event.json");
    let vars = [
        ("CI", "true".to_string()),
        ("GITHUB_ACTIONS", "true".to_string()),
        ("GITHUB_REPOSITORY", repository.clone()),
        ("GITHUB_REF", "refs/heads/main".to_string()),
        ("GITHUB_EVENT_NAME", "push".to_string()),
        ("GITHUB_EVENT_PATH", event.display().to_string()),
        ("ACTIONS_ID_TOKEN_REQUEST_URL", fake.id_token_url()),
        (
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            FakeBuild::ID_TOKEN_REQUEST.to_string(),
        ),
        ("HUSHRUN_BACKEND", fake.url()),
        ("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN.to_string()),
    ];
    let env: String = vars
        .iter()
        .map(|(key, value)| format!("{key}='{value}'\nexport {key}\n"))
        .collect();
    std::fs::write(dir.join("env"), env).expect("write DIR/env");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

//! A fake Sigstore on the loopback (GitHub Actions' ID token endpoint, Fulcio and a timestamp
//! authority), for scripts that record what the CLI prints when CI signs keylessly, without the
//! network, such as the ones behind the samples in hushrun's docs.
//!
//! `cargo run -p hushrun-testkit --example fake_sigstore -- DIR` writes `DIR/trusted_root.json`
//! and `DIR/env`, the variables of a GitHub Actions job that signs with it (`KEY='value'` lines,
//! for `source`), and serves until it is killed. The ID tokens are a workflow's of a repository:
//! write `<owner/name> <repository id> <ref> <workflow path>` to `DIR/claims`, and wait until
//! `DIR/claims.applied` has the same text, before the job that signs as that workflow.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hushrun_attest::testing::github_claims;
use hushrun_testkit::sigstore::FakeSigstore;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: fake_sigstore DIR (writes DIR/env and DIR/trusted_root.json)"),
    );
    std::fs::create_dir_all(&dir).expect("create DIR");
    let fake = FakeSigstore::start();
    let env: String = fake
        .env(&dir)
        .into_iter()
        .map(|(key, value)| format!("{key}='{value}'\n"))
        .collect();
    std::fs::write(dir.join("env"), env).expect("write DIR/env");
    let mut applied = String::new();
    loop {
        if let Some(text) = read(&dir.join("claims"))
            && text != applied
        {
            let fields: Vec<&str> = text.split_whitespace().collect();
            if let [repository, id, reference, workflow] = fields[..] {
                fake.set_claims(github_claims(repository, id, reference, workflow, None));
                std::fs::write(dir.join("claims.applied"), &text).expect("write claims.applied");
            } else {
                eprintln!(
                    "DIR/claims: expected `<owner/name> <id> <ref> <workflow>`, got {text:?}"
                );
            }
            applied = text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

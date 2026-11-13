//! A fake `gh` and `glab`, for commands that change a repository through its host's CLI
//! (`<cli> api`): pull requests that change `hushrun.yaml` (crate `hushrun`'s `forge`).
//!
//! Each call is logged as `<cli> METHOD endpoint` in `calls.log`, its JSON body kept as
//! `bodies/<key>`, and answered from `responses/<key>`, where the key is `METHOD endpoint` with
//! every character but letters, digits, `.`, `_` and `-` made `_`. A call without a response
//! answers 404, as the hosts do. Unix only: the fakes are shell scripts.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use serde_json::{Value, json};

#[cfg(unix)]
const SCRIPT: &str = r#"#!/bin/sh
dir=$(dirname "$0")
[ "$1" = api ] || { echo "fake $(basename "$0"): unexpected $*" >&2; exit 1; }
shift
method=GET; endpoint=; input=
while [ $# -gt 0 ]; do
  case "$1" in
    --method) method=$2; shift 2 ;;
    --hostname|--header) shift 2 ;;
    --input) input=1; shift 2 ;;
    *) endpoint=$1; shift ;;
  esac
done
printf '%s %s %s\n' "$(basename "$0")" "$method" "$endpoint" >> "$dir/calls.log"
key=$(printf '%s %s' "$method" "$endpoint" | sed 's/[^A-Za-z0-9._-]/_/g')
[ -n "$input" ] && cat > "$dir/bodies/$key"
if [ -f "$dir/responses/$key" ]; then cat "$dir/responses/$key"; exit 0; fi
echo '{"message":"Not Found"}'
echo "$(basename "$0"): Not Found (HTTP 404)" >&2
exit 1
"#;

/// `gh` and `glab` in a directory of their own.
pub struct FakeForge {
    pub bin: PathBuf,
}

impl FakeForge {
    #[cfg(unix)]
    pub fn new(bin: &Path) -> FakeForge {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(bin.join("responses")).unwrap();
        std::fs::create_dir_all(bin.join("bodies")).unwrap();
        for name in ["gh", "glab"] {
            let path = bin.join(name);
            std::fs::write(&path, SCRIPT).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        FakeForge {
            bin: bin.to_path_buf(),
        }
    }

    fn key(method: &str, endpoint: &str) -> String {
        format!("{method} {endpoint}")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Answers `METHOD endpoint` with `body` from now on.
    pub fn respond(&self, method: &str, endpoint: &str, body: &Value) {
        std::fs::write(
            self.bin.join("responses").join(Self::key(method, endpoint)),
            body.to_string(),
        )
        .unwrap();
    }

    /// Answers `METHOD endpoint` with 404 from now on.
    pub fn forget(&self, method: &str, endpoint: &str) {
        let _ = std::fs::remove_file(self.bin.join("responses").join(Self::key(method, endpoint)));
    }

    /// The JSON body last sent with `METHOD endpoint`.
    pub fn body(&self, method: &str, endpoint: &str) -> Value {
        let path = self.bin.join("bodies").join(Self::key(method, endpoint));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The calls since the last time, which it forgets.
    pub fn calls(&self) -> Vec<String> {
        let path = self.bin.join("calls.log");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        text.lines().map(str::to_string).collect()
    }

    /// A GitHub repository whose default branch is main, holding `hushrun_yaml` if any, with
    /// nothing open from `branch`, and every write hushrun makes answered.
    pub fn github_repository(&self, path: &str, hushrun_yaml: Option<&str>, branch: &str) {
        self.respond(
            "GET",
            &format!("repos/{path}"),
            &json!({"default_branch": "main"}),
        );
        let contents = format!("repos/{path}/contents/hushrun.yaml?ref=main");
        match hushrun_yaml {
            Some(text) => self.respond(
                "GET",
                &contents,
                &json!({"content": b64(text), "sha": "blob-main"}),
            ),
            None => self.forget("GET", &contents),
        }
        let owner = path.split('/').next().unwrap();
        self.respond(
            "GET",
            &format!(
                "repos/{path}/pulls?state=open&head={owner}%3A{}",
                branch.replace('/', "%2F")
            ),
            &json!([]),
        );
        self.respond(
            "GET",
            &format!("repos/{path}/git/ref/heads/main"),
            &json!({"object": {"sha": "tip-of-main"}}),
        );
        self.respond("POST", &format!("repos/{path}/git/refs"), &json!({}));
        self.respond(
            "PATCH",
            &format!("repos/{path}/git/refs/heads/{branch}"),
            &json!({}),
        );
        self.respond(
            "PUT",
            &format!("repos/{path}/contents/hushrun.yaml"),
            &json!({}),
        );
    }

    /// `cmd` with these fakes first on PATH.
    pub fn on_path(&self, cmd: &mut assert_cmd::Command) {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", self.bin.display()));
    }
}

pub fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

pub fn unb64(text: &str) -> String {
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap(),
    )
    .unwrap()
}

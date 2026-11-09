//! The token command (§11.3, L-10): `HUSHRUN_REPOSITORY_TOKEN_COMMAND` prints a bearer token for
//! an `https://` ledger, run with `HUSHRUN_REPOSITORY_URL` set to the ledger, for tokens no variable
//! holds, such as one per ledger.

use std::process::Command;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::runtime::excerpt;

/// How long a printed token is reused. Commands don't say when their tokens expire; gcloud's
/// last an hour.
const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
pub(crate) struct TokenCommand {
    command: String,
    /// \[M19\] The ledger whose token it prints, in `HUSHRUN_REPOSITORY_URL` (L-10).
    ledger: Option<String>,
    cache: Mutex<Option<(String, Instant)>>,
}

impl TokenCommand {
    pub(crate) fn new(command: &str) -> Self {
        TokenCommand {
            command: command.to_string(),
            ledger: None,
            cache: Mutex::new(None),
        }
    }

    /// \[M19\] Runs the command with `HUSHRUN_REPOSITORY_URL` set to `ledger`, so that one command
    /// can print each ledger's token (L-10).
    pub(crate) fn for_ledger(mut self, ledger: &str) -> Self {
        self.ledger = Some(ledger.to_string());
        self
    }

    pub(crate) async fn token(&self) -> Result<String, TokenError> {
        let mut cache = self.cache.lock().await;
        if let Some((token, at)) = cache.as_ref()
            && at.elapsed() < TOKEN_TTL
        {
            return Ok(token.clone());
        }
        let (command, ledger) = (self.command.clone(), self.ledger.clone());
        let token = tokio::task::spawn_blocking(move || run(&command, ledger.as_deref()))
            .await
            .map_err(|e| TokenError(format!("the token command didn't finish: {e}")))??;
        *cache = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

/// Runs `command` through the shell and returns what it printed, trimmed. Errors never
/// include stdout, which may hold a token.
fn run(command: &str, ledger: Option<&str>) -> Result<String, TokenError> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    if let Some(ledger) = ledger {
        cmd.env("HUSHRUN_REPOSITORY_URL", ledger);
    }
    let output = cmd.output().map_err(|e| {
        TokenError(format!(
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND couldn't start: {e}"
        ))
    })?;
    if !output.status.success() {
        return Err(TokenError(format!(
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND failed ({}): {}",
            output.status,
            excerpt(&String::from_utf8_lossy(&output.stderr), 300)
        )));
    }
    let token = String::from_utf8(output.stdout)
        .map_err(|_| {
            TokenError("HUSHRUN_REPOSITORY_TOKEN_COMMAND printed non-UTF-8 output".into())
        })?
        .trim()
        .to_string();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(TokenError(
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND must print exactly one token".into(),
        ));
    }
    Ok(token)
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct TokenError(pub(crate) String);

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn prints_a_token_and_caches_it() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let command = format!("echo x >> '{}'; echo ' hrl_token '", count.display());
        let provider = TokenCommand::new(&command);
        let rt = runtime();
        for _ in 0..3 {
            assert_eq!(rt.block_on(provider.token()).unwrap(), "hrl_token");
        }
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
    }

    #[test]
    fn failures_show_stderr_never_stdout() {
        let rt = runtime();
        let err = rt
            .block_on(TokenCommand::new("echo secret-token; echo denied >&2; exit 3").token())
            .unwrap_err();
        assert!(err.0.contains("denied"), "{err}");
        assert!(!err.0.contains("secret-token"), "{err}");
        let err = rt.block_on(TokenCommand::new("true").token()).unwrap_err();
        assert!(err.0.contains("exactly one token"), "{err}");
        let err = rt
            .block_on(TokenCommand::new("echo two tokens").token())
            .unwrap_err();
        assert!(err.0.contains("exactly one token"), "{err}");
    }

    // covers: L-10
    #[test]
    fn it_knows_which_ledger_it_prints_a_token_for() {
        let rt = runtime();
        let command = TokenCommand::new("echo \"token-for-$HUSHRUN_REPOSITORY_URL\"")
            .for_ledger("https://hushrun.example/l/acme/back");
        assert_eq!(
            rt.block_on(command.token()).unwrap(),
            "token-for-https://hushrun.example/l/acme/back"
        );
    }
}

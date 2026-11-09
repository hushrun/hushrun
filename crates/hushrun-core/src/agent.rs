//! \[ADR 0046\] `HUSHRUN_AGENT`: all a sandbox needs, in one string safe in a shell and in a
//! secrets form. It holds the backend's URL, the agent token and the agent's private key:
//! `hushrun_agent_v1_`, then the unpadded base64url (RFC 4648 §5) of the JSON object
//! `{"backend", "key", "token"}`, so only `[A-Za-z0-9_-]`, with no newline.
//!
//! It holds a private key, so nothing here may carry any of it into a message: every error is a
//! fixed sentence with no value in it, and `Debug` shows the backend and the token's prefix only.
//! Neither `serde_json`'s errors nor a URL's are passed on, since both quote what they read.
//!
//! [`is_agent_string`] is the one check, anywhere in a value: a string goes astray in quotes, after
//! `Bearer `, behind its variable's name or without its prefix as easily as alone. It looks for a
//! mark with enough base64url after it to be a string, so a file named `hushrun_agent_notes.txt`, a
//! test or a `grep` for the prefix is none of its business. It stops mistakes. Someone who means to
//! get past it, with uppercase, more base64 or percent-encoding, gets past it.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::json;

use crate::ledger_url::LedgerUrl;

/// The variable that holds it.
pub const VARIABLE: &str = "HUSHRUN_AGENT";
/// How an agent token starts (ADR 0027).
pub const TOKEN_PREFIX: &str = "hra_";
/// How every form of the string starts, before its version.
const MARK: &str = "hushrun_agent_v";
/// How the encoded part starts: the base64url of `{"backend":"`, what's left when a string loses
/// its prefix.
const BODY_MARK: &str = "eyJiYWNrZW5kIjoi";
/// The base64url that must follow a mark for it to be a string. A real one has hundreds of
/// characters, and these 36 bytes decode to no further than the backend's URL: less holds no
/// secret, so this much can't miss one, however it was cut.
const MIN_BODY: usize = 48;
/// What a message says for a value that holds a string.
pub const NOT_SHOWN: &str = "<a HUSHRUN_AGENT string>";
/// The form this hushrun reads and writes.
const PREFIX: &str = "hushrun_agent_v1_";
const BEGIN_KEY: &str = "-----BEGIN PRIVATE KEY-----";
const END_KEY: &str = "-----END PRIVATE KEY-----";

/// What's wrong with a string. Each variant is a fixed sentence: none carries a value, so none
/// can print what the string holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentStringError {
    #[error(
        "HUSHRUN_AGENT holds an agent token alone, not the whole string: `hushrun token create --agent` shows the string at a terminal, and --format json gives it as hushrun_agent"
    )]
    JustAToken,
    #[error(
        "HUSHRUN_AGENT holds a private key alone, not the whole string: `hushrun token create --agent` shows the string at a terminal, and --format json gives it as hushrun_agent"
    )]
    JustAKey,
    #[error(
        "HUSHRUN_AGENT has something around the string, such as its own name: set the string alone"
    )]
    Wrapped,
    #[error(
        "HUSHRUN_AGENT doesn't start with hushrun_agent_v1_, as the string `hushrun token create --agent` shows does"
    )]
    NotAnAgentString,
    #[error("HUSHRUN_AGENT comes from a newer hushrun than this one: update hushrun")]
    Newer,
    #[error(
        "HUSHRUN_AGENT is damaged: part of it is missing, or something was added, such as a line break"
    )]
    Damaged,
    #[error("HUSHRUN_AGENT's backend isn't an https:// URL")]
    Backend,
    #[error("HUSHRUN_AGENT's token isn't an agent token (hra_…)")]
    Token,
    #[error("HUSHRUN_AGENT's key isn't a private key in PEM form")]
    Key,
}

/// A string, read: what it stands for in place of `HUSHRUN_BACKEND`, `HUSHRUN_TOKEN` and
/// `HUSHRUN_MACHINE_KEY`.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentString {
    backend: LedgerUrl,
    token: String,
    key: String,
}

/// The JSON a string encodes. Readers ignore fields they don't know.
#[derive(Deserialize)]
struct Payload {
    backend: String,
    key: String,
    token: String,
}

impl AgentString {
    /// The string for `backend`, `token` and `key`, a PEM block.
    pub fn encode(backend: &str, token: &str, key: &str) -> String {
        let payload = json!({"backend": backend, "key": key, "token": token});
        format!(
            "{PREFIX}{}",
            URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes())
        )
    }

    /// Reads `text`, around which spaces, line breaks and quotes don't count, as a paste,
    /// `$(cat file)` or `docker --env-file` leaves them. The key's form is checked here, and the
    /// key itself where it first signs.
    pub fn parse(text: &str) -> Result<AgentString, AgentStringError> {
        let text = text.trim().trim_matches(['"', '\'']);
        let Some(encoded) = text.strip_prefix(PREFIX) else {
            return Err(unreadable(text));
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| AgentStringError::Damaged)?;
        let payload: Payload =
            serde_json::from_slice(&bytes).map_err(|_| AgentStringError::Damaged)?;
        let backend =
            LedgerUrl::parse(payload.backend.trim()).map_err(|_| AgentStringError::Backend)?;
        if !is_token(&payload.token) {
            return Err(AgentStringError::Token);
        }
        let key = payload.key.trim();
        if !(key.starts_with(BEGIN_KEY) && key.ends_with(END_KEY)) {
            return Err(AgentStringError::Key);
        }
        Ok(AgentString {
            backend,
            token: payload.token,
            key: payload.key,
        })
    }

    /// The backend it records to, the only one its token goes to.
    pub fn backend(&self) -> &LedgerUrl {
        &self.backend
    }

    /// The agent token.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The agent's private key, a PEM block.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Debug for AgentString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix: String = self.token.chars().take(10).collect();
        f.debug_struct("AgentString")
            .field("backend", &self.backend.to_string())
            .field("token", &format_args!("{prefix}…"))
            .field("key", &format_args!("…"))
            .finish()
    }
}

/// Whether `text` holds a string of any version, with or without its prefix, anywhere in it: a
/// thing that must never leave as a token, or appear in a message.
pub fn is_agent_string(text: &str) -> bool {
    let after = |mark: &'static str| {
        text.match_indices(mark)
            .map(move |(at, _)| &text[at + mark.len()..])
    };
    after(MARK).any(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0
            && rest[digits..]
                .strip_prefix('_')
                .is_some_and(|body| base64url_run(body) >= MIN_BODY)
    }) || after(BODY_MARK).any(|rest| BODY_MARK.len() + base64url_run(rest) >= MIN_BODY)
}

/// How many characters of base64url `text` starts with.
fn base64url_run(text: &str) -> usize {
    text.bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        .count()
}

/// `text`, for a message: [`NOT_SHOWN`] when it holds a string.
pub fn masked(text: &str) -> &str {
    if is_agent_string(text) {
        NOT_SHOWN
    } else {
        text
    }
}

/// Why `text`, without the prefix this hushrun reads, isn't a string it can read.
fn unreadable(text: &str) -> AgentStringError {
    if let Some(version) = text
        .strip_prefix(MARK)
        .and_then(|rest| rest.split_once('_'))
        .map(|(version, _)| version)
        && !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
    {
        return AgentStringError::Newer;
    }
    if text.starts_with(BODY_MARK) {
        AgentStringError::NotAnAgentString
    } else if is_agent_string(text) {
        AgentStringError::Wrapped
    } else if text.starts_with(TOKEN_PREFIX) {
        AgentStringError::JustAToken
    } else if text.starts_with(BEGIN_KEY) {
        AgentStringError::JustAKey
    } else {
        AgentStringError::NotAnAgentString
    }
}

/// `hra_` and the characters a bearer token can hold.
fn is_token(token: &str) -> bool {
    token.strip_prefix(TOKEN_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKEND: &str = "https://api.hushrun.com/o/acme";
    const TOKEN: &str = "hra_0123456789abcdefghijABCDEFGHIJ0123456789";
    const KEY: &str =
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIA==\n-----END PRIVATE KEY-----\n";

    fn secrets_in(text: &str) -> bool {
        text.contains("MC4CAQAwBQYDK2VwBCIEIA") || text.contains(TOKEN)
    }

    // covers: C-17
    #[test]
    fn a_string_reads_back_what_it_was_made_of() {
        let text = AgentString::encode(BACKEND, TOKEN, KEY);
        assert!(text.starts_with("hushrun_agent_v1_"), "{text}");
        let agent = AgentString::parse(&text).unwrap();
        assert_eq!(agent.backend().to_string(), BACKEND);
        assert_eq!(agent.token(), TOKEN);
        assert_eq!(agent.key(), KEY);
        // As a paste, `$(cat file)` or `docker --env-file` leaves it.
        assert_eq!(AgentString::parse(&format!("  {text}\n")).unwrap(), agent);
        assert_eq!(AgentString::parse(&format!("\"{text}\"")).unwrap(), agent);
        assert_eq!(AgentString::parse(&format!("'{text}'")).unwrap(), agent);
    }

    // covers: C-17
    #[test]
    fn a_string_is_safe_in_a_shell_and_a_secrets_form() {
        let text = AgentString::encode(BACKEND, TOKEN, KEY);
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "{text}"
        );
    }

    // The form is a contract: the dashboard writes it too, so this vector pins it. ui/e2e's
    // agents spec decodes the dashboard's own, and the guide shows how to build one by hand.
    // covers: C-17
    #[test]
    fn the_form_is_this_one() {
        assert_eq!(
            AgentString::encode("http://127.0.0.1:1/o/a", "hra_x", "k"),
            "hushrun_agent_v1_eyJiYWNrZW5kIjoiaHR0cDovLzEyNy4wLjAuMToxL28vYSIsImtleSI6ImsiLCJ0b2tlbiI6ImhyYV94In0"
        );
    }

    // covers: C-14, C-17
    #[test]
    fn whats_wrong_with_a_string_is_said_without_its_value() {
        let good = AgentString::encode(BACKEND, TOKEN, KEY);
        let encoded = |payload: serde_json::Value| {
            format!(
                "{PREFIX}{}",
                URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes())
            )
        };
        let cases = [
            (TOKEN.to_string(), AgentStringError::JustAToken),
            (KEY.to_string(), AgentStringError::JustAKey),
            (
                "not a string".to_string(),
                AgentStringError::NotAnAgentString,
            ),
            (good.replace("_v1_", "_v2_"), AgentStringError::Newer),
            (format!("HUSHRUN_AGENT={good}"), AgentStringError::Wrapped),
            (format!("Bearer {good}"), AgentStringError::Wrapped),
            // Without its prefix.
            (
                good["hushrun_agent_v1_".len()..].to_string(),
                AgentStringError::NotAnAgentString,
            ),
            // Cut short, whatever the length.
            (
                good[..good.len() - 7].to_string(),
                AgentStringError::Damaged,
            ),
            (
                good[..good.len() - 8].to_string(),
                AgentStringError::Damaged,
            ),
            // A form that wrapped it.
            (
                format!("{}\n{}", &good[..60], &good[60..]),
                AgentStringError::Damaged,
            ),
            (PREFIX.to_string(), AgentStringError::Damaged),
            (
                encoded(json!({"backend": BACKEND})),
                AgentStringError::Damaged,
            ),
            (
                encoded(json!({"backend": "file:///tmp/x", "token": TOKEN, "key": KEY})),
                AgentStringError::Backend,
            ),
            (
                encoded(json!({"backend": "http://example.com/o/a", "token": TOKEN, "key": KEY})),
                AgentStringError::Backend,
            ),
            (
                encoded(json!({"backend": BACKEND, "token": "hru_personal", "key": KEY})),
                AgentStringError::Token,
            ),
            (
                encoded(json!({"backend": BACKEND, "token": "hra_a\nb", "key": KEY})),
                AgentStringError::Token,
            ),
            (
                encoded(json!({"backend": BACKEND, "token": TOKEN, "key": "not a key"})),
                AgentStringError::Key,
            ),
        ];
        for (text, expected) in cases {
            let err = AgentString::parse(&text).unwrap_err();
            assert_eq!(err, expected, "{text:?}");
            // Nothing of what it holds is in the message, even a value it read and refused.
            let message = err.to_string();
            assert!(!secrets_in(&message), "{message}");
            assert!(!message.contains("hru_personal"), "{message}");
            assert!(message.contains("HUSHRUN_AGENT"), "{message}");
        }
    }

    // covers: C-17
    #[test]
    fn debug_shows_no_secret() {
        let agent = AgentString::parse(&AgentString::encode(BACKEND, TOKEN, KEY)).unwrap();
        let shown = format!("{agent:?}");
        assert!(!secrets_in(&shown), "{shown}");
        assert!(
            shown.contains(BACKEND) && shown.contains("hra_012345…"),
            "{shown}"
        );
    }

    // covers: C-13
    #[test]
    fn a_string_is_told_from_something_that_only_names_one() {
        let good = AgentString::encode(BACKEND, TOKEN, KEY);
        let cut = &good["hushrun_agent_v1_".len()..];
        let body = "A".repeat(MIN_BODY);
        // However it got there: docker's --env-file keeps quotes, and people paste whole lines.
        for holds in [
            good.clone(),
            format!(" {good}\n"),
            format!("\"{good}\""),
            format!("Bearer {good}"),
            format!("HUSHRUN_AGENT={good}"),
            format!("https://host/o/{good}/x"),
            // Without its prefix, or its end.
            cut.to_string(),
            format!("\"{cut}\""),
            good[..good.len() / 2].to_string(),
            // Any version, and as short as counts, in either form.
            format!("hushrun_agent_v2_{body}"),
            format!("hushrun_agent_v12_{body}"),
            format!("{BODY_MARK}{}", &body[BODY_MARK.len()..]),
        ] {
            assert!(is_agent_string(&holds), "{holds}");
        }
        // A person's words, a file, a test or a search that names the thing are none of this.
        for names in [
            TOKEN.to_string(),
            "hru_abc".to_string(),
            "hushrun_agent_".to_string(),
            "hushrun_agent_v1_".to_string(),
            "hushrun_agent_v1_abc".to_string(),
            "hushrun_agent_string".to_string(),
            "hushrun_agent_notes.txt".to_string(),
            "src/hushrun_agent_v1_notes.txt".to_string(),
            "https://example.com/hushrun_agent_/repo.git".to_string(),
            "tests::a_hushrun_agent_string_is_an_agent_whether_it_reads_or_not".to_string(),
            format!("hushrun_agent_v_{body}"),
            format!("hushrun_agent_vx_{body}"),
            format!("hushrun_agent_v1_{}", &body[1..]),
            format!("hushrun_agent_v1_{} {body}", &body[..8]),
            format!("{BODY_MARK}{}", &body[BODY_MARK.len() + 1..]),
            // A JWT, which starts much the same.
            "eyJhbGciOiJSUzI1NiIsImtpZCI6Ii1fIn0.eyJzdWIiOiJyZXBvOmFjbWUifQ.c2lnbmF0dXJl"
                .to_string(),
        ] {
            assert!(!is_agent_string(&names), "{names}");
        }
        // What's below the length holds no secret: the key's own bytes start past it, whatever
        // the URL, since the JSON's keys and the PEM's first line come first. And a real string is
        // far past it.
        let shortest = AgentString::encode("https://a/o/b", TOKEN, KEY);
        let encoded = &shortest["hushrun_agent_v1_".len()..];
        let json = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
        assert!(json.find("MC4C").unwrap() > MIN_BODY * 3 / 4, "{json}");
        assert!(encoded.len() > 3 * MIN_BODY, "{encoded}");
    }

    // covers: C-11
    #[test]
    fn a_url_holds_no_string() {
        let good = AgentString::encode(BACKEND, TOKEN, KEY);
        for text in [
            format!("https://host/o/{good}"),
            format!("https://{good}/o/acme"),
            good.clone(),
        ] {
            let err = LedgerUrl::parse(&text).unwrap_err().to_string();
            assert!(!err.contains(&good), "{err}");
            assert!(err.contains(NOT_SHOWN), "{err}");
        }
        // A path that names the thing is a path.
        assert!(LedgerUrl::parse("https://host/o/hushrun_agent_notes").is_ok());
    }
}

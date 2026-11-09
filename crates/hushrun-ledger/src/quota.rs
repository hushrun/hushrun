//! \[ADR 0032\] A write that hushrun's server refuses for the org's plan: 402 with the code
//! `quota_exceeded` (L-10). The hosted service limits the checks an org's plan records in a
//! period; past the limit, a new `check` event or a `pass` or `fail` result gets this answer,
//! and hushrun records nothing for it and goes on (L-5, C-14).

use std::fmt;

use serde_json::Value;

/// The error code of a refusal for the org's plan (L-10).
pub const QUOTA_EXCEEDED: &str = "quota_exceeded";

/// The longest message kept, in characters.
const MAX_MESSAGE: usize = 300;
/// The longest upgrade URL kept, in bytes.
const MAX_URL: usize = 2048;

/// What the server said when it refused a write for the org's plan. Only the message is sure to
/// be there; the rest is what the answer carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quota {
    /// One line a person can act on, in the server's words.
    pub message: String,
    /// Where the org's admins upgrade its plan, such as `https://app.hushrun.com/acme/-/billing`.
    pub upgrade_url: Option<String>,
    /// The org's plan, such as `free`.
    pub plan: Option<String>,
    /// The checks the org recorded this period.
    pub used: Option<u64>,
    /// Where recording stops.
    pub limit: Option<u64>,
    /// When the period ends, as the server wrote it (RFC 3339).
    pub period_end: Option<String>,
}

impl Quota {
    /// The refusal an answer describes: a 402 whose `error.code` is `quota_exceeded`. `None` for
    /// any other answer, which keeps its usual meaning (L-10). The other fields are top-level;
    /// one the server put inside `error` instead is read too.
    pub fn from_answer(status: u16, body: &[u8]) -> Option<Quota> {
        if status != 402 {
            return None;
        }
        let value: Value = serde_json::from_slice(body).ok()?;
        let error = value.get("error")?;
        if error.get("code").and_then(Value::as_str) != Some(QUOTA_EXCEEDED) {
            return None;
        }
        let field = |name: &str| {
            value
                .get(name)
                .filter(|v| !v.is_null())
                .or_else(|| error.get(name))
        };
        let text = |name: &str| {
            field(name)
                .and_then(Value::as_str)
                .map(one_line)
                .filter(|t| !t.is_empty())
        };
        let number = |name: &str| field(name).and_then(Value::as_u64);
        Some(Quota {
            message: error
                .get("message")
                .and_then(Value::as_str)
                .map(one_line)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| {
                    "the org has recorded every check its plan allows this period".to_string()
                }),
            upgrade_url: field("upgrade_url")
                .and_then(Value::as_str)
                .filter(|url| is_web_url(url))
                .map(str::to_string),
            plan: text("plan"),
            used: number("used"),
            limit: number("limit"),
            period_end: text("period_end"),
        })
    }

    /// The message and where to upgrade, as hushrun prints them: `<message>; upgrade: <url>`,
    /// or the message alone when the answer names no upgrade URL or the message names it
    /// already.
    pub fn line(&self) -> String {
        match &self.upgrade_url {
            Some(url) if !self.message.contains(url.as_str()) => {
                format!("{}; upgrade: {url}", self.message)
            }
            _ => self.message.clone(),
        }
    }
}

impl fmt::Display for Quota {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

/// `text` on one line, as a terminal shows it safely: control characters become spaces, runs
/// of spaces one, and it is cut at [`MAX_MESSAGE`] characters.
fn one_line(text: &str) -> String {
    let mut line = String::new();
    for c in text.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' && (line.is_empty() || line.ends_with(' ')) {
            continue;
        }
        line.push(c);
    }
    let line = line.trim_end();
    match line.char_indices().nth(MAX_MESSAGE) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// An `https://` or `http://` URL with nothing a terminal would act on: no spaces or control
/// characters, and not overlong.
fn is_web_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty())
        && url.len() <= MAX_URL
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    // covers: L-10
    #[test]
    fn reads_the_refusal_of_the_orgs_plan() {
        let answer = body(serde_json::json!({
            "error": {"code": "quota_exceeded", "message": "acme used the 2,000 checks of its Free plan this month"},
            "plan": "free", "used": 2000, "limit": 2000, "period_end": "2026-11-01T00:00:00Z",
            "upgrade_url": "https://app.hushrun.com/acme/-/billing",
        }));
        let quota = Quota::from_answer(402, &answer).unwrap();
        assert_eq!(
            quota,
            Quota {
                message: "acme used the 2,000 checks of its Free plan this month".into(),
                upgrade_url: Some("https://app.hushrun.com/acme/-/billing".into()),
                plan: Some("free".into()),
                used: Some(2000),
                limit: Some(2000),
                period_end: Some("2026-11-01T00:00:00Z".into()),
            }
        );
        assert_eq!(
            quota.to_string(),
            "acme used the 2,000 checks of its Free plan this month; upgrade: https://app.hushrun.com/acme/-/billing"
        );
        // Another status, another code, or no JSON keep their meaning.
        assert_eq!(Quota::from_answer(403, &answer), None);
        for other in [
            body(serde_json::json!({"error": {"code": "payment_required", "message": "m"}})),
            body(serde_json::json!({"plan": "free"})),
            b"<html>402</html>".to_vec(),
        ] {
            assert_eq!(Quota::from_answer(402, &other), None);
        }
    }

    // covers: L-10
    #[test]
    fn only_the_message_is_needed_and_it_stays_one_safe_line() {
        let bare = body(serde_json::json!({"error": {"code": "quota_exceeded"}}));
        let quota = Quota::from_answer(402, &bare).unwrap();
        assert_eq!(
            quota.line(),
            "the org has recorded every check its plan allows this period"
        );
        assert_eq!(
            (quota.upgrade_url, quota.plan, quota.used, quota.limit),
            (None, None, None, None)
        );
        // Fields inside `error` are read too; a message that names the URL isn't repeated.
        let inside = body(serde_json::json!({"error": {
            "code": "quota_exceeded",
            "message": "over the limit:\n\tupgrade at https://app.example/acme/-/billing\u{1b}[2J",
            "upgrade_url": "https://app.example/acme/-/billing", "used": 5
        }}));
        let quota = Quota::from_answer(402, &inside).unwrap();
        assert_eq!(
            quota.line(),
            "over the limit: upgrade at https://app.example/acme/-/billing [2J"
        );
        assert_eq!(quota.used, Some(5));
        // An upgrade URL a terminal could act on, or that isn't a web page's, is dropped.
        for url in [
            "javascript:alert(1)",
            "https://app.example/a b",
            "https://app.example/\u{7}",
            "https://",
        ] {
            let answer = body(serde_json::json!({
                "error": {"code": "quota_exceeded", "message": "m"}, "upgrade_url": url,
            }));
            assert_eq!(
                Quota::from_answer(402, &answer).unwrap().upgrade_url,
                None,
                "{url:?}"
            );
        }
        // A long message is cut.
        let long = body(
            serde_json::json!({"error": {"code": "quota_exceeded", "message": "x".repeat(400)}}),
        );
        let message = Quota::from_answer(402, &long).unwrap().message;
        assert_eq!(message.chars().count(), MAX_MESSAGE + 1);
        assert!(message.ends_with('…'));
    }
}

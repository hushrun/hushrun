//! What the HTTP ledger needs from the network (L-4): the I/O runtime it blocks on, the budget
//! of a command's requests, retry backoff, and the crypto provider of its TLS.
//!
//! The [`Ledger`](crate::Ledger) trait is synchronous (ADR 0005), so the HTTP backend owns a
//! small tokio runtime and blocks on it.

use std::future::Future;
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use crate::LedgerError;

/// Concurrent reads per `get_many` (§12.4).
pub(crate) const READ_CONCURRENCY: usize = 16;

/// The runtime an HTTP ledger blocks on, and its budget.
pub(crate) struct Io {
    runtime: tokio::runtime::Runtime,
    budget: Option<Budget>,
    /// What to call the ledger in messages, such as `https://hushrun.example/l/acme/back`.
    name: String,
}

impl std::fmt::Debug for Io {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Io")
            .field("name", &self.name)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// The total time a command's ledger requests may take (L-4: 60 s for `plan`). Time the
/// command spends waiting by design isn't spent from it.
#[derive(Debug)]
pub(crate) struct Budget {
    limit: Duration,
    spent: Mutex<Duration>,
}

impl Budget {
    fn new(limit: Duration) -> Self {
        Budget {
            limit,
            spent: Mutex::new(Duration::ZERO),
        }
    }

    fn remaining(&self) -> Duration {
        self.limit
            .saturating_sub(*self.spent.lock().expect("budget lock"))
    }

    fn spend(&self, elapsed: Duration) {
        *self.spent.lock().expect("budget lock") += elapsed;
    }
}

impl Io {
    /// A four-thread runtime for the ledger called `name`, with an optional budget.
    pub(crate) fn new(
        name: impl Into<String>,
        budget: Option<Duration>,
    ) -> Result<Io, LedgerError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("hushrun-ledger")
            .enable_all()
            .build()
            .map_err(|e| LedgerError::Unavailable(format!("starting the I/O runtime: {e}")))?;
        Ok(Io {
            runtime,
            budget: budget.map(Budget::new),
            name: name.into(),
        })
    }

    /// Runs `op` on the runtime, within what is left of the budget.
    pub(crate) fn run<T>(
        &self,
        op: impl Future<Output = Result<T, LedgerError>>,
    ) -> Result<T, LedgerError> {
        let Some(budget) = &self.budget else {
            return self.runtime.block_on(op);
        };
        let remaining = budget.remaining();
        if remaining.is_zero() {
            return Err(self.out_of_budget(budget));
        }
        let started = Instant::now();
        let result = self
            .runtime
            .block_on(async { tokio::time::timeout(remaining, op).await });
        budget.spend(started.elapsed());
        result.unwrap_or_else(|_| Err(self.out_of_budget(budget)))
    }

    /// What is left of the budget, measured when the current [`run`](Io::run) started; `None`
    /// without a budget.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.budget.as_ref().map(Budget::remaining)
    }

    fn out_of_budget(&self, budget: &Budget) -> LedgerError {
        LedgerError::Unavailable(format!(
            "{}: the requests to hushrun's server took longer than this command's {} s budget",
            self.name,
            budget.limit.as_secs()
        ))
    }

    /// Spends `elapsed` of the budget, as if requests had taken it.
    #[cfg(test)]
    pub(crate) fn spend(&self, elapsed: Duration) {
        if let Some(budget) = &self.budget {
            budget.spend(elapsed);
        }
    }
}

/// Exponential backoff from `base` with jitter: `base`, twice `base`, … plus up to half again
/// (L-4).
pub(crate) fn backoff_from(base: Duration, attempt: u32) -> Duration {
    let base =
        u64::try_from(base.as_millis()).unwrap_or(u64::MAX) << attempt.saturating_sub(1).min(5);
    let mut random = [0u8; 2];
    let _ = getrandom::fill(&mut random);
    let jitter = u64::from(u16::from_le_bytes(random)) % (base / 2 + 1);
    Duration::from_millis(base.saturating_add(jitter))
}

/// rustls uses ring (plan §12.3): reqwest is built without a provider, so one must be
/// installed before the first client.
pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Fails only when another provider is already the default, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Percent-encodes `pairs` as `application/x-www-form-urlencoded`.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    fn encode(text: &str, out: &mut String) {
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(byte));
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        encode(key, &mut out);
        out.push('=');
        encode(value, &mut out);
    }
    out
}

/// At most `max` characters of a response body or of stderr, for error messages.
pub(crate) fn excerpt(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_percent_encoded() {
        assert_eq!(
            form(&[("grant_type", "urn:x:y"), ("scope", "a b/c"), ("t", "é~")]),
            "grant_type=urn%3Ax%3Ay&scope=a%20b%2Fc&t=%C3%A9~"
        );
    }

    #[test]
    fn excerpts_cut_on_characters() {
        assert_eq!(excerpt("  short \n", 10), "short");
        assert_eq!(excerpt("ééééé", 3), "ééé…");
    }

    #[test]
    fn backoff_grows_with_jitter() {
        for attempt in 1..=3 {
            let base = 200u64 << (attempt - 1);
            let d = backoff_from(Duration::from_millis(200), attempt).as_millis() as u64;
            assert!((base..=base + base / 2).contains(&d), "{attempt}: {d}");
        }
        assert_eq!(backoff_from(Duration::ZERO, 3), Duration::ZERO);
    }

    // covers: L-4
    #[test]
    fn a_spent_budget_fails_fast_as_unavailable() {
        let io = Io::new("mem://test", Some(Duration::from_millis(50))).unwrap();
        assert_eq!(io.run(async { Ok(1) }), Ok(1));
        io.spend(Duration::from_secs(1));
        let err = io.run(async { Ok(2) }).unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(err.to_string().contains("budget"), "{err}");
        // A slow operation is cut off when the budget runs out.
        let io = Io::new("mem://test", Some(Duration::from_millis(50))).unwrap();
        let err = io
            .run(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
            .unwrap_err();
        assert!(err.to_string().contains("budget"), "{err}");
        assert!(Io::new("x", None).unwrap().remaining().is_none());
    }
}

//! K-14: one request to the backend outside a relay, carrying what a relay's requests carry
//! ([`super::credential::Credential`], [`super::http::Client`]): in GitHub Actions the job's ID
//! token for the org's URL, in GitLab CI the job's ID token with its key's headers. `packages bake
//! --oci hushrun` asks the backend for a push grant with it. The credential is settled when the
//! caller is made, before the bake's fetch runs anything of the repository's (ADR 0102); the
//! token is minted when the request goes.

use std::time::Duration;

use super::credential::Credential;
use super::http::Client;

/// A client for single requests to an org's backend.
pub(crate) struct Caller {
    client: Client,
    runtime: tokio::runtime::Runtime,
}

/// What the backend answered, whatever its status.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

impl Caller {
    /// The caller of `access`, the org a run's build requests reach, with the credential they
    /// carry. `Err`: there is none to send.
    pub(crate) fn of(access: &crate::results::BuildAccess) -> Result<Caller, String> {
        let credential = access
            .credential
            .as_ref()
            .ok_or_else(|| "no credential to send".to_string())?;
        let credential = Credential::of(credential, &access.base, &access.org)?;
        Caller::with(&access.base, credential)
    }

    /// A caller of the org at `base` with a token, as tests send.
    #[cfg(test)]
    pub(crate) fn with_token(base: &str, token: &str) -> Caller {
        Caller::with(base, Credential::Token(token.into())).expect("a caller")
    }

    fn with(base: &str, credential: Credential) -> Result<Caller, String> {
        let client = Client::new(base, credential, true)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("starting the backend's client: {e}"))?;
        Ok(Caller { client, runtime })
    }

    /// `POST {base}/{path}` of `body`, waiting at most `silence` on a server that sends nothing
    /// (ADR 0144): the answer, whatever its status. A 401 to an ID token is tried once more with
    /// a fresh one, a 503 or 429 whose `Retry-After` fits once more. `Err`: no answer came.
    pub(crate) fn post(
        &self,
        path: &str,
        body: Vec<u8>,
        silence: Duration,
    ) -> Result<Answer, String> {
        let reply = self
            .runtime
            .block_on(self.client.post_raw(path, body, silence, None))
            .map_err(|f| f.line())?;
        Ok(Answer {
            status: reply.status,
            body: reply.body,
        })
    }
}

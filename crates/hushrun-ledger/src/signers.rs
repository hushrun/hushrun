//! \[ADR 0031\] A repository's signers, as its server answers them (L-13): who counts for its
//! promotions (T-15), with the server's statement over the document, signed with its log key,
//! so that the answer, saved, can be checked later against the key the repository pins (T-9).

use serde_json::{Value, json};

use hushrun_core::signers::Signers;
use hushrun_tlog::signers::SignersStatement;

use hushrun_tlog::note::Verifier;

/// The signers a server answered, with the document as it came and its statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignersAnswer {
    pub signers: Signers,
    /// The document, as answered: what the statement signs and `hushrun verify --signers` reads.
    pub document: Value,
    /// The server's statement (a signed note), when it keeps a log.
    pub statement: Option<String>,
    /// Whether the statement was checked against the log key the repository pins.
    pub checked: bool,
}

impl SignersAnswer {
    /// The answer as JSON: `{"signers": <document>, "statement": <note or null>}`, the shape of
    /// `GET {base}/signers` and of a saved answer.
    pub fn to_json(&self) -> Value {
        json!({"signers": self.document, "statement": self.statement})
    }
}

/// Reads an answer, `{"signers": …, "statement": …}`, and checks it: the document parses, it is
/// for `repository`, and, with `log`, the statement is signed by that log key and hashes the
/// document. Without `log` nothing can be checked: the answer is the server's word over TLS.
pub fn check_answer(
    answer: &Value,
    repository: &str,
    log: Option<&Verifier>,
) -> Result<SignersAnswer, String> {
    let document = answer
        .get("signers")
        .ok_or("the answer has no `signers` document")?
        .clone();
    let statement = match answer.get("statement") {
        None | Some(Value::Null) => None,
        Some(Value::String(note)) => Some(note.clone()),
        Some(_) => return Err("`statement` isn't a signed note".into()),
    };
    let signers = Signers::from_json(&document)?;
    if signers.repository != repository {
        return Err(format!(
            "the signers are for {}, and this is {repository}",
            signers.repository
        ));
    }
    let checked = match log {
        None => false,
        Some(verifier) => {
            let note = statement.as_deref().ok_or(
                "the answer carries no statement, but hushrun.yaml names the server's log key, so it can't be checked",
            )?;
            let opened = SignersStatement::open(note, verifier).map_err(|e| e.to_string())?;
            let digest = Signers::digest_of(&document)?;
            if opened.document.hex() != digest.hex() {
                return Err(
                    "the statement signs another document than the one answered".to_string()
                );
            }
            true
        }
    };
    Ok(SignersAnswer {
        signers,
        document,
        statement,
        checked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_core::signers::PublicKey;
    use hushrun_tlog::hash::Hash;
    use hushrun_tlog::note::Signer;

    const ORIGIN: &str = "hushrun.example/l/acme/back";

    fn answer(signer: &Signer) -> Value {
        let document = Signers::example(
            "github.com/acme/back",
            &[(&PublicKey::Ed25519([1; 32]), "user:ana@acme.dev")],
        );
        let digest = Signers::digest_of(&document).unwrap();
        let statement = SignersStatement {
            origin: ORIGIN.into(),
            document: Hash::from_hex(&digest.hex()).unwrap(),
        };
        json!({"signers": document, "statement": statement.sign(signer).unwrap()})
    }

    fn key(signer: &Signer) -> Verifier {
        signer.verifier()
    }

    // covers: L-13
    #[test]
    fn checks_the_statement_against_the_pinned_key() {
        let signer = Signer::new(ORIGIN, [7; 32]).unwrap();
        let good = answer(&signer);
        let checked = check_answer(&good, "github.com/acme/back", Some(&key(&signer))).unwrap();
        assert!(checked.checked);
        assert_eq!(checked.signers.machines.len(), 1);
        assert_eq!(checked.to_json(), good);
        // Unchecked without a pinned key.
        assert!(
            !check_answer(&good, "github.com/acme/back", None)
                .unwrap()
                .checked
        );
        // Another key, another document, no statement, another repository.
        let other = Signer::new(ORIGIN, [8; 32]).unwrap();
        assert!(check_answer(&good, "github.com/acme/back", Some(&key(&other))).is_err());
        let mut changed = good.clone();
        changed["signers"]["machines"][0]["name"] = json!("someone-else");
        let err = check_answer(&changed, "github.com/acme/back", Some(&key(&signer))).unwrap_err();
        assert!(err.contains("another document"), "{err}");
        let mut bare = good.clone();
        bare["statement"] = Value::Null;
        assert!(check_answer(&bare, "github.com/acme/back", Some(&key(&signer))).is_err());
        assert!(check_answer(&good, "github.com/acme/front", None).is_err());
    }
}

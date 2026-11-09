//! X.509 certificates (E-10, E-11): Fulcio's signing certificates, and the certificate
//! authority and timestamp authority chains of a Sigstore trust root. Certificates are parsed
//! with `x509-cert`; their signatures are checked with ring over the bytes as written.

use std::ops::Range;

use der::asn1::{ObjectIdentifier, OctetStringRef, Utf8StringRef};
use der::{Decode, Encode};
use ring::signature as ring_sig;
use sha2::Digest as _;
use x509_cert::Certificate;
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAltName};
use x509_cert::spki::SubjectPublicKeyInfoOwned;

use hushrun_core::signers::{CertIdentity, PublicKey, Window};
use hushrun_core::time::Time;

use crate::der_raw;

/// Object identifiers hushrun reads.
pub(crate) mod oid {
    use der::asn1::ObjectIdentifier as Oid;

    pub const EC_PUBLIC_KEY: Oid = Oid::new_unwrap("1.2.840.10045.2.1");
    pub const P256: Oid = Oid::new_unwrap("1.2.840.10045.3.1.7");
    pub const P384: Oid = Oid::new_unwrap("1.3.132.0.34");
    pub const RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.1");
    pub const ED25519: Oid = Oid::new_unwrap("1.3.101.112");
    pub const ECDSA_SHA256: Oid = Oid::new_unwrap("1.2.840.10045.4.3.2");
    pub const ECDSA_SHA384: Oid = Oid::new_unwrap("1.2.840.10045.4.3.3");
    pub const ECDSA_SHA512: Oid = Oid::new_unwrap("1.2.840.10045.4.3.4");
    pub const RSA_SHA256: Oid = Oid::new_unwrap("1.2.840.113549.1.1.11");
    pub const RSA_SHA384: Oid = Oid::new_unwrap("1.2.840.113549.1.1.12");
    pub const RSA_SHA512: Oid = Oid::new_unwrap("1.2.840.113549.1.1.13");
    pub const SHA1: Oid = Oid::new_unwrap("1.3.14.3.2.26");
    pub const SHA256: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.1");
    pub const SHA384: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.2");
    pub const SHA512: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.3");

    pub const SUBJECT_KEY_ID: Oid = Oid::new_unwrap("2.5.29.14");
    pub const KEY_USAGE: Oid = Oid::new_unwrap("2.5.29.15");
    pub const SUBJECT_ALT_NAME: Oid = Oid::new_unwrap("2.5.29.17");
    pub const BASIC_CONSTRAINTS: Oid = Oid::new_unwrap("2.5.29.19");
    pub const EXT_KEY_USAGE: Oid = Oid::new_unwrap("2.5.29.37");
    /// RFC 6962's embedded SCT list.
    pub const SCT_LIST: Oid = Oid::new_unwrap("1.3.6.1.4.1.11129.2.4.2");
    pub const KP_CODE_SIGNING: Oid = Oid::new_unwrap("1.3.6.1.5.5.7.3.3");
    pub const KP_TIME_STAMPING: Oid = Oid::new_unwrap("1.3.6.1.5.5.7.3.8");

    /// Fulcio's extensions (1.3.6.1.4.1.57264.1.*): the OIDC issuer, raw (deprecated) and DER.
    pub const FULCIO_ISSUER_V1: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.1");
    pub const FULCIO_USERNAME: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.7");
    pub const FULCIO_ISSUER: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.8");
    pub const BUILD_SIGNER_URI: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.9");
    pub const SOURCE_REPOSITORY_URI: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.12");
    pub const SOURCE_REPOSITORY_REF: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.14");
    pub const SOURCE_REPOSITORY_ID: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.15");
    pub const BUILD_CONFIG_URI: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.18");
    pub const BUILD_TRIGGER: Oid = Oid::new_unwrap("1.3.6.1.4.1.57264.1.20");
}

/// Extensions whose meaning hushrun checks, so they may be critical.
const UNDERSTOOD: [ObjectIdentifier; 4] = [
    oid::KEY_USAGE,
    oid::EXT_KEY_USAGE,
    oid::SUBJECT_ALT_NAME,
    oid::BASIC_CONSTRAINTS,
];

/// The digest of `bytes` with the hash algorithm `algorithm`: SHA-256, SHA-384, SHA-512, or
/// SHA-1 where RFC 3161 tokens still use it to name a certificate.
pub(crate) fn digest(algorithm: &ObjectIdentifier, bytes: &[u8]) -> Option<Vec<u8>> {
    Some(match *algorithm {
        oid::SHA256 => sha2::Sha256::digest(bytes).to_vec(),
        oid::SHA384 => sha2::Sha384::digest(bytes).to_vec(),
        oid::SHA512 => sha2::Sha512::digest(bytes).to_vec(),
        oid::SHA1 => ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, bytes)
            .as_ref()
            .to_vec(),
        _ => return None,
    })
}

/// The name of a hash algorithm, for messages.
pub(crate) fn hash_name(algorithm: &ObjectIdentifier) -> String {
    match *algorithm {
        oid::SHA256 => "SHA-256".into(),
        oid::SHA384 => "SHA-384".into(),
        oid::SHA512 => "SHA-512".into(),
        oid::SHA1 => "SHA-1".into(),
        other => other.to_string(),
    }
}

/// Verifies `signature` over `message` with the key in `spki`. `algorithm` is the signature
/// algorithm; CMS may name only the key's algorithm, and then `digest` names the hash.
pub(crate) fn verify_with(
    spki: &SubjectPublicKeyInfoOwned,
    algorithm: &ObjectIdentifier,
    digest: Option<&ObjectIdentifier>,
    message: &[u8],
    signature: &[u8],
) -> Result<(), String> {
    let hash = match *algorithm {
        oid::ECDSA_SHA256 | oid::RSA_SHA256 => Some(oid::SHA256),
        oid::ECDSA_SHA384 | oid::RSA_SHA384 => Some(oid::SHA384),
        oid::ECDSA_SHA512 | oid::RSA_SHA512 => Some(oid::SHA512),
        oid::EC_PUBLIC_KEY | oid::RSA => digest.copied(),
        oid::ED25519 => None,
        other => return Err(format!("unsupported signature algorithm {other}")),
    };
    let ecdsa = matches!(
        *algorithm,
        oid::ECDSA_SHA256 | oid::ECDSA_SHA384 | oid::ECDSA_SHA512 | oid::EC_PUBLIC_KEY
    );
    let rsa = matches!(
        *algorithm,
        oid::RSA_SHA256 | oid::RSA_SHA384 | oid::RSA_SHA512 | oid::RSA
    );
    let verifier: &dyn ring_sig::VerificationAlgorithm = match spki.algorithm.oid {
        oid::EC_PUBLIC_KEY if ecdsa => {
            let curve: ObjectIdentifier = spki
                .algorithm
                .parameters
                .as_ref()
                .and_then(|p| p.decode_as().ok())
                .ok_or("an EC key without a named curve")?;
            match (curve, hash) {
                (oid::P256, Some(oid::SHA256)) => &ring_sig::ECDSA_P256_SHA256_ASN1,
                (oid::P256, Some(oid::SHA384)) => &ring_sig::ECDSA_P256_SHA384_ASN1,
                (oid::P384, Some(oid::SHA256)) => &ring_sig::ECDSA_P384_SHA256_ASN1,
                (oid::P384, Some(oid::SHA384)) => &ring_sig::ECDSA_P384_SHA384_ASN1,
                (curve, hash) => {
                    return Err(format!(
                        "unsupported ECDSA curve {curve} with {}",
                        hash.map_or("no hash".into(), |h| hash_name(&h))
                    ));
                }
            }
        }
        oid::RSA if rsa => match hash {
            Some(oid::SHA256) => &ring_sig::RSA_PKCS1_2048_8192_SHA256,
            Some(oid::SHA384) => &ring_sig::RSA_PKCS1_2048_8192_SHA384,
            Some(oid::SHA512) => &ring_sig::RSA_PKCS1_2048_8192_SHA512,
            _ => return Err("unsupported RSA hash".into()),
        },
        oid::ED25519 if *algorithm == oid::ED25519 => &ring_sig::ED25519,
        key => {
            return Err(format!("a {key} key can't make {algorithm} signatures"));
        }
    };
    let key = spki
        .subject_public_key
        .as_bytes()
        .ok_or("a public key with unused bits")?;
    ring_sig::UnparsedPublicKey::new(verifier, key)
        .verify(message, signature)
        .map_err(|_| "the signature doesn't verify".to_string())
}

/// A certificate, parsed, with its DER bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cert {
    pub der: Vec<u8>,
    pub inner: Certificate,
    /// Where the `TBSCertificate` is in `der`.
    tbs: Range<usize>,
}

impl Cert {
    /// Parses a DER certificate. A certificate with an extension twice is refused (RFC 5280).
    pub fn parse(der: &[u8]) -> Result<Cert, String> {
        let inner =
            Certificate::from_der(der).map_err(|e| format!("not an X.509 certificate: {e}"))?;
        let outer = der_raw::only(der)?;
        let (tbs, _) = der_raw::next(outer.value)?;
        let start = der.len() - outer.value.len();
        let cert = Cert {
            der: der.to_vec(),
            inner,
            tbs: start..start + tbs.whole.len(),
        };
        let mut seen: Vec<&ObjectIdentifier> = Vec::new();
        for extension in cert.extensions() {
            if seen.contains(&&extension.extn_id) {
                return Err(format!("the extension {} appears twice", extension.extn_id));
            }
            seen.push(&extension.extn_id);
        }
        Ok(cert)
    }

    /// Parses a PEM certificate (`-----BEGIN CERTIFICATE-----`).
    pub fn from_pem(pem: &str) -> Result<Cert, String> {
        let (label, der) =
            der::pem::decode_vec(pem.trim().as_bytes()).map_err(|e| format!("not PEM: {e}"))?;
        if label != "CERTIFICATE" {
            return Err(format!("a {label} block, not a certificate"));
        }
        Cert::parse(&der)
    }

    /// The `TBSCertificate` as written: what the issuer signed.
    pub fn tbs(&self) -> &[u8] {
        &self.der[self.tbs.clone()]
    }

    pub fn extensions(&self) -> &[Extension] {
        self.inner
            .tbs_certificate
            .extensions
            .as_deref()
            .unwrap_or(&[])
    }

    fn extension(&self, oid: &ObjectIdentifier) -> Option<&Extension> {
        self.extensions().iter().find(|e| &e.extn_id == oid)
    }

    fn decode<'a, T: Decode<'a>>(&'a self, oid: &ObjectIdentifier) -> Result<Option<T>, String> {
        self.extension(oid)
            .map(|e| T::from_der(e.extn_value.as_bytes()).map_err(|err| format!("{oid}: {err}")))
            .transpose()
    }

    pub fn not_before(&self) -> Time {
        time(self.inner.tbs_certificate.validity.not_before)
    }

    pub fn not_after(&self) -> Time {
        time(self.inner.tbs_certificate.validity.not_after)
    }

    /// When the certificate is valid, both ends included.
    pub fn validity(&self) -> Window {
        Window {
            not_before: Some(self.not_before()),
            not_after: Some(self.not_after()),
        }
    }

    pub fn spki(&self) -> &SubjectPublicKeyInfoOwned {
        &self.inner.tbs_certificate.subject_public_key_info
    }

    /// Whether `BasicConstraints` says it is a CA.
    pub fn is_ca(&self) -> bool {
        matches!(
            self.decode::<BasicConstraints>(&oid::BASIC_CONSTRAINTS),
            Ok(Some(BasicConstraints { ca: true, .. }))
        )
    }

    /// Whether the extended key usage lists `purpose`.
    pub fn has_purpose(&self, purpose: &ObjectIdentifier) -> bool {
        matches!(
            self.decode::<ExtendedKeyUsage>(&oid::EXT_KEY_USAGE),
            Ok(Some(eku)) if eku.0.contains(purpose)
        )
    }

    /// The critical extension, if any, whose meaning hushrun doesn't check: such a certificate
    /// must not be accepted (RFC 5280 §4.2).
    pub fn unknown_critical(&self) -> Option<&ObjectIdentifier> {
        self.extensions()
            .iter()
            .find(|e| e.critical && !UNDERSTOOD.contains(&e.extn_id))
            .map(|e| &e.extn_id)
    }

    /// Checks that `issuer` issued this certificate: it names `issuer`'s subject, `issuer` is
    /// a CA that may sign certificates, and its signature verifies.
    pub fn verify_issued_by(&self, issuer: &Cert) -> Result<(), String> {
        let tbs = &self.inner.tbs_certificate;
        if tbs.issuer != issuer.inner.tbs_certificate.subject {
            return Err(format!(
                "it names the issuer {}, not {}",
                tbs.issuer, issuer.inner.tbs_certificate.subject
            ));
        }
        if !issuer.is_ca() {
            return Err(format!(
                "{} is not a CA",
                issuer.inner.tbs_certificate.subject
            ));
        }
        if let Ok(Some(usage)) = issuer.decode::<KeyUsage>(&oid::KEY_USAGE)
            && !usage.key_cert_sign()
        {
            return Err(format!(
                "{} may not sign certificates",
                issuer.inner.tbs_certificate.subject
            ));
        }
        if self.inner.signature_algorithm != tbs.signature {
            return Err("its two signature algorithms differ".into());
        }
        let signature = self
            .inner
            .signature
            .as_bytes()
            .ok_or("a signature with unused bits")?;
        verify_with(
            issuer.spki(),
            &self.inner.signature_algorithm.oid,
            None,
            self.tbs(),
            signature,
        )
        .map_err(|e| format!("{}'s signature: {e}", issuer.inner.tbs_certificate.subject))
    }

    /// The key of a signing certificate (E-11): a P-256 key for digital signatures and code
    /// signing, in a certificate that is no CA and has no critical extension hushrun doesn't
    /// check.
    pub fn signing_key(&self) -> Result<PublicKey, String> {
        if self.is_ca() {
            return Err("a CA certificate, not a signing certificate".into());
        }
        if let Some(unknown) = self.unknown_critical() {
            return Err(format!("an unknown critical extension {unknown}"));
        }
        match self.decode::<KeyUsage>(&oid::KEY_USAGE)? {
            Some(usage) if usage.digital_signature() => {}
            _ => return Err("its key usage doesn't allow digital signatures".into()),
        }
        if !self.has_purpose(&oid::KP_CODE_SIGNING) {
            return Err("its extended key usage doesn't allow code signing".into());
        }
        let spki = self.spki().to_der().map_err(|e| e.to_string())?;
        PublicKey::from_p256_spki(&spki).map_err(|e| format!("its key: {e}"))
    }

    /// Who the certificate names (E-11): the OIDC issuer, the subject alternative name, and
    /// Fulcio's CI extensions.
    pub fn identity(&self) -> Result<CertIdentity, String> {
        let issuer = match self.utf8(&oid::FULCIO_ISSUER)? {
            Some(issuer) => issuer,
            None => self
                .extension(&oid::FULCIO_ISSUER_V1)
                .map(|e| String::from_utf8(e.extn_value.as_bytes().to_vec()))
                .transpose()
                .map_err(|_| "the issuer extension is not UTF-8".to_string())?
                .ok_or("no OIDC issuer extension (1.3.6.1.4.1.57264.1.8)")?,
        };
        let names = self
            .decode::<SubjectAltName>(&oid::SUBJECT_ALT_NAME)?
            .ok_or("no subject alternative name")?;
        let subject = names
            .0
            .iter()
            .find_map(|name| match name {
                GeneralName::UniformResourceIdentifier(uri) => Some(uri.to_string()),
                GeneralName::Rfc822Name(email) => Some(email.to_string()),
                GeneralName::OtherName(other) if other.type_id == oid::FULCIO_USERNAME => other
                    .value
                    .decode_as::<Utf8StringRef<'_>>()
                    .ok()
                    .map(|s| s.as_str().to_string()),
                _ => None,
            })
            .ok_or("no URI, email or username in the subject alternative name")?;
        Ok(CertIdentity {
            issuer,
            subject,
            repository_id: self.utf8(&oid::SOURCE_REPOSITORY_ID)?,
            repository: self.utf8(&oid::SOURCE_REPOSITORY_URI)?,
            reference: self.utf8(&oid::SOURCE_REPOSITORY_REF)?,
            workflow: self.utf8(&oid::BUILD_CONFIG_URI)?,
            signer_workflow: self.utf8(&oid::BUILD_SIGNER_URI)?,
            trigger: self.utf8(&oid::BUILD_TRIGGER)?,
        })
    }

    /// A Fulcio extension holding a DER UTF8String.
    fn utf8(&self, oid: &ObjectIdentifier) -> Result<Option<String>, String> {
        self.extension(oid)
            .map(|e| {
                Utf8StringRef::from_der(e.extn_value.as_bytes())
                    .map(|s| s.as_str().to_string())
                    .map_err(|err| format!("{oid}: {err}"))
            })
            .transpose()
    }

    /// The SHA-256 of the DER `SubjectPublicKeyInfo`: a CT log's id, or an issuer key hash.
    pub fn spki_sha256(&self) -> Result<[u8; 32], String> {
        let der = self.spki().to_der().map_err(|e| e.to_string())?;
        Ok(sha2::Sha256::digest(der).into())
    }
}

fn time(t: x509_cert::time::Time) -> Time {
    let millis = i64::try_from(t.to_unix_duration().as_millis()).unwrap_or(i64::MAX);
    Time::from_millis(millis).unwrap_or_else(|_| Time::from_millis(0).expect("the epoch"))
}

/// A certificate transparency log of a trust root (RFC 6962), which signs SCTs with a P-256 key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtLog {
    pub base_url: String,
    /// The SHA-256 of its key's DER `SubjectPublicKeyInfo`.
    pub log_id: [u8; 32],
    pub key: PublicKey,
    pub valid_for: Window,
}

/// One SCT of a certificate's embedded list.
struct Sct<'a> {
    log_id: &'a [u8],
    timestamp: u64,
    extensions: &'a [u8],
    signature: &'a [u8],
}

/// Reads TLS's opaque `<0..2^16-1>`: a 2-byte length and the bytes.
fn opaque16(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let length = usize::from(u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?));
    let value = bytes.get(2..2 + length)?;
    Some((value, &bytes[2 + length..]))
}

fn parse_scts(list: &[u8]) -> Option<Vec<Sct<'_>>> {
    let (mut entries, rest) = opaque16(list)?;
    if !rest.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    while !entries.is_empty() {
        let (sct, rest) = opaque16(entries)?;
        entries = rest;
        // Version 1 (0): log id, timestamp, extensions, then SHA-256 with ECDSA (4, 3).
        if sct.first() != Some(&0) || sct.len() < 1 + 32 + 8 + 2 {
            continue;
        }
        let log_id = &sct[1..33];
        let timestamp = u64::from_be_bytes(sct[33..41].try_into().ok()?);
        let (extensions, rest) = opaque16(&sct[41..])?;
        if rest.get(..2) != Some(&[4, 3]) {
            continue;
        }
        let (signature, rest) = opaque16(&rest[2..])?;
        if !rest.is_empty() {
            return None;
        }
        out.push(Sct {
            log_id,
            timestamp,
            extensions,
            signature,
        });
    }
    Some(out)
}

/// Verifies that one of `leaf`'s embedded SCTs (RFC 6962 §3.3) is signed by one of `logs`, which
/// was valid when it logged the certificate: the certificate authority published the
/// certificate, so misissuance is visible.
pub fn verify_embedded_sct(leaf: &Cert, issuer: &Cert, logs: &[CtLog]) -> Result<(), String> {
    let extension = leaf
        .extension(&oid::SCT_LIST)
        .ok_or("the certificate carries no SCT")?;
    let list = OctetStringRef::from_der(extension.extn_value.as_bytes())
        .map_err(|e| format!("the SCT list: {e}"))?;
    let scts = parse_scts(list.as_bytes()).ok_or("the SCT list is malformed")?;
    // The precertificate's TBSCertificate: the certificate's, without the SCT list.
    let mut tbs = leaf.inner.tbs_certificate.clone();
    tbs.extensions = tbs.extensions.map(|extensions| {
        extensions
            .into_iter()
            .filter(|e| e.extn_id != oid::SCT_LIST)
            .collect()
    });
    let tbs = tbs.to_der().map_err(|e| e.to_string())?;
    let issuer_key_hash = issuer.spki_sha256()?;
    let mut reasons = Vec::new();
    for sct in scts {
        let Some(log) = logs.iter().find(|log| log.log_id == sct.log_id) else {
            reasons.push(format!(
                "an SCT of an unknown log {}",
                hex::encode(sct.log_id)
            ));
            continue;
        };
        let logged = i64::try_from(sct.timestamp)
            .ok()
            .and_then(|ms| Time::from_millis(ms).ok());
        if !logged.is_some_and(|t| log.valid_for.contains(t)) {
            reasons.push(format!(
                "{} wasn't valid when it logged the certificate",
                log.base_url
            ));
            continue;
        }
        let mut signed = Vec::with_capacity(tbs.len() + 64);
        signed.extend([0, 0]); // version 1, certificate_timestamp
        signed.extend(sct.timestamp.to_be_bytes());
        signed.extend([0, 1]); // precert_entry
        signed.extend(issuer_key_hash);
        let length = u32::try_from(tbs.len()).map_err(|_| "an oversized certificate")?;
        signed.extend(&length.to_be_bytes()[1..]);
        signed.extend(&tbs);
        signed.extend(
            u16::try_from(sct.extensions.len())
                .map_err(|_| "oversized SCT extensions")?
                .to_be_bytes(),
        );
        signed.extend(sct.extensions);
        if crate::keys::verify_signature(&log.key, &signed, sct.signature) {
            return Ok(());
        }
        reasons.push(format!("the SCT of {} doesn't verify", log.base_url));
    }
    Err(if reasons.is_empty() {
        "the SCT list is empty".into()
    } else {
        reasons.join("; ")
    })
}

#[cfg(test)]
mod tests;

//! RFC 3161 timestamps (E-10) \[M10\]: a timestamp authority's signed statement that a
//! signature existed at a time. hushrun asks for one over each event's signature, and the
//! bundle carries the authority's whole `TimeStampResp`, as Sigstore bundles do. A timestamp
//! counts only when a timestamp authority of the trust root signed it.

use der::asn1::{Any, BitString, Int, ObjectIdentifier, OctetString, OctetStringRef};
use der::{Decode, Encode, Sequence, Tag, Tagged};
use x509_cert::ext::Extensions;
use x509_cert::ext::pkix::SubjectKeyIdentifier;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::spki::AlgorithmIdentifierOwned;

use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerIdentifier};

use hushrun_core::time::Time;

use crate::der_raw;
use crate::trust_root::{Authority, TrustedRoot};
use crate::x509::{Cert, digest, hash_name, oid, verify_with};

/// The media type of a request; answers are `application/timestamp-reply`.
pub const REQUEST_CONTENT_TYPE: &str = "application/timestamp-query";

const SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const TST_INFO: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4");
const CONTENT_TYPE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
const MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const SIGNING_CERTIFICATE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.12");
const SIGNING_CERTIFICATE_V2: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.47");

/// `MessageImprint ::= SEQUENCE { hashAlgorithm AlgorithmIdentifier, hashedMessage OCTET STRING }`
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct MessageImprint {
    pub hash_algorithm: AlgorithmIdentifierOwned,
    pub hashed_message: OctetString,
}

/// RFC 3161 §2.4.1.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct TimeStampReq {
    pub version: u8,
    pub message_imprint: MessageImprint,
    #[asn1(optional = "true")]
    pub req_policy: Option<ObjectIdentifier>,
    #[asn1(optional = "true")]
    pub nonce: Option<Int>,
    #[asn1(default = "Default::default")]
    pub cert_req: bool,
    #[asn1(context_specific = "0", tag_mode = "IMPLICIT", optional = "true")]
    pub extensions: Option<Extensions>,
}

/// RFC 3161 §2.4.2.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct PkiStatusInfo {
    pub status: u32,
    #[asn1(optional = "true")]
    pub status_string: Option<Vec<String>>,
    #[asn1(optional = "true")]
    pub fail_info: Option<BitString>,
}

/// RFC 3161 §2.4.2.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct TimeStampResp {
    pub status: PkiStatusInfo,
    #[asn1(optional = "true")]
    pub time_stamp_token: Option<ContentInfo>,
}

/// RFC 3161 §2.4.2. `genTime` is kept raw: authorities may add fractions of a second, which a
/// strict `GeneralizedTime` refuses.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct TstInfo {
    pub version: u8,
    pub policy: ObjectIdentifier,
    pub message_imprint: MessageImprint,
    pub serial_number: Int,
    pub gen_time: Any,
    #[asn1(optional = "true")]
    pub accuracy: Option<Accuracy>,
    #[asn1(default = "Default::default")]
    pub ordering: bool,
    #[asn1(optional = "true")]
    pub nonce: Option<Int>,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    pub tsa: Option<GeneralName>,
    #[asn1(context_specific = "1", tag_mode = "IMPLICIT", optional = "true")]
    pub extensions: Option<Extensions>,
}

#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct Accuracy {
    #[asn1(optional = "true")]
    pub seconds: Option<u32>,
    #[asn1(context_specific = "0", tag_mode = "IMPLICIT", optional = "true")]
    pub millis: Option<u16>,
    #[asn1(context_specific = "1", tag_mode = "IMPLICIT", optional = "true")]
    pub micros: Option<u16>,
}

/// `ESSCertID` (RFC 2634) and `ESSCertIDv2` (RFC 5035): a hash of the signer's certificate.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct EssCertId {
    cert_hash: OctetString,
    #[asn1(optional = "true")]
    issuer_serial: Option<Any>,
}

#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct EssCertIdV2 {
    #[asn1(optional = "true")]
    pub hash_algorithm: Option<AlgorithmIdentifierOwned>,
    pub cert_hash: OctetString,
    #[asn1(optional = "true")]
    pub issuer_serial: Option<Any>,
}

#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct SigningCertificate {
    certs: Vec<EssCertId>,
    #[asn1(optional = "true")]
    policies: Option<Any>,
}

#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
pub(crate) struct SigningCertificateV2 {
    pub certs: Vec<EssCertIdV2>,
    #[asn1(optional = "true")]
    pub policies: Option<Any>,
}

/// A verified timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timestamp {
    /// When the authority says the signature existed (`genTime`), to the millisecond.
    pub time: Time,
    /// The authority's URI in the trust root.
    pub authority: String,
}

fn sha256_identifier() -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned {
        oid: oid::SHA256,
        parameters: Some(Any::new(Tag::Null, Vec::new()).expect("NULL encodes")),
    }
}

/// A DER `TimeStampReq` for the SHA-256 of `signature`, asking for the authority's certificate,
/// with `nonce` (8 bytes, the first not zero and below 0x80, so the INTEGER is positive and
/// minimal).
pub fn request(signature: &[u8], nonce: &[u8; 8]) -> Vec<u8> {
    let message_imprint = MessageImprint {
        hash_algorithm: sha256_identifier(),
        hashed_message: OctetString::new(digest(&oid::SHA256, signature).expect("SHA-256"))
            .expect("32 bytes"),
    };
    TimeStampReq {
        version: 1,
        message_imprint,
        req_policy: None,
        nonce: Some(Int::new(nonce).expect("a minimal positive INTEGER")),
        cert_req: true,
        extensions: None,
    }
    .to_der()
    .expect("requests encode")
}

/// A random nonce for [`request`].
pub fn nonce() -> [u8; 8] {
    let mut nonce = [0u8; 8];
    // Without OS randomness the nonce is weaker, never wrong: the imprint binds the answer.
    let _ = getrandom::fill(&mut nonce);
    nonce[0] = (nonce[0] & 0x7f).max(1);
    nonce
}

/// Verifies a DER `TimeStampResp` over `signature` against `root`'s timestamp authorities
/// (E-10): the authority granted it, its imprint is the hash of `signature`, a timestamp
/// authority of the root signed it and was valid then, and, when `nonce` is given, it echoes
/// the request's nonce.
pub fn verify(
    response: &[u8],
    signature: &[u8],
    root: &TrustedRoot,
    nonce: Option<&[u8]>,
) -> Result<Timestamp, String> {
    let response =
        TimeStampResp::from_der(response).map_err(|e| format!("not a TimeStampResp: {e}"))?;
    if response.status.status > 1 {
        let why = response
            .status
            .status_string
            .map(|s| s.join("; "))
            .unwrap_or_default();
        return Err(format!(
            "the authority refused (status {}{})",
            response.status.status,
            if why.is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        ));
    }
    let token = response
        .time_stamp_token
        .ok_or("the response has no timestamp token")?;
    verify_token(&token, signature, root, nonce)
}

/// Verifies a timestamp token: a CMS `ContentInfo` around `SignedData` around `TSTInfo`.
fn verify_token(
    token: &ContentInfo,
    signature: &[u8],
    root: &TrustedRoot,
    nonce: Option<&[u8]>,
) -> Result<Timestamp, String> {
    if token.content_type != SIGNED_DATA {
        return Err(format!("a {} token, not SignedData", token.content_type));
    }
    let token_der = token.to_der().map_err(|e| e.to_string())?;
    let signed: SignedData = token
        .content
        .decode_as()
        .map_err(|e| format!("SignedData: {e}"))?;
    let encapsulated = &signed.encap_content_info;
    if encapsulated.econtent_type != TST_INFO {
        return Err(format!(
            "the token holds {}, not TSTInfo",
            encapsulated.econtent_type
        ));
    }
    let content = encapsulated
        .econtent
        .as_ref()
        .ok_or("the token has no TSTInfo")?;
    let tst_der = OctetStringRef::try_from(content)
        .map_err(|e| format!("TSTInfo: {e}"))?
        .as_bytes();
    let tst = TstInfo::from_der(tst_der).map_err(|e| format!("TSTInfo: {e}"))?;
    if tst.version != 1 {
        return Err(format!("TSTInfo version {}", tst.version));
    }

    // The imprint is the hash of the signature.
    let algorithm = &tst.message_imprint.hash_algorithm.oid;
    let expected = digest(algorithm, signature)
        .filter(|_| *algorithm != oid::SHA1)
        .ok_or_else(|| format!("unsupported imprint hash {}", hash_name(algorithm)))?;
    if tst.message_imprint.hashed_message.as_bytes() != expected.as_slice() {
        return Err("the timestamp is over another signature".into());
    }
    if let Some(nonce) = nonce
        && tst.nonce.as_ref().map(|n| n.as_bytes()) != Some(nonce)
    {
        return Err("the timestamp doesn't echo the request's nonce".into());
    }
    let time = gen_time(&tst.gen_time)?;

    // One signer, with the signed attributes RFC 3161 needs.
    let mut signers = signed.signer_infos.0.iter();
    let signer = match (signers.next(), signers.next()) {
        (Some(signer), None) => signer,
        _ => return Err("a timestamp token has exactly one signer".into()),
    };
    let attributes = signer
        .signed_attrs
        .as_ref()
        .ok_or("the signer has no signed attributes")?;
    let attribute = |oid: &ObjectIdentifier| -> Result<&Any, String> {
        let mut found = attributes.iter().filter(|a| &a.oid == oid);
        match (found.next(), found.next()) {
            (Some(a), None) if a.values.len() == 1 => {
                Ok(a.values.iter().next().expect("one value"))
            }
            (None, _) => Err(format!("no {oid} attribute")),
            _ => Err(format!(
                "the {oid} attribute must appear once, with one value"
            )),
        }
    };
    let content_type: ObjectIdentifier = attribute(&CONTENT_TYPE)?
        .decode_as()
        .map_err(|e| format!("content type: {e}"))?;
    if content_type != TST_INFO {
        return Err("the signed content type isn't TSTInfo".into());
    }
    let digest_algorithm = &signer.digest_alg.oid;
    let message_digest = OctetStringRef::try_from(attribute(&MESSAGE_DIGEST)?)
        .map_err(|e| format!("message digest: {e}"))?;
    let tst_digest = digest(digest_algorithm, tst_der)
        .filter(|_| *digest_algorithm != oid::SHA1)
        .ok_or_else(|| format!("unsupported digest {}", hash_name(digest_algorithm)))?;
    if message_digest.as_bytes() != tst_digest.as_slice() {
        return Err("the signed message digest isn't TSTInfo's".into());
    }

    // The signer's certificate: in the token, or the authority's in the trust root.
    let mut candidates: Vec<Cert> = Vec::new();
    for choice in signed.certificates.iter().flat_map(|set| set.0.iter()) {
        if let CertificateChoices::Certificate(cert) = choice {
            let der = cert.to_der().map_err(|e| e.to_string())?;
            candidates.push(Cert::parse(&der)?);
        }
    }
    candidates.extend(
        root.timestamp_authorities
            .iter()
            .flat_map(|a| a.chain.iter().cloned()),
    );
    let cert = candidates
        .iter()
        .find(|cert| identifies(&signer.sid, cert))
        .ok_or("the token's signer is neither in it nor in the trust root")?;
    bound_to(attributes, cert)?;

    // The signature over the signed attributes, as written, with their SET tag.
    let mut signed_attributes = raw_signed_attributes(&token_der)?;
    signed_attributes[0] = 0x31;
    verify_with(
        cert.spki(),
        &signer.signature_algorithm.oid,
        Some(digest_algorithm),
        &signed_attributes,
        signer.signature.as_bytes(),
    )
    .map_err(|e| format!("the token's signature: {e}"))?;

    let authority = anchored(cert, root, time)?;
    Ok(Timestamp {
        time,
        authority: authority.uri.clone(),
    })
}

/// Whether `cert` is the one `sid` names.
fn identifies(sid: &SignerIdentifier, cert: &Cert) -> bool {
    let tbs = &cert.inner.tbs_certificate;
    match sid {
        SignerIdentifier::IssuerAndSerialNumber(id) => {
            id.issuer == tbs.issuer && id.serial_number == tbs.serial_number
        }
        SignerIdentifier::SubjectKeyIdentifier(id) => tbs
            .extensions
            .iter()
            .flatten()
            .filter(|e| e.extn_id == oid::SUBJECT_KEY_ID)
            .any(|e| {
                SubjectKeyIdentifier::from_der(e.extn_value.as_bytes())
                    .is_ok_and(|ski| ski.0 == id.0)
            }),
    }
}

/// The ESS signing-certificate attribute, when present, names `cert` by its hash: an attacker
/// can't swap in another certificate with the same name and serial.
fn bound_to(attributes: &x509_cert::attr::Attributes, cert: &Cert) -> Result<(), String> {
    for attribute in attributes.iter() {
        let Some(value) = attribute.values.iter().next() else {
            continue;
        };
        let (algorithm, hash) = match attribute.oid {
            SIGNING_CERTIFICATE_V2 => {
                let v2: SigningCertificateV2 = value
                    .decode_as()
                    .map_err(|e| format!("signing certificate: {e}"))?;
                let first = v2.certs.first().ok_or("an empty signing certificate")?;
                (
                    first.hash_algorithm.as_ref().map_or(oid::SHA256, |a| a.oid),
                    first.cert_hash.as_bytes().to_vec(),
                )
            }
            SIGNING_CERTIFICATE => {
                let v1: SigningCertificate = value
                    .decode_as()
                    .map_err(|e| format!("signing certificate: {e}"))?;
                let first = v1.certs.first().ok_or("an empty signing certificate")?;
                (oid::SHA1, first.cert_hash.as_bytes().to_vec())
            }
            _ => continue,
        };
        let actual = digest(&algorithm, &cert.der)
            .ok_or_else(|| format!("unsupported certificate hash {}", hash_name(&algorithm)))?;
        if actual != hash {
            return Err("the signing certificate attribute names another certificate".into());
        }
    }
    Ok(())
}

/// The signer certificate belongs to a timestamp authority of `root` trusted at `time`: it is
/// the authority's own leaf, or one of the authority's certificates issued it. It may timestamp
/// and was valid at `time`.
fn anchored<'r>(cert: &Cert, root: &'r TrustedRoot, time: Time) -> Result<&'r Authority, String> {
    if !cert.has_purpose(&oid::KP_TIME_STAMPING) {
        return Err("the signer's certificate is not for timestamping".into());
    }
    if !cert.validity().contains(time) {
        return Err(format!(
            "the signer's certificate is valid {}, not at {time}",
            cert.validity()
        ));
    }
    let mut reasons = Vec::new();
    for authority in &root.timestamp_authorities {
        if !authority.valid_for.contains(time) {
            reasons.push(format!(
                "{} is trusted {}, not at {time}",
                authority.uri, authority.valid_for
            ));
            continue;
        }
        let listed = authority
            .chain
            .first()
            .is_some_and(|leaf| leaf.der == cert.der);
        let issued = || {
            authority
                .chain
                .iter()
                .any(|ca| ca.validity().contains(time) && cert.verify_issued_by(ca).is_ok())
        };
        if listed || issued() {
            return Ok(authority);
        }
    }
    Err(if reasons.is_empty() {
        format!("no timestamp authority of {} signed it", root.source)
    } else {
        format!(
            "no timestamp authority of {} signed it ({})",
            root.source,
            reasons.join("; ")
        )
    })
}

/// The signed attributes of a token's only signer, as written (`[0] IMPLICIT SET OF`).
fn raw_signed_attributes(token: &[u8]) -> Result<Vec<u8>, String> {
    let content_info = der_raw::children(der_raw::only(token)?.value)?;
    let explicit = content_info.get(1).ok_or("no SignedData")?;
    let signed_data = der_raw::children(der_raw::only(explicit.value)?.value)?;
    let signer_infos = signed_data
        .last()
        .filter(|t| t.tag == 0x31)
        .ok_or("no signer infos")?;
    let signer = der_raw::children(signer_infos.value)?;
    let signer = der_raw::children(signer.first().ok_or("no signer info")?.value)?;
    signer
        .get(3)
        .filter(|t| t.tag == 0xa0)
        .map(|t| t.whole.to_vec())
        .ok_or_else(|| "no signed attributes".to_string())
}

/// `GeneralizedTime` with an optional fraction: `YYYYMMDDHHMMSS[.f…]Z`.
fn gen_time(value: &Any) -> Result<Time, String> {
    if value.tag() != Tag::GeneralizedTime {
        return Err("genTime is not a GeneralizedTime".into());
    }
    let text = std::str::from_utf8(value.value()).map_err(|_| "genTime is not ASCII")?;
    let bad = || format!("genTime {text:?} is not YYYYMMDDHHMMSS[.fff]Z");
    let body = text.strip_suffix('Z').ok_or_else(bad)?;
    let (whole, fraction) = match body.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (body, ""),
    };
    if whole.len() != 14
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || (body.contains('.') && fraction.is_empty())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    let millis = format!("{fraction:0<3}");
    Time::parse(&format!(
        "{}-{}-{}T{}:{}:{}.{}Z",
        &whole[..4],
        &whole[4..6],
        &whole[6..8],
        &whole[8..10],
        &whole[10..12],
        &whole[12..14],
        &millis[..3]
    ))
    .map_err(|_| bad())
}

#[cfg(test)]
mod tests;

//! A throwaway Sigstore for tests (feature `testing`): a certificate authority that issues
//! Fulcio-style signing certificates with an SCT from a CT log, a timestamp authority, the
//! `trusted_root.json` that trusts them, a TUF repository that serves it, and the answers a
//! fake Fulcio and a fake timestamp authority give. Every key is made per process; nothing
//! here is trusted anywhere else.

use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::time::Duration;

use base64::Engine as _;
use der::asn1::{
    Any, BitString, GeneralizedTime, Ia5String, Int, ObjectIdentifier, OctetString, SetOfVec,
    UtcTime, Utf8StringRef,
};
use der::{Decode, Encode, Tag};
use ring::rand::SystemRandom;
use ring::signature::{self as ring_sig, EcdsaKeyPair, KeyPair as _};
use serde_json::{Value, json};
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::certificate::{TbsCertificate, Version};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages, SubjectAltName,
};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::time::Validity;

use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};

use hushrun_core::signers::{CertIdentity, PublicKey};
use hushrun_core::time::{Dur, Time};

use crate::keys::SigningKey;
use crate::timestamp::{
    EssCertIdV2, MessageImprint, PkiStatusInfo, SigningCertificateV2, TimeStampReq, TimeStampResp,
    TstInfo,
};
use crate::tuf::canonical_json;
use crate::x509::{Cert, digest, oid};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
/// How long the fake Fulcio's certificates last, as Fulcio's do.
pub const CERTIFICATE_LIFETIME: Dur = Dur::from_secs(600);
/// The issuer of GitHub Actions' ID tokens.
pub const GITHUB_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// A P-256 key pair that signs certificates, timestamps and SCTs.
pub struct TestKey {
    pair: EcdsaKeyPair,
}

impl TestKey {
    pub fn generate() -> TestKey {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ring_sig::ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .expect("a P-256 key");
        TestKey {
            pair: EcdsaKeyPair::from_pkcs8(
                &ring_sig::ECDSA_P256_SHA256_ASN1_SIGNING,
                pkcs8.as_ref(),
                &rng,
            )
            .expect("its own PKCS#8"),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_p256_point(self.pair.public_key().as_ref()).expect("uncompressed")
    }

    pub fn spki(&self) -> SubjectPublicKeyInfoOwned {
        SubjectPublicKeyInfoOwned::from_der(&self.public_key().encoded()).expect("an SPKI")
    }

    /// A DER ECDSA signature over the SHA-256 of `message`.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair
            .sign(&SystemRandom::new(), message)
            .expect("signs")
            .as_ref()
            .to_vec()
    }
}

/// A certificate and the key it certifies.
pub struct Issuer {
    pub key: TestKey,
    pub cert: Cert,
}

fn x509_time(time: Time) -> x509_cert::time::Time {
    let seconds = Duration::from_secs(u64::try_from(time.millis() / 1000).expect("after 1970"));
    if seconds < Duration::from_secs(2_524_608_000) {
        x509_cert::time::Time::UtcTime(UtcTime::from_unix_duration(seconds).expect("UTCTime"))
    } else {
        x509_cert::time::Time::GeneralTime(
            GeneralizedTime::from_unix_duration(seconds).expect("GeneralizedTime"),
        )
    }
}

fn extension(id: ObjectIdentifier, critical: bool, value: &impl Encode) -> Extension {
    Extension {
        extn_id: id,
        critical,
        extn_value: OctetString::new(value.to_der().expect("encodes")).expect("an extension"),
    }
}

fn utf8_extension(id: ObjectIdentifier, value: &str) -> Extension {
    extension(id, false, &Utf8StringRef::new(value).expect("UTF-8"))
}

fn serial() -> SerialNumber {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("randomness");
    bytes[0] = (bytes[0] & 0x7f) | 0x01;
    SerialNumber::new(&bytes).expect("a serial")
}

/// A certificate for `spki`, signed by `signer` with ECDSA over SHA-256.
fn certificate(
    subject: &Name,
    issuer: &Name,
    spki: SubjectPublicKeyInfoOwned,
    validity: (Time, Time),
    extensions: Vec<Extension>,
    signer: &TestKey,
) -> Cert {
    let algorithm = AlgorithmIdentifierOwned {
        oid: oid::ECDSA_SHA256,
        parameters: None,
    };
    let tbs = TbsCertificate {
        version: Version::V3,
        serial_number: serial(),
        signature: algorithm.clone(),
        issuer: issuer.clone(),
        validity: Validity {
            not_before: x509_time(validity.0),
            not_after: x509_time(validity.1),
        },
        subject: subject.clone(),
        subject_public_key_info: spki,
        issuer_unique_id: None,
        subject_unique_id: None,
        extensions: Some(extensions),
    };
    sign_tbs(tbs, &algorithm, signer)
}

fn sign_tbs(tbs: TbsCertificate, algorithm: &AlgorithmIdentifierOwned, signer: &TestKey) -> Cert {
    let signature = signer.sign(&tbs.to_der().expect("a TBSCertificate"));
    let cert = Certificate {
        tbs_certificate: tbs,
        signature_algorithm: algorithm.clone(),
        signature: BitString::from_bytes(&signature).expect("a BIT STRING"),
    };
    Cert::parse(&cert.to_der().expect("a certificate")).expect("it parses")
}

fn name(text: &str) -> Name {
    Name::from_str(text).expect("an RFC 4514 name")
}

fn ca_extensions(path_len: Option<u8>) -> Vec<Extension> {
    vec![
        extension(
            oid::BASIC_CONSTRAINTS,
            true,
            &BasicConstraints {
                ca: true,
                path_len_constraint: path_len,
            },
        ),
        extension(
            oid::KEY_USAGE,
            true,
            &KeyUsage(KeyUsages::KeyCertSign | KeyUsages::CRLSign),
        ),
    ]
}

impl Issuer {
    fn root(subject: &str, validity: (Time, Time)) -> Issuer {
        let key = TestKey::generate();
        let subject = name(subject);
        let cert = certificate(
            &subject,
            &subject,
            key.spki(),
            validity,
            ca_extensions(None),
            &key,
        );
        Issuer { key, cert }
    }

    fn issue(&self, subject: &str, validity: (Time, Time), extensions: Vec<Extension>) -> Issuer {
        let key = TestKey::generate();
        let cert = certificate(
            &name(subject),
            &self.cert.inner.tbs_certificate.subject,
            key.spki(),
            validity,
            extensions,
            &self.key,
        );
        Issuer { key, cert }
    }
}

/// A throwaway Sigstore instance.
pub struct Pki {
    pub root: Issuer,
    pub intermediate: Issuer,
    pub tsa_root: Issuer,
    pub tsa: Issuer,
    /// The CT log that logs the signing certificates.
    pub ct_log: TestKey,
    /// Whether the trust root lists the CT log, so that SCTs are required.
    pub with_ct_log: bool,
    /// When the CAs and the timestamp authority are valid, and the trust root trusts them.
    pub valid: (Time, Time),
}

impl Default for Pki {
    fn default() -> Self {
        Pki::new()
    }
}

impl Pki {
    /// A certificate authority (a root and an intermediate), a timestamp authority (a root and
    /// a leaf) and a CT log, valid from 2020 to 2099.
    pub fn new() -> Pki {
        let valid = (
            Time::parse("2020-01-01T00:00:00Z").expect("time"),
            Time::parse("2099-01-01T00:00:00Z").expect("time"),
        );
        let root = Issuer::root("CN=test-fulcio-root,O=hushrun-test", valid);
        let intermediate = root.issue(
            "CN=test-fulcio-intermediate,O=hushrun-test",
            valid,
            ca_extensions(Some(0)),
        );
        let tsa_root = Issuer::root("CN=test-tsa-root,O=hushrun-test", valid);
        let tsa = tsa_root.issue(
            "CN=test-tsa,O=hushrun-test",
            valid,
            vec![
                extension(
                    oid::KEY_USAGE,
                    true,
                    &KeyUsage(KeyUsages::DigitalSignature.into()),
                ),
                extension(
                    oid::EXT_KEY_USAGE,
                    true,
                    &ExtendedKeyUsage(vec![oid::KP_TIME_STAMPING]),
                ),
            ],
        );
        Pki {
            root,
            intermediate,
            tsa_root,
            tsa,
            ct_log: TestKey::generate(),
            with_ct_log: true,
            valid,
        }
    }

    /// The `trusted_root.json` that trusts this instance.
    pub fn trusted_root(&self) -> Vec<u8> {
        let chain = |certs: &[&Cert]| json!({"certificates": certs.iter().map(|c| json!({"rawBytes": B64.encode(&c.der)})).collect::<Vec<_>>()});
        let valid_for = json!({"start": self.valid.0.to_string(), "end": self.valid.1.to_string()});
        let ct_key = self.ct_log.public_key().encoded();
        let ctlogs = if self.with_ct_log {
            json!([{
                "baseUrl": "https://ct.test",
                "hashAlgorithm": "SHA2_256",
                "publicKey": {"rawBytes": B64.encode(&ct_key), "keyDetails": "PKIX_ECDSA_P256_SHA_256", "validFor": {"start": self.valid.0.to_string()}},
                "logId": {"keyId": B64.encode(digest(&oid::SHA256, &ct_key).expect("SHA-256"))}
            }])
        } else {
            json!([])
        };
        serde_json::to_vec(&json!({
            "mediaType": "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
            "tlogs": [],
            "certificateAuthorities": [{
                "subject": {"organization": "hushrun-test", "commonName": "test-fulcio"},
                "uri": "https://fulcio.test",
                "certChain": chain(&[&self.intermediate.cert, &self.root.cert]),
                "validFor": valid_for
            }],
            "ctlogs": ctlogs,
            "timestampAuthorities": [{
                "subject": {"organization": "hushrun-test", "commonName": "test-tsa"},
                "uri": "https://tsa.test/api/v1/timestamp",
                "certChain": chain(&[&self.tsa.cert, &self.tsa_root.cert]),
                "validFor": valid_for
            }]
        }))
        .expect("JSON")
    }

    /// A Fulcio-style signing certificate for `key`, valid from `not_before` for 10 minutes,
    /// with `identity`'s issuer, subject and CI extensions, and an SCT from the CT log.
    pub fn signing_certificate(
        &self,
        key: &PublicKey,
        identity: &CertIdentity,
        not_before: Time,
    ) -> Cert {
        let algorithm = AlgorithmIdentifierOwned {
            oid: oid::ECDSA_SHA256,
            parameters: None,
        };
        let san = if identity.subject.contains("://") {
            GeneralName::UniformResourceIdentifier(
                Ia5String::new(&identity.subject).expect("an IA5 URI"),
            )
        } else {
            GeneralName::Rfc822Name(Ia5String::new(&identity.subject).expect("an IA5 email"))
        };
        let mut extensions = vec![
            extension(
                oid::KEY_USAGE,
                true,
                &KeyUsage(KeyUsages::DigitalSignature.into()),
            ),
            extension(
                oid::EXT_KEY_USAGE,
                false,
                &ExtendedKeyUsage(vec![oid::KP_CODE_SIGNING]),
            ),
            extension(oid::SUBJECT_ALT_NAME, true, &SubjectAltName(vec![san])),
            utf8_extension(oid::FULCIO_ISSUER, &identity.issuer),
        ];
        for (id, value) in [
            (oid::BUILD_SIGNER_URI, &identity.signer_workflow),
            (oid::SOURCE_REPOSITORY_URI, &identity.repository),
            (oid::SOURCE_REPOSITORY_REF, &identity.reference),
            (oid::SOURCE_REPOSITORY_ID, &identity.repository_id),
            (oid::BUILD_CONFIG_URI, &identity.workflow),
            (oid::BUILD_TRIGGER, &identity.trigger),
        ] {
            if let Some(value) = value {
                extensions.push(utf8_extension(id, value));
            }
        }
        let validity = (not_before, not_before.saturating_add(CERTIFICATE_LIFETIME));
        let mut tbs = TbsCertificate {
            version: Version::V3,
            serial_number: serial(),
            signature: algorithm.clone(),
            issuer: self.intermediate.cert.inner.tbs_certificate.subject.clone(),
            validity: Validity {
                not_before: x509_time(validity.0),
                not_after: x509_time(validity.1),
            },
            subject: Name::default(),
            subject_public_key_info: SubjectPublicKeyInfoOwned::from_der(&key.encoded())
                .expect("an SPKI"),
            issuer_unique_id: None,
            subject_unique_id: None,
            extensions: Some(extensions),
        };
        // The CT log signs the precertificate's TBSCertificate: everything but the SCT list.
        let sct = self.sct(&tbs.to_der().expect("TBS"), not_before);
        if let Some(extensions) = tbs.extensions.as_mut() {
            extensions.push(extension(
                oid::SCT_LIST,
                false,
                &OctetString::new(sct).expect("an SCT list"),
            ));
        }
        sign_tbs(tbs, &algorithm, &self.intermediate.key)
    }

    /// A TLS-encoded SCT list with one SCT over `precert_tbs` (RFC 6962 §3.2).
    fn sct(&self, precert_tbs: &[u8], at: Time) -> Vec<u8> {
        let log_id = digest(&oid::SHA256, &self.ct_log.public_key().encoded()).expect("SHA-256");
        let timestamp = u64::try_from(at.millis()).expect("after 1970");
        let issuer_key_hash = self.intermediate.cert.spki_sha256().expect("SPKI");
        let mut signed = vec![0, 0];
        signed.extend(timestamp.to_be_bytes());
        signed.extend([0, 1]);
        signed.extend(issuer_key_hash);
        signed.extend(&(precert_tbs.len() as u32).to_be_bytes()[1..]);
        signed.extend(precert_tbs);
        signed.extend([0, 0]);
        let signature = self.ct_log.sign(&signed);
        let mut sct = vec![0];
        sct.extend(&log_id);
        sct.extend(timestamp.to_be_bytes());
        sct.extend([0, 0, 4, 3]);
        sct.extend((signature.len() as u16).to_be_bytes());
        sct.extend(signature);
        let mut list = (sct.len() as u16).to_be_bytes().to_vec();
        list.extend(sct);
        let mut out = (list.len() as u16).to_be_bytes().to_vec();
        out.extend(list);
        out
    }

    /// A DER `TimeStampResp` over `signature` at `time`, signed by the timestamp authority,
    /// echoing `nonce`.
    pub fn timestamp(&self, signature: &[u8], time: Time, nonce: Option<&[u8]>) -> Vec<u8> {
        self.timestamp_with(
            MessageImprint {
                hash_algorithm: AlgorithmIdentifierOwned {
                    oid: oid::SHA256,
                    parameters: None,
                },
                hashed_message: OctetString::new(digest(&oid::SHA256, signature).expect("SHA-256"))
                    .expect("bytes"),
            },
            time,
            nonce.map(|n| Int::new(n).expect("an INTEGER")),
            &self.tsa,
        )
    }

    /// The timestamp authority's answer to a DER `TimeStampReq`, at `time`.
    pub fn answer(&self, request: &[u8], time: Time) -> Result<Vec<u8>, String> {
        let request = TimeStampReq::from_der(request).map_err(|e| e.to_string())?;
        if request.version != 1 {
            return Err(format!("version {}", request.version));
        }
        Ok(self.timestamp_with(request.message_imprint, time, request.nonce, &self.tsa))
    }

    /// A timestamp from another authority, which this instance's trust root doesn't list.
    pub fn foreign_timestamp(&self, signature: &[u8], time: Time) -> Vec<u8> {
        Pki::new().timestamp(signature, time, None)
    }

    /// A timestamp signed by `signer`.
    pub(crate) fn timestamp_with(
        &self,
        imprint: MessageImprint,
        time: Time,
        nonce: Option<Int>,
        signer: &Issuer,
    ) -> Vec<u8> {
        let millis = time.millis().rem_euclid(1000);
        let compact = time.to_string().replace(['-', ':', 'T'], "");
        let gen_time = format!("{}.{millis:03}Z", &compact[..14]);
        let tst = TstInfo {
            version: 1,
            policy: ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.2"),
            message_imprint: imprint,
            serial_number: Int::new(&[0x01, 0x23]).expect("INTEGER"),
            gen_time: Any::new(Tag::GeneralizedTime, gen_time.as_bytes()).expect("time"),
            accuracy: None,
            ordering: false,
            nonce,
            tsa: None,
            extensions: None,
        }
        .to_der()
        .expect("TSTInfo");
        let attribute = |id: &str, value: Any| Attribute {
            oid: ObjectIdentifier::new_unwrap(id),
            values: SetOfVec::try_from(vec![value]).expect("one value"),
        };
        // The signer's certificate by its SHA-256 (ESSCertIDv2's default hash).
        let signing_certificate = SigningCertificateV2 {
            certs: vec![EssCertIdV2 {
                hash_algorithm: None,
                cert_hash: OctetString::new(
                    digest(&oid::SHA256, &signer.cert.der).expect("SHA-256"),
                )
                .expect("bytes"),
                issuer_serial: None,
            }],
            policies: None,
        };
        let attributes = SetOfVec::try_from(vec![
            attribute(
                "1.2.840.113549.1.9.3",
                any(&ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4")),
            ),
            attribute(
                "1.2.840.113549.1.9.4",
                any(
                    &OctetString::new(digest(&oid::SHA256, &tst).expect("SHA-256")).expect("bytes"),
                ),
            ),
            attribute("1.2.840.113549.1.9.16.2.47", any(&signing_certificate)),
        ])
        .expect("attributes");
        let signature = signer.key.sign(&attributes.to_der().expect("SET OF"));
        let sha256 = AlgorithmIdentifierOwned {
            oid: oid::SHA256,
            parameters: None,
        };
        let tbs = &signer.cert.inner.tbs_certificate;
        let signer_info = SignerInfo {
            version: CmsVersion::V1,
            sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
                issuer: tbs.issuer.clone(),
                serial_number: tbs.serial_number.clone(),
            }),
            digest_alg: sha256.clone(),
            signed_attrs: Some(attributes),
            signature_algorithm: AlgorithmIdentifierOwned {
                oid: oid::ECDSA_SHA256,
                parameters: None,
            },
            signature: OctetString::new(signature).expect("bytes"),
            unsigned_attrs: None,
        };
        let signed_data = SignedData {
            version: CmsVersion::V3,
            digest_algorithms: SetOfVec::try_from(vec![sha256]).expect("one"),
            encap_content_info: EncapsulatedContentInfo {
                econtent_type: ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4"),
                econtent: Some(Any::new(Tag::OctetString, tst).expect("OCTET STRING")),
            },
            certificates: Some(CertificateSet(
                SetOfVec::try_from(vec![CertificateChoices::Certificate(
                    Certificate::from_der(&signer.cert.der).expect("certificate"),
                )])
                .expect("one"),
            )),
            crls: None,
            signer_infos: SignerInfos(SetOfVec::try_from(vec![signer_info]).expect("one")),
        };
        TimeStampResp {
            status: PkiStatusInfo {
                status: 0,
                status_string: None,
                fail_info: None,
            },
            time_stamp_token: Some(ContentInfo {
                content_type: ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2"),
                content: Any::encode_from(&signed_data).expect("SignedData"),
            }),
        }
        .to_der()
        .expect("TimeStampResp")
    }

    /// A refusal, as an authority answers a request it won't serve.
    pub fn refusal(text: &str) -> Vec<u8> {
        TimeStampResp {
            status: PkiStatusInfo {
                status: 2,
                status_string: Some(vec![text.to_string()]),
                fail_info: None,
            },
            time_stamp_token: None,
        }
        .to_der()
        .expect("TimeStampResp")
    }

    /// What Fulcio answers to `POST /api/v2/signingCert` (JSON in, JSON out): it checks the
    /// proof of possession, a signature over the token's `sub`, and certifies the key for the
    /// identity the token's claims describe, as Fulcio maps GitHub's and GitLab's claims.
    pub fn fulcio_answer(&self, body: &[u8], now: Time) -> Result<Value, String> {
        let request: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        let token = request["credentials"]["oidcIdentityToken"]
            .as_str()
            .ok_or("no oidcIdentityToken")?;
        let claims = jwt_claims(token)?;
        let key_request = &request["publicKeyRequest"];
        let pem = key_request["publicKey"]["content"]
            .as_str()
            .ok_or("no public key")?;
        let (_, spki) = der::pem::decode_vec(pem.as_bytes()).map_err(|e| e.to_string())?;
        let key = PublicKey::from_p256_spki(&spki)?;
        let proof = B64
            .decode(
                key_request["proofOfPossession"]
                    .as_str()
                    .ok_or("no proof")?,
            )
            .map_err(|e| e.to_string())?;
        // As Fulcio does: the proof signs the token's email when it has one, else its subject.
        let subject = claims["email"]
            .as_str()
            .filter(|e| !e.is_empty())
            .or_else(|| claims["sub"].as_str())
            .ok_or("no sub claim")?;
        if !crate::keys::verify_signature(&key, subject.as_bytes(), &proof) {
            return Err("the proof of possession doesn't verify".into());
        }
        // A workflow's token names its repository (GitHub) or its project (GitLab); a person's,
        // their email (M8's approvers).
        let identity = if claims["iss"] == GITHUB_ISSUER || claims.get("repository").is_some() {
            github_identity(&claims)?
        } else if claims.get("project_path").is_some() {
            gitlab_identity(&claims)?
        } else {
            CertIdentity {
                issuer: claims["iss"].as_str().ok_or("no iss claim")?.to_string(),
                subject: subject.to_string(),
                repository_id: None,
                repository: None,
                reference: None,
                workflow: None,
                signer_workflow: None,
                trigger: None,
            }
        };
        let leaf = self.signing_certificate(&key, &identity, now);
        let pem = |cert: &Cert| {
            der::pem::encode_string("CERTIFICATE", der::pem::LineEnding::LF, &cert.der)
                .expect("PEM")
        };
        Ok(
            json!({"signedCertificateEmbeddedSct": {"chain": {"certificates": [
                pem(&leaf), pem(&self.intermediate.cert), pem(&self.root.cert)
            ]}}}),
        )
    }
}

fn any<T: der::EncodeValue + der::Tagged>(value: &T) -> Any {
    Any::encode_from(value).expect("encodes")
}

/// The identity Fulcio certifies for GitHub Actions' claims (Fulcio's OID table).
pub fn github_identity(claims: &Value) -> Result<CertIdentity, String> {
    let claim = |name: &str| {
        claims[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("no {name} claim"))
    };
    let server = "https://github.com";
    let signer = format!("{server}/{}", claim("job_workflow_ref")?);
    Ok(CertIdentity {
        issuer: claim("iss")?,
        subject: signer.clone(),
        repository_id: Some(claim("repository_id")?),
        repository: Some(format!("{server}/{}", claim("repository")?)),
        reference: Some(claim("ref")?),
        workflow: Some(format!("{server}/{}", claim("workflow_ref")?)),
        signer_workflow: Some(signer),
        trigger: claims["event_name"].as_str().map(str::to_string),
    })
}

/// The identity Fulcio certifies for GitLab CI's claims: its `gitlab-pipeline` template, with
/// the instance's URL taken from `iss`.
pub fn gitlab_identity(claims: &Value) -> Result<CertIdentity, String> {
    let claim = |name: &str| {
        claims[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("no {name} claim"))
    };
    let url = claim("iss")?;
    let config = format!("https://{}", claim("ci_config_ref_uri")?);
    let kind = if claims["ref_type"] == "branch" {
        "heads"
    } else {
        "tags"
    };
    Ok(CertIdentity {
        issuer: url.clone(),
        subject: config.clone(),
        repository_id: Some(claim("project_id")?),
        repository: Some(format!("{url}/{}", claim("project_path")?)),
        reference: Some(format!("refs/{kind}/{}", claim("ref")?)),
        workflow: Some(config.clone()),
        signer_workflow: Some(config),
        trigger: claims["pipeline_source"].as_str().map(str::to_string),
    })
}

/// GitLab's issuer on gitlab.com.
pub const GITLAB_ISSUER: &str = "https://gitlab.com";

/// GitLab CI's claims for a pipeline of `project` (`group/name`, id `project_id`) on the branch
/// `branch`, with the token's `aud`, as an `id_tokens` entry declares it.
pub fn gitlab_claims(project: &str, project_id: &str, branch: &str, aud: &str) -> Value {
    let host = GITLAB_ISSUER.trim_start_matches("https://");
    json!({
        "iss": GITLAB_ISSUER,
        "aud": aud,
        "sub": format!("project_path:{project}:ref_type:branch:ref:{branch}"),
        "project_id": project_id,
        "project_path": project,
        "namespace_path": project.rsplit_once('/').map_or(project, |(group, _)| group),
        "pipeline_source": "push",
        "ref": branch,
        "ref_type": "branch",
        "ref_path": format!("refs/heads/{branch}"),
        "ref_protected": "true",
        "ci_config_ref_uri": format!("{host}/{project}//.gitlab-ci.yml@refs/heads/{branch}"),
        "exp": 4_102_444_800u64,
    })
}

/// GitHub Actions' claims for a workflow of `repository` (`owner/name`, id `repository_id`)
/// running `workflow` (a path) on `reference`, signing from `signer` (a reusable workflow's
/// `owner/repo/path@ref`, or the workflow itself).
pub fn github_claims(
    repository: &str,
    repository_id: &str,
    reference: &str,
    workflow: &str,
    signer: Option<&str>,
) -> Value {
    let workflow_ref = format!("{repository}/{workflow}@{reference}");
    json!({
        "iss": GITHUB_ISSUER,
        "aud": "sigstore",
        "sub": format!("repo:{repository}:ref:{reference}"),
        "repository": repository,
        "repository_id": repository_id,
        "repository_owner": repository.split('/').next().unwrap_or_default(),
        "ref": reference,
        "workflow_ref": workflow_ref,
        "job_workflow_ref": signer.map_or(workflow_ref.clone(), str::to_string),
        "event_name": "push",
        "exp": 4_102_444_800u64,
    })
}

/// An unsigned JWT with `claims`: the fakes never check token signatures.
pub fn jwt(claims: &Value) -> String {
    format!(
        "{}.{}.{}",
        B64URL.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        B64URL.encode(serde_json::to_vec(claims).expect("JSON")),
        B64URL.encode(b"not-a-signature")
    )
}

/// The claims of a JWT, unverified.
pub fn jwt_claims(token: &str) -> Result<Value, String> {
    let payload = token.split('.').nth(1).ok_or("not a JWT")?;
    let bytes = B64URL
        .decode(payload.trim_end_matches('='))
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// A P-256 test key from `testdata/keys/p256-<n>.pem` (n from 1 to 3), which OpenSSL wrote.
pub fn p256_key(n: u8) -> SigningKey {
    let pem = match n {
        1 => include_str!("../../../testdata/keys/p256-1.pem"),
        2 => include_str!("../../../testdata/keys/p256-2.pem"),
        3 => include_str!("../../../testdata/keys/p256-3.pem"),
        _ => panic!("test P-256 keys are 1, 2 and 3"),
    };
    SigningKey::from_pem(pem).expect("a test key")
}

/// A TUF repository that serves a trust root, with throwaway keys: what Sigstore's TUF
/// repository serves, with consistent snapshots.
pub struct TufRepo {
    root_keys: Vec<TestKey>,
    online: TestKey,
    roots: Vec<Vec<u8>>,
    expires: Time,
    /// Files by path, as a mirror serves them.
    pub files: BTreeMap<String, Vec<u8>>,
}

fn tuf_key(key: &TestKey) -> (String, Value) {
    let pem = der::pem::encode_string(
        "PUBLIC KEY",
        der::pem::LineEnding::LF,
        &key.public_key().encoded(),
    )
    .expect("PEM");
    let value =
        json!({"keytype": "ecdsa", "scheme": "ecdsa-sha2-nistp256", "keyval": {"public": pem}});
    let id = hex::encode(digest(&oid::SHA256, &canonical_json(&value)).expect("SHA-256"));
    (id, value)
}

fn tuf_sign(signed: &Value, keys: &[&TestKey]) -> Vec<u8> {
    let message = canonical_json(signed);
    let signatures: Vec<Value> = keys
        .iter()
        .map(|key| json!({"keyid": tuf_key(key).0, "sig": hex::encode(key.sign(&message))}))
        .collect();
    serde_json::to_vec_pretty(&json!({"signatures": signatures, "signed": signed})).expect("JSON")
}

impl TufRepo {
    /// A repository with root version 1 (two root keys, threshold 2) whose metadata expires at
    /// `expires`, serving `trusted_root`.
    pub fn new(trusted_root: &[u8], expires: Time) -> TufRepo {
        let mut repo = TufRepo {
            root_keys: vec![TestKey::generate(), TestKey::generate()],
            online: TestKey::generate(),
            roots: Vec::new(),
            expires,
            files: BTreeMap::new(),
        };
        repo.add_root(None);
        repo.publish(trusted_root, 1);
        repo
    }

    fn root_body(&self, version: u64) -> Value {
        let mut keys = serde_json::Map::new();
        for key in self.root_keys.iter().chain([&self.online]) {
            let (id, value) = tuf_key(key);
            keys.insert(id, value);
        }
        let ids = |keys: &[&TestKey]| keys.iter().map(|k| tuf_key(k).0).collect::<Vec<_>>();
        let root_ids = ids(&self.root_keys.iter().collect::<Vec<_>>());
        let online = ids(&[&self.online]);
        json!({
            "_type": "root", "spec_version": "1.0", "version": version,
            "expires": self.expires.to_string(), "consistent_snapshot": true,
            "keys": keys,
            "roles": {
                "root": {"keyids": root_ids, "threshold": 2},
                "timestamp": {"keyids": online, "threshold": 1},
                "snapshot": {"keyids": online, "threshold": 1},
                "targets": {"keyids": root_ids, "threshold": 2}
            }
        })
    }

    /// Adds the next root version. With `new_keys`, the root keys rotate to them: the old keys
    /// and the new ones both sign the new version.
    pub fn add_root(&mut self, new_keys: Option<Vec<TestKey>>) {
        let old: Vec<TestKey> = match new_keys {
            Some(keys) => std::mem::replace(&mut self.root_keys, keys),
            None => Vec::new(),
        };
        let version = self.roots.len() as u64 + 1;
        let body = self.root_body(version);
        let signers: Vec<&TestKey> = old.iter().chain(self.root_keys.iter()).collect();
        let bytes = tuf_sign(&body, &signers);
        self.files
            .insert(format!("{version}.root.json"), bytes.clone());
        self.roots.push(bytes);
    }

    /// Root version `n`.
    pub fn root(&self, n: u64) -> Vec<u8> {
        self.roots[usize::try_from(n - 1).expect("a version")].clone()
    }

    /// Publishes `trusted_root` as targets version `version`, with a new snapshot and
    /// timestamp.
    pub fn publish(&mut self, trusted_root: &[u8], version: u64) {
        let sha256 = hex::encode(digest(&oid::SHA256, trusted_root).expect("SHA-256"));
        let expires = self.expires.to_string();
        let targets = json!({
            "_type": "targets", "spec_version": "1.0", "version": version, "expires": expires,
            "targets": {"trusted_root.json": {"length": trusted_root.len(), "hashes": {"sha256": sha256}}}
        });
        let root_keys: Vec<&TestKey> = self.root_keys.iter().collect();
        let targets = tuf_sign(&targets, &root_keys);
        let snapshot = json!({
            "_type": "snapshot", "spec_version": "1.0", "version": version, "expires": expires,
            "meta": {"targets.json": {"version": version}}
        });
        let snapshot = tuf_sign(&snapshot, &[&self.online]);
        let timestamp = json!({
            "_type": "timestamp", "spec_version": "1.0", "version": version, "expires": expires,
            "meta": {"snapshot.json": {"version": version, "length": snapshot.len(),
                     "hashes": {"sha256": hex::encode(digest(&oid::SHA256, &snapshot).expect("SHA-256"))}}}
        });
        self.files.insert(
            format!("targets/{sha256}.trusted_root.json"),
            trusted_root.to_vec(),
        );
        self.files
            .insert(format!("{version}.targets.json"), targets);
        self.files
            .insert(format!("{version}.snapshot.json"), snapshot);
        self.files.insert(
            "timestamp.json".into(),
            tuf_sign(&timestamp, &[&self.online]),
        );
    }
}

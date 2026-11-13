use serde_json::{Value, json};

use super::*;

fn hex(n: u8) -> Hex32 {
    Hex32([n; 32])
}

fn round_trip<T>(value: &T, expected: Value)
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
{
    let written = serde_json::to_value(value).unwrap();
    assert_eq!(written, expected);
    let read: T = serde_json::from_value(expected).unwrap();
    assert_eq!(&read, value);
}

#[test]
fn hex32_is_64_lowercase_hex_characters_and_nothing_else() {
    let text = "00ff".repeat(16);
    let parsed = Hex32::parse(&text).unwrap();
    assert_eq!(parsed.0[0], 0x00);
    assert_eq!(parsed.0[1], 0xff);
    assert_eq!(parsed.to_string(), text);
    for bad in [
        "00FF".repeat(16),
        "00ff".repeat(15),
        format!("{}0", "00ff".repeat(16)),
        format!("{}zz", "00".repeat(31)),
        String::new(),
    ] {
        assert!(Hex32::parse(&bad).is_none(), "{bad}");
        assert!(
            serde_json::from_value::<Hex32>(json!(bad)).is_err(),
            "{bad}"
        );
    }
    assert!(serde_json::from_value::<Hex32>(json!(7)).is_err());
    assert_eq!(
        Hex32::sha256(b"").to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn base64_is_standard_and_padded() {
    round_trip(&Base64(b"hint".to_vec()), json!("aGludA=="));
    assert!(serde_json::from_value::<Base64>(json!("aGludA")).is_err());
    assert!(serde_json::from_value::<Base64>(json!("a_b-")).is_err());
}

#[test]
fn kinds_and_platforms() {
    for good in [
        "rustc/1",
        "go/12",
        "c-cxx/9999",
        "a/0",
        "abcdefghijklmnop/1",
    ] {
        assert!(is_kind(good), "{good}");
    }
    for bad in [
        "rustc",
        "Rustc/1",
        "1rustc/1",
        "rustc/",
        "rustc/12345",
        "rustc/1/2",
        "abcdefghijklmnopq/1",
        "rust c/1",
        "",
    ] {
        assert!(!is_kind(bad), "{bad}");
    }
    assert!(is_platform("x86_64-unknown-linux-gnu"));
    assert!(is_platform("aarch64-apple-darwin"));
    assert!(is_platform("thumbv7em-none-eabihf"));
    assert!(!is_platform(""));
    assert!(!is_platform("X86_64-unknown-linux-gnu"));
    assert!(!is_platform("x86_64 linux"));
    assert!(!is_platform(&"a".repeat(65)));
}

#[test]
fn class_words() {
    for class in [
        Class::Library,
        Class::ProcMacro,
        Class::BuildScript,
        Class::Program,
        Class::Other,
    ] {
        assert_eq!(Class::from_word(class.word()), Some(class));
        assert_eq!(serde_json::to_value(class).unwrap(), json!(class.word()));
    }
    assert_eq!(Class::from_word("dylib"), None);
    assert_eq!(Class::from_word("unknown"), None);
    // A newer side's word reads as unknown, either way, and doesn't break what holds it.
    assert_eq!(
        serde_json::from_value::<Class>(json!("dylib")).unwrap(),
        Class::Unknown
    );
    let found: LookupResponse = serde_json::from_value(json!({"found": [{
        "address": "01".repeat(32), "scope": "protected", "bytes": 1,
        "packSha256": "01".repeat(32), "outputs": "01".repeat(32), "costMs": 1, "class": "cdylib",
    }]}))
    .unwrap();
    assert_eq!(found.found[0].class, Class::Unknown);
    let begun: BeginRequest = serde_json::from_value(json!({"kind": "rustc/1", "blocks": [{
        "address": "01".repeat(32), "bytes": 1, "packSha256": "01".repeat(32),
        "outputs": "01".repeat(32), "class": "cdylib", "costMs": 1,
    }]}))
    .unwrap();
    assert_eq!(begun.blocks[0].class, Class::Unknown);
}

#[test]
fn the_begin_signature_names_the_org_and_the_body() {
    let message = begin_message("acme", b"{}");
    assert_eq!(
        message,
        "hushrun-build-begin/v1\nacme\n44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\n"
    );
    assert_ne!(begin_message("other", b"{}"), message);
    assert_ne!(begin_message("acme", b"{ }"), message);
}

#[test]
fn open_and_its_policy() {
    round_trip(
        &OpenRequest {
            kind: "rustc/1".into(),
            repository: Some("github.com/acme/shop".into()),
            commit: Some("ab".repeat(20)),
            check: Some(hex(1)),
            platform: "x86_64-unknown-linux-gnu".into(),
        },
        json!({
            "kind": "rustc/1",
            "repository": "github.com/acme/shop",
            "commit": "ab".repeat(20),
            "check": "01".repeat(32),
            "platform": "x86_64-unknown-linux-gnu",
        }),
    );
    round_trip(
        &OpenRequest {
            kind: "rustc/1".into(),
            repository: None,
            commit: None,
            check: None,
            platform: "aarch64-apple-darwin".into(),
        },
        json!({"kind": "rustc/1", "platform": "aarch64-apple-darwin"}),
    );
    let policy = Policy {
        sharing: true,
        reason: None,
        mode: Some(Mode::Observe),
        writes: true,
        untrusted: None,
        write: Some("ci/42/0123456789abcdef0123456789abcdef".into()),
        read: vec![
            "protected/42".into(),
            "ci/42/0123456789abcdef0123456789abcdef".into(),
        ],
        kept_days: 30,
        limits: Limits::default(),
        allowance_bytes: 10_000_000_000,
        held_bytes: 123,
        manifest: vec![Pair(hex(2), hex(3))],
    };
    round_trip(
        &policy,
        json!({
            "sharing": true,
            "mode": "observe",
            "writes": true,
            "write": "ci/42/0123456789abcdef0123456789abcdef",
            "read": ["protected/42", "ci/42/0123456789abcdef0123456789abcdef"],
            "keptDays": 30,
            "limits": {
                "lookup": 500, "begin": 200, "hintsSent": 200, "hintsSentBytes": 1048576,
                "hintBytes": 262144, "packBytes": 2147483648u64, "manifest": 16384,
            },
            "allowanceBytes": 10_000_000_000u64,
            "heldBytes": 123,
            "manifest": [["02".repeat(32), "03".repeat(32)]],
        }),
    );
    // A job on a listed branch that isn't trusted is told why, in a word.
    let untrusted: Policy = serde_json::from_value(json!({
        "sharing": true, "mode": "on", "writes": false, "untrusted": "commit",
        "read": ["protected/42"], "keptDays": 30, "allowanceBytes": 1, "heldBytes": 0,
    }))
    .unwrap();
    assert_eq!(untrusted.untrusted, Some(Untrusted::Commit));
    assert!(!untrusted.writes);
    for (word, why) in [
        ("repository", Untrusted::Repository),
        ("event", Untrusted::Event),
        ("environment", Untrusted::Environment),
        ("runner", Untrusted::Runner),
        ("run", Untrusted::Run),
        ("a-new-reason", Untrusted::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<Untrusted>(json!(word)).unwrap(),
            why
        );
    }
    // Off for the plan, from a newer server that says more than this version knows.
    let off: Policy = serde_json::from_value(json!({
        "sharing": false, "reason": "plan", "keptDays": 30, "allowanceBytes": 0,
        "heldBytes": 0, "somethingNew": {"x": 1},
    }))
    .unwrap();
    assert!(!off.sharing);
    assert_eq!(off.reason, Some(OffReason::Plan));
    assert_eq!(off.write, None);
    assert_eq!(off.limits, Limits::default());
    let newer: Policy = serde_json::from_value(json!({
        "sharing": false, "reason": "a-new-reason", "mode": "a-new-mode", "keptDays": 30,
        "allowanceBytes": 0, "heldBytes": 0,
    }))
    .unwrap();
    assert_eq!(newer.reason, Some(OffReason::Unknown));
    assert_eq!(newer.mode, Some(Mode::Unknown));
    // A plan whose shared storage only a contract sets, before one does.
    let contract: Policy = serde_json::from_value(json!({
        "sharing": false, "reason": "contract", "keptDays": 30, "allowanceBytes": 0,
        "heldBytes": 0,
    }))
    .unwrap();
    assert_eq!(contract.reason, Some(OffReason::Contract));
    assert_eq!(
        serde_json::to_value(OffReason::Contract).unwrap(),
        json!("contract")
    );
}

#[test]
fn hints() {
    round_trip(
        &HintsLookupRequest {
            kind: "rustc/1".into(),
            addresses: vec![hex(4)],
            repository: Some("github.com/acme/shop".into()),
            commit: Some("a".repeat(40)),
        },
        json!({"kind": "rustc/1", "addresses": ["04".repeat(32)],
               "repository": "github.com/acme/shop", "commit": "a".repeat(40)}),
    );
    round_trip(
        &HintsLookupResponse {
            hints: vec![FoundHints {
                address: hex(4),
                lists: vec![Base64(vec![1, 2, 3]), Base64(vec![4])],
            }],
        },
        json!({"hints": [{"address": "04".repeat(32), "lists": ["AQID", "BA=="]}]}),
    );
    round_trip(
        &HintsRequest {
            kind: "rustc/1".into(),
            hints: vec![SentHint {
                address: hex(5),
                list: Base64(vec![9]),
            }],
            commit: None,
        },
        json!({"kind": "rustc/1", "hints": [{"address": "05".repeat(32), "list": "CQ=="}]}),
    );
    round_trip(&HintsResponse { stored: 1 }, json!({"stored": 1}));
}

#[test]
fn lookup() {
    round_trip(
        &LookupRequest {
            kind: "rustc/1".into(),
            addresses: vec![hex(6)],
            fetch: true,
            repository: None,
            machine: Some("sha256:ab".into()),
            commit: Some("b".repeat(40)),
        },
        json!({"kind": "rustc/1", "addresses": ["06".repeat(32)], "fetch": true,
               "machine": "sha256:ab", "commit": "b".repeat(40)}),
    );
    // A lookup from a version before the commit reads with none.
    let older: LookupRequest =
        serde_json::from_value(json!({"kind": "rustc/1", "addresses": [], "fetch": true})).unwrap();
    assert_eq!(older.commit, None);
    // `fetch` is false unless said.
    let quiet: LookupRequest =
        serde_json::from_value(json!({"kind": "rustc/1", "addresses": []})).unwrap();
    assert!(!quiet.fetch);
    let link = Link {
        method: "GET".into(),
        url: "https://store.example.com/bucket/v1/x".into(),
        headers: BTreeMap::new(),
        expires_in_seconds: 600,
    };
    round_trip(
        &LookupResponse {
            found: vec![Found {
                address: hex(6),
                scope: "protected".into(),
                bytes: 4096,
                pack_sha256: hex(7),
                outputs: hex(8),
                cost_ms: 1500,
                class: Class::ProcMacro,
                mine: true,
                get: Some(link),
            }],
            suspended: vec![hex(9)],
        },
        json!({
            "found": [{
                "address": "06".repeat(32), "scope": "protected", "bytes": 4096,
                "packSha256": "07".repeat(32), "outputs": "08".repeat(32), "costMs": 1500,
                "class": "proc-macro", "mine": true,
                "get": {
                    "method": "GET", "url": "https://store.example.com/bucket/v1/x",
                    "headers": {}, "expiresInSeconds": 600,
                },
            }],
            "suspended": ["09".repeat(32)],
        }),
    );
}

#[test]
fn begin_and_its_answers() {
    round_trip(
        &BeginRequest {
            kind: "rustc/1".into(),
            blocks: vec![BeginBlock {
                address: hex(10),
                hint_address: Some(hex(11)),
                bytes: 1,
                pack_sha256: hex(12),
                outputs: hex(13),
                class: Class::Library,
                cost_ms: 0,
            }],
            repository: Some("github.com/acme/shop".into()),
            commit: None,
        },
        json!({
            "kind": "rustc/1",
            "repository": "github.com/acme/shop",
            "blocks": [{
                "address": "0a".repeat(32), "hintAddress": "0b".repeat(32), "bytes": 1,
                "packSha256": "0c".repeat(32), "outputs": "0d".repeat(32), "class": "library",
                "costMs": 0,
            }],
        }),
    );
    let mut headers = BTreeMap::new();
    headers.insert("content-length".to_string(), "1".to_string());
    headers.insert("if-none-match".to_string(), "*".to_string());
    round_trip(
        &BeginResponse {
            blocks: vec![
                BeginAnswer {
                    address: hex(10),
                    answer: Answer::Put {
                        link: Link {
                            method: "PUT".into(),
                            url: "https://store.example.com/b/v1/y".into(),
                            headers,
                            expires_in_seconds: 900,
                        },
                    },
                },
                BeginAnswer {
                    address: hex(14),
                    answer: Answer::Have {
                        scope: "protected".into(),
                    },
                },
                BeginAnswer {
                    address: hex(15),
                    answer: Answer::Refused {
                        reason: Refusal::OverAllowance,
                    },
                },
            ],
        },
        json!({"blocks": [
            {"address": "0a".repeat(32), "answer": "put", "link": {
                "method": "PUT", "url": "https://store.example.com/b/v1/y",
                "headers": {"content-length": "1", "if-none-match": "*"},
                "expiresInSeconds": 900,
            }},
            {"address": "0e".repeat(32), "answer": "have", "scope": "protected"},
            {"address": "0f".repeat(32), "answer": "refused", "reason": "over_allowance"},
        ]}),
    );
    for (word, reason) in [
        ("too_large", Refusal::TooLarge),
        ("suspended", Refusal::Suspended),
        ("differs", Refusal::Differs),
        ("busy", Refusal::Busy),
        ("no_scope", Refusal::NoScope),
        ("too_many_uploads", Refusal::TooManyUploads),
        ("unsupported", Refusal::Unsupported),
        ("from_the_future", Refusal::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<Refusal>(json!(word)).unwrap(),
            reason
        );
    }
    // An answer a newer server gives that this version doesn't know.
    let newer: BeginAnswer =
        serde_json::from_value(json!({"address": "0a".repeat(32), "answer": "later", "x": 1}))
            .unwrap();
    assert_eq!(newer.answer, Answer::Unknown);
}

#[test]
fn commit() {
    round_trip(
        &CommitRequest {
            kind: "rustc/1".into(),
            addresses: vec![hex(16)],
            commit: None,
        },
        json!({"kind": "rustc/1", "addresses": ["10".repeat(32)]}),
    );
    round_trip(
        &CommitResponse {
            blocks: vec![
                Committed {
                    address: hex(16),
                    state: CommitState::Stored,
                },
                Committed {
                    address: hex(17),
                    state: CommitState::Missing,
                },
                Committed {
                    address: hex(18),
                    state: CommitState::Differs,
                },
                Committed {
                    address: hex(19),
                    state: CommitState::Gone,
                },
            ],
        },
        json!({"blocks": [
            {"address": "10".repeat(32), "state": "stored"},
            {"address": "11".repeat(32), "state": "missing"},
            {"address": "12".repeat(32), "state": "differs"},
            {"address": "13".repeat(32), "state": "gone"},
        ]}),
    );
    assert_eq!(
        serde_json::from_value::<CommitState>(json!("evicted")).unwrap(),
        CommitState::Unknown
    );
}

#[test]
fn close_and_report() {
    round_trip(
        &CloseRequest {
            kind: "rustc/1".into(),
            run: hex(23),
            repository: None,
            check: Some(hex(20)),
            platform: "x86_64-unknown-linux-gnu".into(),
            commit: None,
            manifest: vec![Pair(hex(21), hex(24))],
            counts: Counts {
                reused_local: 1,
                reused_shared: 2,
                built: 3,
                uploaded: 4,
                bytes_down: 5,
                bytes_up: 6,
                saved_ms: 7,
                unused_downloads: 8,
                compared: 9,
                compared_others: 10,
                compared_differed: 11,
            },
        },
        json!({
            "kind": "rustc/1", "run": "17".repeat(32), "check": "14".repeat(32),
            "platform": "x86_64-unknown-linux-gnu",
            "manifest": [["15".repeat(32), "18".repeat(32)]],
            "counts": {
                "reusedLocal": 1, "reusedShared": 2, "built": 3, "uploaded": 4, "bytesDown": 5,
                "bytesUp": 6, "savedMs": 7, "unusedDownloads": 8, "compared": 9,
                "comparedOthers": 10, "comparedDiffered": 11,
            },
        }),
    );
    // Counts a client doesn't send are zero.
    let sparse: CloseRequest = serde_json::from_value(json!({
        "kind": "rustc/1", "run": "17".repeat(32), "platform": "aarch64-apple-darwin",
        "counts": {"built": 2},
    }))
    .unwrap();
    assert_eq!(sparse.counts.built, 2);
    assert_eq!(sparse.counts.reused_shared, 0);
    // A close from a client from before `comparedDiffered` reads it as 0.
    assert_eq!(sparse.counts.compared_differed, 0);
    assert!(sparse.manifest.is_empty());
    round_trip(
        &ReportRequest {
            kind: "rustc/1".into(),
            address: hex(22),
            scope: "protected/42".into(),
            problem: Problem::Rejected,
            repository: None,
            commit: Some("cd".repeat(20)),
        },
        json!({"kind": "rustc/1", "address": "16".repeat(32), "scope": "protected/42",
               "problem": "rejected", "commit": "cd".repeat(20)}),
    );
    assert_eq!(
        serde_json::from_value::<Problem>(json!("rejected")).unwrap(),
        Problem::Rejected
    );
    // A problem a newer client sends reads as unknown: the server records nothing for it.
    assert_eq!(
        serde_json::from_value::<Problem>(json!("odd")).unwrap(),
        Problem::Unknown
    );
}

// covers: B-28. The one definition of a block's digest of outputs: JCS of the files sorted by name
#[test]
fn the_digest_of_outputs() {
    let a = OutputFile {
        name: "lib/libfoo.rlib",
        size: 1234,
        sha256: hex(1),
    };
    let b = OutputFile {
        name: "lib/foo.d",
        size: 7,
        sha256: hex(2),
    };
    // The order the files come in doesn't matter: they are sorted by name.
    assert_eq!(outputs_digest(&[a, b]), outputs_digest(&[b, a]));
    // Exactly the JCS bytes of the sorted list.
    let expected = format!(
        "[{{\"name\":\"lib/foo.d\",\"sha256\":\"{}\",\"size\":7}},{{\"name\":\"lib/libfoo.rlib\",\"sha256\":\"{}\",\"size\":1234}}]",
        "02".repeat(32),
        "01".repeat(32)
    );
    assert_eq!(outputs_digest(&[a, b]), Hex32::sha256(expected.as_bytes()));
    // A name, a size or a hash that differs makes another digest.
    let other_size = OutputFile { size: 8, ..b };
    assert_ne!(outputs_digest(&[a, other_size]), outputs_digest(&[a, b]));
    let other_name = OutputFile {
        name: "lib/bar.d",
        ..b
    };
    assert_ne!(outputs_digest(&[a, other_name]), outputs_digest(&[a, b]));
    // No files is a digest too: of `[]`.
    assert_eq!(outputs_digest(&[]), Hex32::sha256(b"[]"));
}

#[test]
fn commits() {
    assert!(is_commit(&"a".repeat(40)));
    assert!(is_commit(&"0".repeat(64)));
    assert!(!is_commit(&"a".repeat(39)));
    assert!(!is_commit(&"A".repeat(40)));
    assert!(!is_commit(""));
}

// covers: B-27
#[test]
fn an_address_is_the_kind_and_the_key_under_its_schema() {
    let key = [7u8; 32];
    let mut expected = Vec::new();
    expected.extend_from_slice(b"hushrun.block-address/v1\nrustc/1\n");
    expected.extend_from_slice(&key);
    assert_eq!(address("rustc/1", &key), Hex32::sha256(&expected));
    assert_ne!(address("rustc/1", &key), address("rustc/2", &key));
    assert_ne!(address("rustc/1", &key), address("rustc/1", &[8u8; 32]));
    // The key's own SHA-256 is not its address: the address is taken under its schema.
    assert_ne!(address("rustc/1", &key), Hex32::sha256(&key));
}

// covers: B-31
#[test]
fn only_a_caller_writing_a_trusted_scope_writes_trusted_blocks() {
    let policy = |write: Option<&str>| -> Policy {
        serde_json::from_value(json!({
            "sharing": true, "mode": "on", "writes": write.is_some(), "write": write,
            "keptDays": 30, "allowanceBytes": 1, "heldBytes": 0
        }))
        .unwrap()
    };
    assert!(policy(Some("protected/9001")).writes_trusted());
    assert!(!policy(Some("ci/9001/pr7")).writes_trusted());
    assert!(!policy(Some("person/1")).writes_trusted());
    assert!(!policy(None).writes_trusted());
}

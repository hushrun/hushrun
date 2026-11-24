use super::*;

/// Entries Gradle 8.14.3 wrote for `compileJava` (testdata/gradle): one plain, and one whose
/// members have names longer than a ustar header holds, which Gradle gives in PAX headers.
const COMPILE_JAVA: &[u8] = include_bytes!("../../../../../testdata/gradle/compile-java.entry");
const COMPILE_JAVA_KEY: &str = "49b7acd36407da65ca900b05e84a6241";
const LONG_NAMES: &[u8] = include_bytes!("../../../../../testdata/gradle/long-names.entry");
const LONG_NAMES_KEY: &str = "c27db382b318167fb28bbb46a191a577";

fn member(name: &str, bytes: &[u8]) -> Member {
    Member {
        name: name.into(),
        folder: false,
        mode: 0o644,
        bytes: bytes.to_vec(),
    }
}

fn metadata(key: &str, execution: u64, invocation: &str) -> Member {
    member(
        METADATA,
        format!(
            "#Generated origin information\n#Tue Nov 24 00:21:57 UTC 2026\nbuildCacheKey={key}\nbuildInvocationId={invocation}\ncreationTime=1795479717607\nexecutionTime={execution}\ngradleVersion=8.14.3\nidentity=\\:app\\:compileJava\ntype=org.gradle.api.internal.tasks.execution.TaskExecution\n"
        )
        .as_bytes(),
    )
}

/// A tar as Gradle writes one, through the canonical writer with dates added: what a second
/// execution of a task gives.
fn tar_of(members: &[Member], mtime: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for m in members {
        let start = out.len();
        write_member(&mut out, m);
        // Date every header, as Gradle does, and fix its checksum.
        let mut at = start;
        while at < out.len() {
            let size = octal(&out[at + 124..at + 136]).unwrap() as usize;
            put_octal(&mut out[at + 136..at + 148], mtime);
            out[at + 148..at + 156].copy_from_slice(b"        ");
            let sum: u32 = out[at..at + BLOCK].iter().map(|b| u32::from(*b)).sum();
            out[at + 148..at + 156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            at += BLOCK + size.div_ceil(BLOCK) * BLOCK;
        }
    }
    out.extend_from_slice(&[0u8; 2 * BLOCK]);
    out
}

// covers: B-78
#[test]
fn gradle_s_own_entries_are_read_and_their_metadata_made_canonical() {
    let checked = Checked::from_gradle(COMPILE_JAVA, COMPILE_JAVA_KEY).unwrap();
    assert_eq!(checked.identity, ":compileJava");
    assert_eq!(checked.gradle_version, "8.14.3");
    assert!(checked.execution_ms > 0);
    let names: Vec<&str> = checked.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names[0], METADATA);
    assert!(names.contains(&"tree-destinationDirectory/demo/App.class"));
    let meta = String::from_utf8(checked.members[0].bytes.clone()).unwrap();
    assert!(meta.contains(&format!("buildInvocationId={INVOCATION}\n")));
    assert!(meta.contains("creationTime=0\nexecutionTime=0\n"));
    assert!(!meta.contains('#'));

    let long = Checked::from_gradle(LONG_NAMES, LONG_NAMES_KEY).unwrap();
    let longest = long.members.iter().map(|m| m.name.len()).max().unwrap();
    assert!(longest > NAME_FIELD, "{longest}");
    assert!(long.members.iter().any(|m| {
        m.name
            .ends_with("anotherlevelofnesting/AnExtremelyLongClassNameForTesting.class")
            && !m.folder
    }));

    // Asked under another key, the entry is refused.
    assert_eq!(
        Checked::from_gradle(COMPILE_JAVA, LONG_NAMES_KEY),
        Err(Refused::OtherKey)
    );
}

// covers: B-78
#[test]
fn the_canonical_form_reads_back_as_itself_and_gradle_s_gzip_holds_it() {
    for (bytes, key) in [
        (COMPILE_JAVA, COMPILE_JAVA_KEY),
        (LONG_NAMES, LONG_NAMES_KEY),
    ] {
        let checked = Checked::from_gradle(bytes, key).unwrap();
        let canonical = checked.canonical();
        let again = Checked::from_tar(&canonical, key).unwrap();
        assert_eq!(again.members, checked.members);
        assert_eq!(again.canonical(), canonical);
        let served = to_gradle(&canonical);
        assert_eq!(gunzip(&served, MOST_UNPACKED).unwrap(), canonical);
        assert_eq!(
            Checked::from_gradle(&served, key).unwrap().canonical(),
            canonical
        );
    }
    // A tar too large for one stored block, and an empty one, gzip as well.
    let big = vec![7u8; 200_000];
    assert_eq!(gunzip(&to_gradle(&big), u64::MAX).unwrap(), big);
    assert!(gunzip(&to_gradle(&[]), u64::MAX).unwrap().is_empty());
}

// covers: B-78
#[test]
fn two_executions_with_the_same_outputs_make_the_same_canonical_entry() {
    let key = "ab".repeat(16);
    let outputs = [
        Member {
            name: "tree-destinationDirectory".into(),
            folder: true,
            mode: 0o755,
            bytes: Vec::new(),
        },
        member("tree-destinationDirectory/A.class", b"\xca\xfe\xba\xbe"),
    ];
    let first: Vec<Member> = std::iter::once(metadata(&key, 583, "hqflb7szezhv5ob52isyqwtfae"))
        .chain(outputs.iter().cloned())
        .collect();
    let second: Vec<Member> = std::iter::once(metadata(&key, 27, "hoyvcgh4pffmrc5hxea3xtsf5q"))
        .chain(outputs.iter().cloned())
        .collect();
    let one = Checked::from_tar(&tar_of(&first, 1_795_479_717), &key).unwrap();
    let two = Checked::from_tar(&tar_of(&second, 1_795_479_944), &key).unwrap();
    assert_eq!(one.execution_ms, 583);
    assert_eq!(two.execution_ms, 27);
    assert_eq!(one.identity, ":app:compileJava");
    assert_eq!(one.canonical(), two.canonical());
    // Other outputs, another canonical entry.
    let mut third = second.clone();
    third[2].bytes = b"\xca\xfe\xba\xbf".to_vec();
    let three = Checked::from_tar(&tar_of(&third, 0), &key).unwrap();
    assert_ne!(three.canonical(), one.canonical());
}

// covers: B-78
#[test]
fn an_entry_that_isn_t_one_is_refused() {
    let key = "cd".repeat(16);
    let good = vec![metadata(&key, 1, INVOCATION), member("tree-out/x", b"x")];
    assert!(Checked::from_tar(&tar_of(&good, 0), &key).is_ok());

    for name in [
        "/etc/passwd",
        "tree-out/../../x",
        "tree-out/./x",
        "a\\b",
        "tree-out//x",
    ] {
        let members = vec![metadata(&key, 1, INVOCATION), member(name, b"x")];
        assert!(
            matches!(
                Checked::from_tar(&tar_of(&members, 0), &key),
                Err(Refused::Name(_))
            ),
            "{name}"
        );
    }
    // No METADATA, or two.
    let none = vec![member("tree-out/x", b"x")];
    assert_eq!(
        Checked::from_tar(&tar_of(&none, 0), &key),
        Err(Refused::Metadata)
    );
    let two = vec![metadata(&key, 1, INVOCATION), metadata(&key, 1, INVOCATION)];
    assert_eq!(
        Checked::from_tar(&tar_of(&two, 0), &key),
        Err(Refused::Metadata)
    );
    // A link, which Gradle never writes.
    let mut tar = tar_of(&good, 0);
    let second = BLOCK + metadata(&key, 1, INVOCATION).bytes.len().div_ceil(BLOCK) * BLOCK;
    tar[second + 156] = b'2';
    tar[second + 148..second + 156].copy_from_slice(b"        ");
    let sum: u32 = tar[second..second + BLOCK]
        .iter()
        .map(|b| u32::from(*b))
        .sum();
    tar[second + 148..second + 156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert_eq!(Checked::from_tar(&tar, &key), Err(Refused::MemberType));
    // A header whose checksum fails, and a tar cut short.
    let mut bad = tar_of(&good, 0);
    bad[0] ^= 1;
    assert_eq!(Checked::from_tar(&bad, &key), Err(Refused::Tar));
    let cut = tar_of(&good, 0);
    assert_eq!(
        Checked::from_tar(&cut[..BLOCK + 10], &key),
        Err(Refused::Tar)
    );
    // Not gzip, a gzip stream cut short, one with bytes after its trailer, and one that unpacks
    // past the limit.
    assert_eq!(
        Checked::from_gradle(b"not gzip at all...", &key),
        Err(Refused::Gzip)
    );
    let gz = to_gradle(&tar_of(&good, 0));
    assert_eq!(
        Checked::from_gradle(&gz[..gz.len() - 3], &key),
        Err(Refused::Inflate)
    );
    let mut trailing = gz.clone();
    trailing.push(0);
    assert_eq!(Checked::from_gradle(&trailing, &key), Err(Refused::Inflate));
    let mut wrong_crc = gz.clone();
    let n = wrong_crc.len();
    wrong_crc[n - 8] ^= 1;
    assert_eq!(
        Checked::from_gradle(&wrong_crc, &key),
        Err(Refused::Inflate)
    );
    assert_eq!(
        gunzip(&to_gradle(&vec![0u8; 4096]), 1024),
        Err(Refused::TooLarge(1024))
    );
}

// covers: B-79
#[test]
fn outputs_are_searched_for_folders_metadata_aside() {
    let key = "ef".repeat(16);
    let members = vec![
        metadata(&key, 1, INVOCATION),
        member(
            "tree-out/report.xml",
            b"<file path=\"/home/ana/shop/src/A.kt\"/>",
        ),
    ];
    let checked = Checked::from_tar(&tar_of(&members, 0), &key).unwrap();
    assert!(checked.outputs_hold(b"/home/ana/shop"));
    assert!(checked.outputs_hold(b"tree-out"));
    assert!(!checked.outputs_hold(b"/home/bob"));
    assert!(!checked.outputs_hold(b"buildCacheKey"));
    assert!(!checked.outputs_hold(b""));
}

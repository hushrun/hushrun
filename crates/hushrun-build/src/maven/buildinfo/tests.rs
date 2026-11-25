use super::*;

/// What Apache's build cache extension 1.2.0 on Maven 3.9.11 wrote for a module `demo:core`
/// whose `attachedOutputs` keep `target/classes`.
const BUILDINFO: &str = include_str!("../../../../../testdata/maven/core.buildinfo.xml");

fn module() -> Module {
    Module {
        group: "demo".into(),
        artifact: "core".into(),
        checksum: "07b43d4938bbe3e1".into(),
    }
}

// covers: B-172
#[test]
fn the_extension_s_buildinfo_names_its_files_and_its_canonical_form_fixes_what_changes() {
    let checked = check(BUILDINFO.as_bytes(), &module()).unwrap();
    assert_eq!(
        checked.files,
        vec![
            Named {
                name: "core.jar".into(),
                size: 1544
            },
            Named {
                name: "core-mvn-cache-ext-extra-output-1.zip".into(),
                size: 364
            },
        ]
    );
    assert_eq!(checked.hash_function, "XX");
    let canonical = String::from_utf8(checked.canonical.clone()).unwrap();
    assert!(canonical.contains("<buildTime>1970-01-01T00:00:00.000</buildTime>"));
    assert!(canonical.contains("<buildServer>hushrun</buildServer>"));
    assert!(!checked.is_canonical(BUILDINFO.as_bytes()));
    // The rest is as the extension wrote it.
    assert_eq!(
        canonical.len() + "2026-11-24T05:38:36.070".len() + "localhost".len(),
        BUILDINFO.len() + BUILD_TIME.len() + BUILD_SERVER.len()
    );
    // Another execution's time and host make the same canonical form, which is canonical.
    let other = BUILDINFO
        .replace("2026-11-24T05:38:36.070", "2027-01-02T03:04:05.678")
        .replace("localhost", "ci-runner-7");
    let again = check(other.as_bytes(), &module()).unwrap();
    assert_eq!(again.canonical, checked.canonical);
    assert!(
        check(&checked.canonical, &module())
            .unwrap()
            .is_canonical(&checked.canonical)
    );
}

// covers: B-172
#[test]
fn a_buildinfo_of_another_module_or_with_a_file_it_can_t_hold_is_refused() {
    let refused = |text: &str, module: &Module| check(text.as_bytes(), module).unwrap_err();
    assert!(
        refused(
            BUILDINFO,
            &Module {
                checksum: "07b43d4938bbe3e2".into(),
                ..module()
            }
        )
        .contains("another checksum")
    );
    assert!(
        refused(
            BUILDINFO,
            &Module {
                artifact: "app".into(),
                ..module()
            }
        )
        .contains("another module")
    );
    for (from, to, why) in [
        (
            "<fileName>core.jar</fileName>",
            "<fileName>../core.jar</fileName>",
            "names the file",
        ),
        (
            "<fileName>core.jar</fileName>",
            "<fileName>buildinfo.xml</fileName>",
            "names the file",
        ),
        (
            "<fileName>core.jar</fileName>",
            "<fileName>core-mvn-cache-ext-extra-output-1.zip</fileName>",
            "twice",
        ),
        (
            "<fileSize>1544</fileSize>",
            "<fileSize>-1</fileSize>",
            "no size",
        ),
        ("<fileSize>1544</fileSize>", "", "no size"),
        (
            "<cacheImplementationVersion>v1.1</cacheImplementationVersion>",
            "<cacheImplementationVersion>v1</cacheImplementationVersion>",
            "layout",
        ),
        ("</build>", "", "ends before"),
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<?xml version=\"1.0\"?><!DOCTYPE build [<!ENTITY x \"y\">]>",
            "DTD",
        ),
        ("</build>", "</build><build/>", "no one build"),
        (
            "<fileName>core.jar</fileName>",
            "<fileName>core&amp;.jar</fileName>",
            "plain text",
        ),
    ] {
        assert!(BUILDINFO.contains(from), "{from}");
        let why_refused = refused(&BUILDINFO.replacen(from, to, 1), &module());
        assert!(why_refused.contains(why), "{to}: {why_refused}");
    }
    assert!(check(b"\xff\xfe", &module()).unwrap_err().contains("UTF-8"));
    let too_many: String = (0..=MOST_FILES)
        .map(|n| {
            format!(
                "<attachedArtifact><fileName>f{n}.zip</fileName><fileSize>1</fileSize></attachedArtifact>"
            )
        })
        .collect();
    let crowded = BUILDINFO.replacen(
        "<attachedArtifacts>",
        &format!("<attachedArtifacts>{too_many}"),
        1,
    );
    assert!(refused(&crowded, &module()).contains("more than"));
}

// covers: B-172
#[test]
fn a_module_with_no_file_names_none() {
    let start = BUILDINFO.find("  <artifact>").unwrap();
    let end = BUILDINFO.find("  </attachedArtifacts>").unwrap() + "  </attachedArtifacts>".len();
    let pom = format!(
        "{}  <artifact>\n    <groupId>demo</groupId>\n    <artifactId>core</artifactId>\n    <type>pom</type>\n  </artifact>{}",
        &BUILDINFO[..start],
        &BUILDINFO[end..]
    );
    assert!(check(pom.as_bytes(), &module()).unwrap().files.is_empty());
}

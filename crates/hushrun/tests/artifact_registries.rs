//! `artifact resolve`, `copy` and `digest`, and tags in `attest`, against real registries
//! (`--features it`). `HUSHRUN_IT_REGISTRIES` names two registry hosts reached over plain
//! HTTP: the image's location is on the first, and `copy` copies to the second.
#![cfg(feature = "it")]

use std::time::{SystemTime, UNIX_EPOCH};

use hushrun_core::digest::Digest;
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use serde_json::Value;

/// The machine that validates and deploys, and the one that deploys again later.
const WRITER: u8 = 1;
const LATER: u8 = 2;

fn hosts() -> Option<(String, String)> {
    let Ok(text) = std::env::var("HUSHRUN_IT_REGISTRIES") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "HUSHRUN_IT_REGISTRIES must be set in CI"
        );
        return None;
    };
    let mut hosts = text.split_whitespace().map(str::to_string);
    let a = hosts.next().expect("two hosts");
    let b = hosts.next().unwrap_or_else(|| a.clone());
    Some((a, b))
}

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn ok(cmd: &mut assert_cmd::Command) -> Vec<u8> {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Pushes a one-layer image to `host/path:tag` and returns its digest.
fn seed(host: &str, path: &str, tag: &str) -> Digest {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::Http,
        ..ClientConfig::default()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let blob = |bytes: &'static [u8]| {
            let digest = Digest::of(bytes);
            (bytes, digest)
        };
        let mut descriptors = Vec::new();
        for (bytes, digest) in [blob(b"{\"os\":\"linux\"}"), blob(b"the layer")] {
            let image = Reference::with_digest(host.into(), path.into(), digest.to_string());
            client
                .store_auth_if_needed(image.resolve_registry(), &RegistryAuth::Anonymous)
                .await;
            client
                .push_blob(&image, bytes.to_vec(), &digest.to_string())
                .await
                .unwrap();
            descriptors.push(serde_json::json!({"mediaType": "application/octet-stream", "digest": digest.to_string(), "size": bytes.len()}));
        }
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": descriptors[0],
            "layers": [descriptors[1]],
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let image = Reference::with_tag(host.into(), path.into(), tag.into());
        client
            .push_manifest_raw(&image, bytes.clone(), "application/vnd.oci.image.manifest.v1+json".parse().unwrap())
            .await
            .unwrap();
        Digest::of(&bytes)
    })
}

fn world(location: &str) -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/app\n{}\nchecks:\n  unit: {{}}\nartifacts:\n  image:\n    type: oci\n    location: {location}\ndeploy:\n  branch: main\n  require: [unit]\n",
            w.ledger_lines()
        ),
    );
    w.commit("registries");
    w
}

// covers: A-1, A-4
#[test]
fn a_later_deploy_takes_the_image_that_passed_by_digest() {
    let Some((a, _)) = hosts() else { return };
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = format!("acme/app-{unique}");
    let digest = seed(&a, &path, "build-1");
    let location = format!("{a}/{path}");
    let w = world(&location);
    let registries = ["--insecure-registry", a.as_str()];

    // The run records the tag it pushed; hushrun pins it through the registry (A-4).
    let writer = || {
        let mut cmd = w.hushrun_as(WRITER);
        cmd.env("HUSHRUN_RUN_ID", "r1");
        cmd
    };
    ok(writer().args(["plan", "--start"]));
    let attested = json(&ok(writer()
        .args([
            "attest",
            "--check",
            "unit=pass",
            "--artifact",
            &format!("image={location}:build-1"),
            "--format",
            "json",
        ])
        .args(registries)));
    assert_eq!(attested["refs"]["image"], format!("{location}@{digest}"));
    ok(writer().args(["mark", "deployed"]));
    ok(writer().args(["mark", "passed"]));

    // A deploy started later promotes: the image that passed, by digest, with nothing built or
    // copied. The plan starts the run attempt, as the recipes' does: deploying needs its
    // `started`.
    let plan = w.path("later.json");
    let later = || {
        let mut cmd = w.hushrun_as(LATER);
        cmd.env("HUSHRUN_RUN_ID", "r2").arg("--plan").arg(&plan);
        cmd
    };
    ok(w.hushrun_as(LATER)
        .env("HUSHRUN_RUN_ID", "r2")
        .args(["plan", "--start", "--out"])
        .arg(&plan));
    let resolved = json(&ok(later()
        .args(["artifact", "resolve", "--format", "json"])
        .args(registries)));
    schema::assert_valid(&schema::load("artifact-resolve.v2.schema.json"), &resolved);
    assert_eq!(resolved["mode"], "promote", "{resolved}");
    assert_eq!(resolved["build"], false, "{resolved}");
    assert_ne!(resolved["artifacts"]["image"]["copied"], true, "{resolved}");
    assert_eq!(
        resolved["artifacts"]["image"]["ref"],
        format!("{location}@{digest}")
    );
    // The registry still serves that digest under the tag the run pushed.
    let tagged = json(&ok(w
        .hushrun()
        .args([
            "artifact",
            "digest",
            &format!("{location}:build-1"),
            "--format",
            "json",
        ])
        .args(registries)));
    assert_eq!(tagged["digest"], digest.to_string());

    // `mark deployed` names the image this run deployed.
    let marked = json(&ok(later().args(["mark", "deployed", "--format", "json"])));
    let deployed_key = marked["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "deployed")
        .unwrap()["keys"][0]
        .as_str()
        .unwrap()
        .to_string();
    let deployed = json(&ok(w.hushrun().args([
        "repository",
        "show",
        &deployed_key,
        "--format",
        "json",
    ])));
    assert_eq!(
        deployed["statement"]["predicate"]["deployed"]["artifacts"][0]["ref"],
        format!("{location}@{digest}")
    );
}

// covers: A-2
#[test]
fn copy_and_digest_by_hand() {
    let Some((a, b)) = hosts() else { return };
    let w = World::new("acme-api.yaml");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = format!("acme/manual-{unique}");
    let digest = seed(&a, &path, "v1");
    let registries = [
        "--insecure-registry",
        a.as_str(),
        "--insecure-registry",
        b.as_str(),
    ];
    let copied = json(&ok(w
        .hushrun()
        .args([
            "artifact",
            "copy",
            &format!("{a}/{path}:v1"),
            &format!("{b}/{path}:v1"),
            "--format",
            "json",
        ])
        .args(registries)));
    schema::assert_valid(&schema::load("artifact-copy.v1.schema.json"), &copied);
    assert_eq!(copied["ref"], format!("{b}/{path}@{digest}"));
    let found = json(&ok(w
        .hushrun()
        .args([
            "artifact",
            "digest",
            &format!("{b}/{path}:v1"),
            "--format",
            "json",
        ])
        .args(registries)));
    schema::assert_valid(&schema::load("artifact-digest.v1.schema.json"), &found);
    assert_eq!(found["digest"], digest.to_string());
}

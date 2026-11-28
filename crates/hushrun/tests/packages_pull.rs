//! `hushrun packages bake --oci-dir` and `hushrun packages pull` (K-14, K-15, ADR 0163): a store
//! baked and signed on one machine, published as an image, served by a registry that holds what
//! was published, and pulled into a fresh sandbox's folder, checked and trusted as K-7 says; a
//! second pull downloads nothing, and a store refused leaves what was there.

#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/maven.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;
use std::sync::{Arc, Mutex};

use cli::Cli as _;
use fixtures::{file, module_pom, parent};
use repo::Repo;
use serde_json::Value;

fn fixture() -> Repo {
    Repo::new(&[
        file(".gitignore", "target/\n"),
        file("pom.xml", &parent(&["core"], "4", "3.5.4")),
        file("core/pom.xml", &module_pom("core", &[])),
    ])
    .clear(&[
        "MAVEN_OPTS",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
        "HUSHRUN_PACKAGES_IMAGE",
        "HUSHRUN_REGISTRY_TOKEN",
    ])
}

/// A registry that serves the image layout at `layout`, read only, over plain HTTP on the
/// loopback: each blob it sends is counted.
struct LayoutRegistry {
    server: hushrun_testkit::http::Server,
    sent: Arc<Mutex<Vec<String>>>,
}

impl LayoutRegistry {
    fn serve(layout: &Path) -> LayoutRegistry {
        use hushrun_testkit::http::{Reply, Response, Server};
        let layout = layout.to_path_buf();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let counted = sent.clone();
        let server = Server::start(move |request| {
            let blob = |digest: &str| {
                std::fs::read(
                    layout
                        .join("blobs/sha256")
                        .join(digest.trim_start_matches("sha256:")),
                )
                .ok()
            };
            let path = request.path.as_str();
            if path == "/v2/" {
                return Reply::Send(Response::json(200, &serde_json::json!({})));
            }
            let head = request.method == "HEAD";
            if let Some((_, reference)) = path.split_once("/manifests/") {
                let index: Value =
                    serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap())
                        .unwrap();
                let found = index["manifests"].as_array().unwrap().iter().find(|m| {
                    m["digest"] == reference
                        || m["annotations"]["org.opencontainers.image.ref.name"] == reference
                });
                let Some(found) = found else {
                    return Reply::Send(Response::new(404));
                };
                let digest = found["digest"].as_str().unwrap();
                let bytes = blob(digest).unwrap();
                let kind = found["mediaType"].as_str().unwrap().to_string();
                let mut response = Response::bytes(200, if head { Vec::new() } else { bytes })
                    .header("docker-content-digest", digest);
                response.headers.retain(|(n, _)| n != "content-type");
                return Reply::Send(response.header("content-type", kind));
            }
            if let Some((_, digest)) = path.split_once("/blobs/") {
                return Reply::Send(match blob(digest) {
                    Some(bytes) => {
                        if !head {
                            counted.lock().unwrap().push(digest.to_string());
                        }
                        Response::bytes(200, if head { Vec::new() } else { bytes })
                            .header("docker-content-digest", digest)
                    }
                    None => Response::new(404),
                });
            }
            Reply::Send(Response::new(404))
        });
        LayoutRegistry { server, sent }
    }

    /// The blobs sent since this was last asked.
    fn sent(&self) -> Vec<String> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }
}

/// A key `hushrun keys generate` writes, and the public key's line for `hushrun.yaml`.
fn key(r: &Repo) -> (std::path::PathBuf, String) {
    let key = r.root.join("key.pem");
    let made = r
        .hushrun()
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let public = r
        .hushrun()
        .args(["keys", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    let public = String::from_utf8_lossy(&public.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("public key: ").map(str::to_string))
        .expect("a public key");
    (key, public)
}

fn policy(signers: Option<&str>) -> String {
    let baked = signers
        .map(|key| format!("build:\n  baked:\n    signers:\n      - key: {key}\n"))
        .unwrap_or_default();
    format!(
        "version: 1\nrepository: github.com/acme/shop\n{baked}checks:\n  test:\n    run: mvn test\n"
    )
}

// covers: K-14, K-15
#[test]
fn a_store_published_as_an_image_is_pulled_into_a_fresh_sandbox_once() {
    let r = fixture();
    let (key, public) = key(&r);
    r.write("hushrun.yaml", &policy(Some(&public)));
    let cache = r.root.join("m2");
    let jar = cache.join("g/n/1/n-1.jar");
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    std::fs::write(&jar, b"jar").unwrap();
    let layout = r.root.join("layout");
    // K-14: no --out: baked aside, published into the layout, signed with the key.
    let baked = r
        .hushrun()
        .args([
            "packages",
            "bake",
            "--no-build",
            "--format",
            "json",
            "--oci-dir",
        ])
        .arg(&layout)
        .arg("--harvest")
        .arg(format!("maven={}", cache.display()))
        .arg("--signing-key")
        .arg(&key)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
    assert!(baked.status.success(), "{stderr}");
    let said: Value = serde_json::from_slice(&baked.stdout).expect(&stderr);
    let identity = said["identity"].as_str().unwrap().to_string();
    let tags: Vec<&str> = said["image"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let fixed = format!("hushrun-packages-{}-{}", platform(&said), &identity[..16]);
    assert_eq!(tags[0], fixed, "{said}");
    assert!(said["image"]["uploaded"].as_bool().unwrap(), "{said}");
    let digest = said["image"]["digest"].as_str().unwrap().to_string();

    let registry = LayoutRegistry::serve(&layout);
    let image = format!("{}/acme/shop", registry.server.addr());
    let sandbox = r.root.join("sandbox").join("hushrun-packages");
    let pull = |extra: &[&str], env: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "pull", "--insecure", "--tag", &fixed, "--to"])
            .arg(&sandbox)
            .args(extra);
        for (name, value) in env {
            cmd.env(name, value);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let schema = hushrun_testkit::schema::load("packages-pull.v1.schema.json");

    // --resolve: the image the tag names, nothing written.
    let resolved = r
        .hushrun()
        .args([
            "packages",
            "pull",
            "--insecure",
            "--tag",
            &fixed,
            "--image",
            &image,
            "--resolve",
        ])
        .output()
        .unwrap();
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&resolved.stdout).trim(),
        format!("{image}@{digest}")
    );
    assert!(!sandbox.exists());

    // Pulled, by the image HUSHRUN_PACKAGES_IMAGE names, and signed by the key hushrun.yaml names.
    let (code, out, err) = pull(&["--format", "json"], &[("HUSHRUN_PACKAGES_IMAGE", &image)]);
    assert_eq!(code, 0, "{err}");
    let first: Value = serde_json::from_str(&out).expect(&err);
    hushrun_testkit::schema::assert_valid(&schema, &first);
    assert_eq!(first["state"], "pulled", "{first}");
    assert_eq!(first["identity"], identity.as_str());
    assert_eq!(first["signature"], "named", "{first}");
    assert!(err.contains("hushrun: pulled "), "{err}");
    assert!(err.contains("signed by "), "{err}");
    assert!(sandbox.join("packages.sigstore.json").is_file());
    let verified = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &sandbox)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    // No layer or staging folder is left beside it.
    let beside: Vec<String> = std::fs::read_dir(sandbox.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(beside, ["hushrun-packages"], "{beside:?}");

    // Again: the folder holds it, so no layer is downloaded.
    registry.sent();
    let (code, out, err) = pull(&["--image", &image, "--format", "json"], &[]);
    assert_eq!(code, 0, "{err}");
    let again: Value = serde_json::from_str(&out).expect(&err);
    assert_eq!(again["state"], "already_there", "{again}");
    let layer = first_layer(&layout, &digest);
    assert!(
        !registry.sent().contains(&layer),
        "the layer was downloaded again"
    );
    // -q says nothing.
    let (code, _, err) = pull(&["--image", &image, "-q"], &[]);
    assert_eq!(code, 0);
    assert_eq!(err, "");

    // hushrun.yaml names no signer and --require-signed: refused, and the folder is as it was.
    let before = std::fs::read(sandbox.join("packages.json")).unwrap();
    r.write("hushrun.yaml", &policy(None));
    let (code, _, err) = pull(&["--image", &image, "--force", "--require-signed"], &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("--require-signed was given"), "{err}");
    assert_eq!(
        std::fs::read(sandbox.join("packages.json")).unwrap(),
        before
    );
    // Without it, it is pulled, and the line says what a run makes of it.
    let (code, out, err) = pull(&["--image", &image, "--force", "--format", "json"], &[]);
    assert_eq!(code, 0, "{err}");
    let unchecked: Value = serde_json::from_str(&out).expect(&err);
    assert_eq!(unchecked["signature"], "not_checked", "{unchecked}");
    assert!(err.contains("HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1"), "{err}");

    // A tag that names nothing replaces nothing.
    let mut cmd = r.hushrun();
    let missing = cmd
        .args([
            "packages",
            "pull",
            "--insecure",
            "--image",
            &image,
            "--platform",
            "plan9-x64",
            "--to",
        ])
        .arg(&sandbox)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(
        err.contains("has no hushrun-packages-plan9-x64-latest"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(sandbox.join("packages.json")).unwrap(),
        before
    );
}

// covers: K-14
#[test]
fn the_backends_registry_is_refused_before_anything_bakes() {
    let r = fixture();
    let out = r
        .hushrun()
        .args(["packages", "bake", "--no-build", "--oci", "hushrun"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a package store yet"), "{err}");
    assert!(err.contains("HUSHRUN_PACKAGES_IMAGE"), "{err}");
    let out = r
        .hushrun()
        .args([
            "packages",
            "bake",
            "--no-build",
            "--oci",
            "registry.hushrun.com/acme/shop",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// The platform the bake's tags name, from its JSON's first tag.
fn platform(said: &Value) -> String {
    let tag = said["image"]["tags"][0].as_str().unwrap();
    let rest = tag.strip_prefix("hushrun-packages-").unwrap();
    rest.rsplit_once('-').unwrap().0.to_string()
}

/// The digest of the one layer of the image `digest` in `layout`.
fn first_layer(layout: &Path, digest: &str) -> String {
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(
            layout
                .join("blobs/sha256")
                .join(digest.trim_start_matches("sha256:")),
        )
        .unwrap(),
    )
    .unwrap();
    manifest["layers"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string()
}

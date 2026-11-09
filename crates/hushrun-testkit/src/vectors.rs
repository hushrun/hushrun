//! The source digest golden vectors (`testdata/digest/vectors.json`).
//!
//! `tools/refdigest.py` computes every expected value independently; these helpers load the
//! vectors and materialize them as git repositories and directories for the Rust adapters.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use serde_json::Value;

use hushrun_core::ScopeName;
use hushrun_core::glob::Pattern;
use hushrun_core::scope::{Scope, Scopes};
use hushrun_digest::hash::sha256;

use crate::git;

/// One entry of a vector.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    pub raw_path: Vec<u8>,
    /// `f`, `x`, `l` or `g`.
    pub kind: char,
    pub content: Vec<u8>,
    /// For `g`: the commit id hex.
    pub commit: Option<String>,
}

/// What a vector expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// scope → (digest, files)
    Digests(BTreeMap<String, (String, u64)>),
    /// An error code, such as `E_EMPTY_SCOPE`.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Vector {
    pub name: String,
    pub entries: Vec<VectorEntry>,
    pub config: Option<Value>,
    pub no_git: bool,
    pub no_dir: bool,
    pub posix_only: bool,
    pub object_format: String,
    pub expect: Expect,
}

/// Every vector.
pub fn load() -> Vec<Vector> {
    let path = crate::testdata("digest/vectors.json");
    let text = std::fs::read_to_string(&path).expect("vectors.json");
    let data: Value = serde_json::from_str(&text).expect("valid JSON");
    data["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .map(parse_vector)
        .collect()
}

fn parse_vector(v: &Value) -> Vector {
    let flag = |name: &str| v[name].as_bool().unwrap_or(false);
    let entries = v["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| {
            let raw_path = match e["pathHex"].as_str() {
                Some(hex) => decode_hex(hex),
                None => e["path"].as_str().expect("path").as_bytes().to_vec(),
            };
            let kind = e["kind"]
                .as_str()
                .expect("kind")
                .chars()
                .next()
                .expect("kind");
            VectorEntry {
                raw_path,
                kind,
                content: content_of(e),
                commit: e["commit"].as_str().map(str::to_string),
            }
        })
        .collect();
    let expect = match v["expect"].get("error") {
        Some(code) => Expect::Error(code.as_str().expect("code").to_string()),
        None => Expect::Digests(
            v["expect"]
                .as_object()
                .expect("expect")
                .iter()
                .map(|(scope, e)| {
                    (
                        scope.clone(),
                        (
                            e["digest"].as_str().expect("digest").to_string(),
                            e["files"].as_u64().expect("files"),
                        ),
                    )
                })
                .collect(),
        ),
    };
    Vector {
        name: v["name"].as_str().expect("name").to_string(),
        entries,
        config: v.get("config").cloned(),
        no_git: flag("noGit"),
        no_dir: flag("noDir"),
        posix_only: flag("posixOnly"),
        object_format: v["objectFormat"].as_str().unwrap_or("sha1").to_string(),
        expect,
    }
}

fn content_of(e: &Value) -> Vec<u8> {
    if let Some(text) = e["text"].as_str() {
        return text.as_bytes().to_vec();
    }
    if let Some(b64) = e["base64"].as_str() {
        return base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64");
    }
    if let Some(spec) = e.get("generate") {
        return generated(
            spec["size"].as_u64().expect("size") as usize,
            spec["seed"].as_u64().unwrap_or(0),
        );
    }
    if let Some(target) = e["target"].as_str() {
        return target.as_bytes().to_vec();
    }
    if let Some(commit) = e["commit"].as_str() {
        let format = e["objectFormat"].as_str().unwrap_or("sha1");
        return format!("{format}:{commit}").into_bytes();
    }
    Vec::new()
}

/// Deterministic content: a SHA-256 counter stream, as in `refdigest.py`.
pub fn generated(size: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 32);
    let mut counter = 0u64;
    while out.len() < size {
        out.extend_from_slice(&sha256(
            format!("hushrun-vector-{seed}-{counter}").as_bytes(),
        ));
        counter += 1;
    }
    out.truncate(size);
    out
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

impl Vector {
    /// The vector's scopes, as a policy would define them.
    pub fn scopes(&self) -> Scopes {
        let compile = |list: &Value| -> Vec<Pattern> {
            list.as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| Pattern::new(p.as_str().expect("pattern")).expect("valid pattern"))
                        .collect()
                })
                .unwrap_or_default()
        };
        let Some(config) = &self.config else {
            return Scopes::default_scope(Vec::new());
        };
        let global = compile(&config["digestExclude"]);
        match config["scopes"].as_object() {
            None => Scopes::default_scope(global),
            Some(scopes) => Scopes::new(
                scopes
                    .iter()
                    .map(|(name, s)| Scope {
                        name: ScopeName::new(name.clone()).expect("scope name"),
                        include: if s.get("include").is_some() {
                            compile(&s["include"])
                        } else {
                            vec![Pattern::new("**").expect("valid")]
                        },
                        exclude: compile(&s["exclude"]),
                        external: Vec::new(),
                    })
                    .collect(),
                global,
            ),
        }
    }

    /// A git repository at `dir` whose HEAD holds exactly the vector's entries, built with
    /// plumbing so raw paths and modes are exact. The worktree stays empty.
    pub fn materialize_git(&self, dir: &Path) {
        git::init(dir, &self.object_format);
        let mut index_info = Vec::new();
        for entry in &self.entries {
            let (mode, oid) = match entry.kind {
                'g' => ("160000", entry.commit.clone().expect("gitlink commit")),
                kind => {
                    let mode = match kind {
                        'f' => "100644",
                        'x' => "100755",
                        'l' => "120000",
                        other => panic!("unknown kind {other}"),
                    };
                    let oid = String::from_utf8(git::run(
                        dir,
                        &["hash-object", "-w", "--stdin"],
                        Some(&entry.content),
                    ))
                    .expect("oid")
                    .trim()
                    .to_string();
                    (mode, oid)
                }
            };
            index_info.extend_from_slice(format!("{mode} {oid}\t").as_bytes());
            index_info.extend_from_slice(&entry.raw_path);
            index_info.push(0);
        }
        git::run(
            dir,
            &["update-index", "-z", "--index-info"],
            Some(&index_info),
        );
        let tree = git::text(dir, &["write-tree"]);
        let commit = git::text(dir, &["commit-tree", &tree, "-m", &self.name]);
        git::run(dir, &["update-ref", "HEAD", &commit], None);
    }

    /// Writes the vector's files, execute bits and symbolic links under `dir`.
    pub fn materialize_dir(&self, dir: &Path) {
        for entry in &self.entries {
            let relative = std::str::from_utf8(&entry.raw_path).expect("UTF-8 path on disk");
            let path = dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            match entry.kind {
                'f' | 'x' => {
                    std::fs::write(&path, &entry.content).expect("write");
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = if entry.kind == 'x' { 0o755 } else { 0o644 };
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                            .expect("chmod");
                    }
                }
                'l' => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(
                        std::str::from_utf8(&entry.content).expect("target"),
                        &path,
                    )
                    .expect("symlink");
                    #[cfg(not(unix))]
                    panic!("symbolic links need POSIX ({})", self.name);
                }
                other => panic!("kind {other} can't be written to a directory"),
            }
        }
    }
}

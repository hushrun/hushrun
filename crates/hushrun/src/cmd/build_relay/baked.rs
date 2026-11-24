//! B-63, B-64: `hushrun build bake`'s writing of a baked store, the relay's check of one as a
//! run begins, and the opening of a baked store's packs and hints that the wrapper, the Go
//! cache program and the C and C++ launcher ask of the binary ([`Packs`]): the format
//! (`hushrun_build::baked`) and its states are the wrapper's crate's; zstd and the cipher are here.
//!
//! A bake writes a new folder beside `--out`, checks that it holds sealed packs, sealed hints and
//! `baked.json` and nothing else ([`baked::check_contents`]), and only then puts it in place
//! ([`baked::put_in_place`]), so a reader finds the old store or the new one, never half of
//! either, and never a file a bake didn't mean to write.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::baked::{
    self, BAKED_JSON, Baked, BakedCheck, PackRow, Part, Source, Trust, Unseal, Untrusted,
};
use hushrun_build::manifests;
use hushrun_build::model::Kind;
use hushrun_build::origin::{Origin, Standing};
use hushrun_build::store::Store;
use hushrun_core::Digest;
use hushrun_core::blocks::{self, Class, Hex32, OpenRequest, Pair, Policy};

use super::{Opened, Relay, pack};

/// What opens a baked store's packs and hints for the wrapper, the Go cache program and the
/// launcher (B-64).
pub(crate) struct Packs;

/// The one [`Packs`], which `main` installs before any of them runs.
pub(crate) static PACKS: Packs = Packs;

/// Installs [`PACKS`] for this process.
pub(crate) fn install() {
    baked::install(&PACKS);
}

impl Unseal for Packs {
    fn hint(&self, step: &Digest, sealed: &[u8]) -> Option<Vec<u8>> {
        blocks::pack::open_hint(step.as_bytes(), sealed)
    }

    fn block(
        &self,
        store: &Store,
        kind: Kind,
        path: &Path,
        row: &PackRow,
        key: &Digest,
        step: &Digest,
        origin: &Origin,
        floor: u64,
        repository: Option<&str>,
        now: SystemTime,
    ) -> Result<(), String> {
        let free = store.free_bytes().unwrap_or(u64::MAX);
        let room = blocks::pack::Room::for_pack(row.bytes, free.saturating_sub(floor));
        let expect = blocks::pack::Expect {
            key: *key.as_bytes(),
            step: Some(Hex32(*step.as_bytes())),
            pack_sha256: Some(row.sha256),
            bytes: Some(row.bytes),
            outputs: Some(row.outputs),
        };
        let dir = store
            .root()
            .join("tmp")
            .join(hushrun_build::baked::unique(&format!(
                "baked-{}",
                &key.hex()[..16]
            )));
        let opened = pack::open_pack(path, &expect, Some(room), &dir).map_err(|e| e.to_string())?;
        let published = pack::publish(
            store,
            kind,
            key,
            &opened,
            repository,
            row.cost_ms,
            origin,
            now,
        );
        opened.remove();
        published?;
        // A copy with the same outputs only downloaded, or opened from an older baked store,
        // stays where it is (a reuse may be copying from it) and names this baked store, and
        // the server's word, from now on.
        if let Some(from) = &origin.baked {
            let _ = store.update_origin(key, |there| {
                let mut there = there.filter(Origin::downloaded_only)?;
                let mut changed = there.baked.as_ref() != Some(from);
                there.baked = Some(from.clone());
                for vouch in &origin.vouched {
                    if !there.vouched.contains(vouch) {
                        there.renew(vouch.clone());
                        changed = true;
                    }
                }
                changed.then_some(there)
            });
        }
        Ok(())
    }
}

/// What a bake knows besides its blocks.
pub(crate) struct Facts {
    pub(crate) repository: Option<String>,
    /// The checks to bake from a backend (`None`: the repository's newest build of any check).
    pub(crate) checks: Vec<Option<Hex32>>,
    pub(crate) toolchains: Vec<String>,
    /// The compiler's target triple where it was baked (`rustc -vV`'s `host:`).
    pub(crate) rustc_host: Option<String>,
    /// Go's `GOOS_GOARCH` where it was baked, for a Go module.
    pub(crate) go_platform: Option<String>,
    /// The C compiler's target triple where it was baked.
    pub(crate) cc_platform: Option<String>,
    /// `--trusted`: only blocks a published image may hold.
    pub(crate) trusted_only: bool,
    /// B-65: the lockfile's packages, which place a Rust block in an image's deps layer.
    pub(crate) third_party: Option<super::bake_image::ThirdParty>,
    pub(crate) now: SystemTime,
    /// B-76: the CI job's folders, for a store CI bakes from its trusted scope; else none.
    pub(crate) paths: Option<baked::BakedPaths>,
}

/// What a bake wrote.
pub(crate) struct Written {
    pub(crate) baked: Baked,
    pub(crate) bytes: u64,
    pub(crate) hints: usize,
    /// Blocks of the manifests left out by rule (not portable, outside the trusted scope), by
    /// why.
    pub(crate) left_out: BTreeMap<String, usize>,
    /// Blocks of the manifests that should have gone in and didn't (no longer in the store,
    /// not downloaded in time): the store is partial.
    pub(crate) missing: usize,
    /// B-65: the image layer group of each pack, by its address, and of each first key's hints,
    /// by the first key's address. What isn't here is the repository's own code.
    pub(crate) pack_groups: std::collections::HashMap<Hex32, hushrun_artifact::image::Group>,
    pub(crate) hint_groups: std::collections::HashMap<Hex32, hushrun_artifact::image::Group>,
    /// B-63: the folder's bake lock, held while this lives: the bake signs the folder and writes
    /// its archives under it, so no other bake replaces the folder they read. Held for what its
    /// drop does.
    #[allow(dead_code)]
    pub(crate) held: baked::BakeLock,
}

/// Why a block of `description` may not be baked from a machine's own store: only a block that
/// may leave any machine goes in (B-24, B-42, B-56), never one bound to a person's folders. A
/// description that can't be read says nothing of where the block may go, so it stays.
fn kept_here(kind: Kind, description: &serde_json::Value) -> Option<String> {
    match kind {
        Kind::Rustc => {
            let Some(parsed) = hushrun_build::description::Description::from_value(description)
            else {
                return Some("its description can't be read".into());
            };
            let portability = parsed.portability;
            (!portability.portable).then(|| {
                portability
                    .reasons
                    .first()
                    .map_or("stays on this machine", |r| r.as_str())
                    .to_string()
            })
        }
        // A Go block is stored only when it may travel (B-42), and so is an Xcode one (B-89).
        Kind::Go | Kind::Xcode => None,
        Kind::Cc => description
            .get("local")
            .and_then(serde_json::Value::as_array)
            .and_then(|local| local.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        // Gradle's entries aren't baked yet (B-82).
        Kind::Gradle => Some("Gradle entries aren't baked".into()),
    }
}

fn kind_of(word: &str) -> Option<Kind> {
    baked::KINDS.iter().copied().find(|k| k.word() == word)
}

/// The digest of a linked output's link environment (B-37), from its description.
fn link_environment(description: &serde_json::Value) -> Option<Hex32> {
    let env = description.get("link_environment")?;
    Some(Hex32::sha256(serde_json::to_vec(env).ok()?.as_slice()))
}

/// B-63, B-68: the checks of a store baked from this machine, from the manifests runs saved
/// here (`saved`, one per check): each part of a kind a baked store carries, the `rustc/1`
/// part's platform the compiler's own target where it is known. The one definition `bake` and
/// `build manifest` take a store's identity from ([`baked::store_manifest`]).
pub(crate) fn checks_of_store(
    saved: &[manifests::Saved],
    rustc_host: Option<&str>,
) -> Vec<BakedCheck> {
    saved
        .iter()
        .map(|saved| BakedCheck {
            check: saved.check,
            parts: saved
                .parts
                .iter()
                .filter_map(|part| {
                    let kind = kind_of(&part.kind)?;
                    let platform = match (kind, rustc_host) {
                        (Kind::Rustc, Some(host)) => host.to_string(),
                        _ => part.platform.clone(),
                    };
                    Some(Part {
                        kind: kind.word().into(),
                        platform,
                        manifest: part.id(),
                    })
                })
                .collect(),
        })
        .filter(|check| !check.parts.is_empty())
        .collect()
}

/// B-63, B-68: the checks of a store baked from a backend, from the manifests `open` answered
/// (check, kind, platform, pairs): an empty manifest is no part, and a check with none is left
/// out. The one definition `bake` and `build manifest` take a store's identity from.
pub(crate) fn checks_of_backend(
    manifests: &[(Option<Hex32>, Kind, String, Vec<Pair>)],
) -> Vec<BakedCheck> {
    let mut out: Vec<BakedCheck> = Vec::new();
    for (check, kind, platform, pairs) in manifests {
        if pairs.is_empty() {
            continue;
        }
        let part = Part {
            kind: kind.word().into(),
            platform: platform.clone(),
            manifest: baked::manifest_id(kind.word(), pairs),
        };
        match out.iter_mut().find(|c| c.check == *check) {
            Some(entry) => entry.parts.push(part),
            None => out.push(BakedCheck {
                check: *check,
                parts: vec![part],
            }),
        }
    }
    out
}

/// B-63: bakes `out` from this machine's store and the manifests runs left there (`saved`, one
/// per check), every kind of each. A block several checks name goes in once. Only a block that
/// may leave any machine goes in; the result is untrusted, since blocks a machine built or
/// downloaded itself are for whoever baked them.
pub(crate) fn from_store(
    store: &Store,
    saved: &[manifests::Saved],
    facts: &Facts,
    out: &Path,
) -> Result<Written, String> {
    if facts.trusted_only {
        return Err(
            "a store baked from this machine's store is never trusted: `--trusted` takes `--from backend`, whose blocks CI on the default branch wrote"
                .into(),
        );
    }
    let repository = saved.first().and_then(|s| s.repository.clone());
    let target = saved.first().map(|s| s.target.clone()).unwrap_or_default();
    // A folder of its own: a name no other bake on this store uses, never removed before it
    // is ours (a crashed bake's is swept with the store's leftovers).
    let spill = store
        .root()
        .join("tmp")
        .join(hushrun_build::baked::unique("bake"));
    fs::create_dir_all(spill.parent().unwrap_or(store.root()))
        .map_err(|err| format!("{}: {err}", spill.display()))?;
    fs::create_dir(&spill).map_err(|err| format!("{}: {err}", spill.display()))?;
    let mut left_out: BTreeMap<String, usize> = BTreeMap::new();
    let mut missing = 0usize;
    let mut bytes = 0u64;
    let mut hints = 0usize;
    let mut pack_groups = std::collections::HashMap::new();
    let mut hint_groups = std::collections::HashMap::new();
    let now_ms = epoch_ms(facts.now);
    let checks = checks_of_store(saved, facts.rustc_host.as_deref());
    let written = write_whole(out, |folder| {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for part in saved.iter().flat_map(|s| s.parts.iter()) {
            let Some(kind) = kind_of(&part.kind) else {
                continue;
            };
            for (step, key) in &part.pairs {
                let address = blocks::address(kind.word(), key.as_bytes());
                // B-63: one row per pack, whichever checks name it.
                if !seen.insert(address) {
                    continue;
                }
                let block = match store.block(key) {
                    Ok(Some(block)) => block,
                    _ => {
                        missing += 1;
                        continue;
                    }
                };
                if hushrun_build::model::Kind::of_description(&block.description) != kind {
                    continue;
                }
                if let Some(why) = kept_here(kind, &block.description) {
                    *left_out.entry(why).or_default() += 1;
                    continue;
                }
                // A download a server withdrew is never reused as one again (B-34): nor baked.
                if store
                    .origin(key)
                    .is_some_and(|o| o.standing(None, now_ms) == Standing::Withdrawn)
                {
                    *left_out.entry("a server withdrew it".into()).or_default() += 1;
                    continue;
                }
                let packed = pack::pack_block_within(store, key, &spill, pack::IN_MEMORY)
                    .map_err(|err| format!("packing a block: {err:?}"))?;
                let path = baked::pack_path(folder, &address);
                let copied = (|| {
                    let mut reader = packed.pack.reader()?;
                    let mut file = File::create(&path)?;
                    io::copy(&mut reader, &mut file)?;
                    file.sync_all()
                })();
                let sealed = packed.sealed;
                let outputs = packed.outputs;
                packed.pack.remove();
                copied.map_err(|err| format!("{}: {err}", path.display()))?;
                bytes += sealed.bytes;
                let class = block
                    .description
                    .get("class")
                    .cloned()
                    .and_then(|c| serde_json::from_value::<Class>(c).ok())
                    .unwrap_or(Class::Library);
                rows.push(PackRow {
                    kind: kind.word().into(),
                    address,
                    bytes: sealed.bytes,
                    sha256: sealed.sha256,
                    outputs,
                    class,
                    cost_ms: block.description["rustc_ms"]
                        .as_u64()
                        .or_else(|| block.description["compile_ms"].as_u64())
                        .unwrap_or(0),
                    scope: None,
                    link_environment: link_environment(&block.description),
                });
                if kind != Kind::Go {
                    hints += write_hints(folder, kind, step, store)?;
                }
                if kind == Kind::Rustc
                    && let Some(third_party) = &facts.third_party
                {
                    let package = block.description["package"].as_str();
                    let group = third_party.group(package);
                    pack_groups.insert(address, group);
                    hint_groups.insert(blocks::address(kind.word(), step.as_bytes()), group);
                }
            }
        }
        Ok(Baked {
            schema: baked::SCHEMA.into(),
            trust: Trust::Untrusted {
                because: Untrusted::LocalStore,
            },
            manifest: baked::store_manifest(&checks),
            source: Source::Store,
            repository: repository.clone(),
            platform: baked::platform_of(facts.rustc_host.as_deref().unwrap_or(&target)),
            checks: checks.clone(),
            toolchains: facts.toolchains.clone(),
            packs: rows,
            baked_ms: now_ms,
            paths: None,
        })
    });
    let _ = fs::remove_dir_all(&spill);
    let (written, held) = written?;
    Ok(Written {
        baked: written,
        held,
        bytes,
        hints,
        left_out,
        missing,
        pack_groups,
        hint_groups,
    })
}

/// The hints this machine holds for `step`, its own and those downloaded, each sealed with the
/// first key and written once. How many.
fn write_hints(folder: &Path, kind: Kind, step: &Digest, store: &Store) -> Result<usize, String> {
    let mut lists = store.hints(step);
    lists.extend(store.shared_hints(step));
    let mut written = 0;
    let mut seen = HashSet::new();
    for list in lists {
        if !seen.insert(list.clone()) {
            continue;
        }
        let sealed = blocks::pack::seal_hint(step.as_bytes(), &list)
            .map_err(|err| format!("sealing a hint: {err}"))?;
        let dir = baked::hints_dir(folder, &blocks::address(kind.word(), step.as_bytes()));
        fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = dir.join(Hex32::sha256(&sealed).to_string());
        fs::write(&path, &sealed).map_err(|err| format!("{}: {err}", path.display()))?;
        written += 1;
    }
    Ok(written)
}

/// B-63: bakes `out` from the org's backend: for each check of `facts` and each kind and
/// platform this machine builds (`rustc/1` always; `go/1` in a Go module; `cc/1` where a C
/// compiler answers), the manifest `open` answers, and what they name that the backend serves, as
/// `hushrun build fetch` downloads it, each pack once. It claims trust only when every pack came
/// from the repository's trusted scope (the `protected/…` scope `open` named); with `--trusted`,
/// every other pack is left out.
pub(crate) fn from_backend(
    relay: &Arc<Relay>,
    facts: &Facts,
    out: &Path,
) -> Result<Written, String> {
    let mut wanted = vec![(
        Kind::Rustc,
        facts
            .rustc_host
            .clone()
            .unwrap_or_else(|| relay.config.platform.clone()),
    )];
    if let Some(go) = &facts.go_platform {
        wanted.push((Kind::Go, go.clone()));
    }
    if let Some(cc) = &facts.cc_platform {
        wanted.push((Kind::Cc, cc.clone()));
    }
    let pieces = relay.fetch_parts(&facts.checks, &wanted);
    let pieces = match pieces {
        Ok(pieces) => pieces,
        Err(why) => {
            relay.drop_incoming();
            return Err(why);
        }
    };
    let trusted_scope = |scope: &str| pieces.trusted_scope.as_deref() == Some(scope);
    let all_trusted = pieces
        .arrived
        .iter()
        .all(|(_, f, _)| trusted_scope(&f.scope));
    let trust = if facts.trusted_only || all_trusted {
        Trust::Trusted
    } else {
        Trust::Untrusted {
            because: Untrusted::Scope,
        }
    };
    let mut left_out: BTreeMap<String, usize> = BTreeMap::new();
    let mut bytes = 0u64;
    let mut hints = 0usize;
    let now_ms = epoch_ms(facts.now);
    let arrived: HashSet<Hex32> = pieces.arrived.iter().map(|(_, f, _)| f.address).collect();
    let missing = pieces
        .manifests
        .iter()
        .flat_map(|(_, _, _, pairs)| pairs.iter().map(|Pair(_, block)| *block))
        .collect::<HashSet<_>>()
        .difference(&arrived)
        .count();
    let checks = checks_of_backend(&pieces.manifests);
    let written = write_whole(out, |folder| {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for (kind, found, path) in &pieces.arrived {
            if !seen.insert(found.address) {
                continue;
            }
            if trust.is_trusted() && !trusted_scope(&found.scope) {
                *left_out
                    .entry("outside the repository's trusted scope".into())
                    .or_default() += 1;
                continue;
            }
            let dest = baked::pack_path(folder, &found.address);
            fs::rename(path, &dest)
                .or_else(|_| fs::copy(path, &dest).map(|_| ()))
                .map_err(|err| format!("{}: {err}", dest.display()))?;
            bytes += found.bytes;
            rows.push(PackRow {
                kind: kind.word().into(),
                address: found.address,
                bytes: found.bytes,
                sha256: found.pack_sha256,
                outputs: found.outputs,
                class: found.class,
                cost_ms: u64::from(found.cost_ms),
                scope: Some(found.scope.clone()),
                link_environment: None,
            });
        }
        for found in &pieces.hints {
            let dir = baked::hints_dir(folder, &found.address);
            for list in &found.lists {
                fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
                let path = dir.join(Hex32::sha256(&list.0).to_string());
                fs::write(&path, &list.0).map_err(|err| format!("{}: {err}", path.display()))?;
                hints += 1;
            }
        }
        Ok(Baked {
            schema: baked::SCHEMA.into(),
            trust,
            manifest: baked::store_manifest(&checks),
            source: Source::Backend {
                host: relay.by.host.clone(),
                org: relay.by.org.clone(),
            },
            repository: facts.repository.clone(),
            platform: baked::platform_of(&wanted[0].1),
            checks: checks.clone(),
            toolchains: facts.toolchains.clone(),
            packs: rows,
            baked_ms: now_ms,
            paths: facts.paths.clone(),
        })
    });
    relay.drop_incoming();
    let (written, held) = written?;
    Ok(Written {
        baked: written,
        held,
        bytes,
        hints,
        left_out,
        missing,
        pack_groups: std::collections::HashMap::new(),
        hint_groups: std::collections::HashMap::new(),
    })
}

/// Writes a baked store whole at `out`: `fill` writes the packs and hints into a new folder
/// beside it and says what `baked.json` holds; the folder is checked to hold nothing else, then
/// put in place ([`baked::put_in_place`]). `out` must not exist, be an empty folder, or hold a
/// baked store (which is then replaced). On any failure `out` stays as it was and the new folder
/// goes. One bake of `out` at a time ([`baked::lock_bake`]); what a bake that died left is put
/// right first ([`baked::reconcile`]). The lock comes back with the store, for the caller to hold
/// as long as it reads `out` (its signature and archives are made from it).
pub(crate) fn write_whole(
    out: &Path,
    fill: impl FnOnce(&Path) -> Result<Baked, String>,
) -> Result<(Baked, baked::BakeLock), String> {
    let at = |err: io::Error, path: &Path| format!("{}: {err}", path.display());
    // B-63: one bake of a folder at a time, for the whole bake: under the lock, whatever lies
    // beside `out` is a dead bake's, and the check below and the exchange see one `out`. A
    // process other than a bake that writes into `out` meanwhile loses what it wrote with the
    // old store: `out` is bake's own folder, and the check only refuses a folder that was
    // never a baked store.
    let held = baked::lock_bake(out).map_err(|err| err.to_string())?;
    baked::reconcile(out, &held).map_err(|err| at(err, out))?;
    match fs::symlink_metadata(out) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(at(err, out)),
        Ok(meta) if !meta.is_dir() => {
            return Err(format!("{} is not a folder", out.display()));
        }
        Ok(_) => {
            let empty = fs::read_dir(out)
                .map_err(|err| at(err, out))?
                .next()
                .is_none();
            let a_baked_store = out.join(BAKED_JSON).is_file()
                && baked::check_contents(out, None, baked::Depth::Names).is_ok();
            if !empty && !a_baked_store {
                return Err(format!(
                    "{} holds something other than a baked store: bake writes into a new or empty folder, or over a baked store",
                    out.display()
                ));
            }
        }
    }
    let new = baked::new_folder(out).map_err(|err| at(err, out))?;
    let filled = fs::create_dir_all(new.join(baked::PACKS))
        .map_err(|err| at(err, &new))
        .and_then(|()| fill(&new))
        .and_then(|mut baked| {
            // B-63: the rows in one order (kind, then address), never the order the build's
            // compiles ended in, which cargo's parallel build changes: the same store, the same
            // bytes.
            sort_rows(&mut baked.packs);
            let json = serde_json::to_vec_pretty(&baked).map_err(|err| err.to_string())?;
            let path = new.join(BAKED_JSON);
            fs::write(&path, json).map_err(|err| at(err, &path))?;
            // Nothing but sealed packs, sealed hints and baked.json leaves this command.
            baked::check_contents(&new, Some(&baked), baked::Depth::Bytes)
                .map_err(|why| format!("the baked folder isn't what a bake writes: {why}"))?;
            // And the file reads back as one, its claims included.
            Baked::read(&new)
                .map_err(|why| format!("the baked folder doesn't read back: {}", why.0))?;
            // On the disk before it takes `out`'s name: a crash after the swap finds the whole
            // store, never names whose bytes were still in memory.
            sync_tree(&new).map_err(|err| at(err, &new))?;
            Ok(baked)
        });
    let baked = match filled {
        Ok(baked) => baked,
        Err(why) => {
            let _ = fs::remove_dir_all(&new);
            return Err(why);
        }
    };
    if let Err(err) = baked::put_in_place(&new, out) {
        let _ = fs::remove_dir_all(&new);
        return Err(at(err, out));
    }
    if let Some(parent) = out.parent() {
        let _ = sync_dir(parent);
    }
    Ok((baked, held))
}

/// B-63: a store's rows in the one order its `baked.json` holds them: by kind, then address.
pub(crate) fn sort_rows(rows: &mut [PackRow]) {
    rows.sort_by(|a, b| (a.kind.as_str(), a.address).cmp(&(b.kind.as_str(), b.address)));
}

/// Every file and folder under `dir`, and `dir` itself, written through to the disk.
fn sync_tree(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            sync_tree(&path)?;
        } else {
            File::open(&path)?.sync_all()?;
        }
    }
    sync_dir(dir)
}

/// A folder's entries written through to the disk, where the system lets a folder be opened.
fn sync_dir(dir: &Path) -> io::Result<()> {
    if cfg!(unix) {
        File::open(dir)?.sync_all()
    } else {
        Ok(())
    }
}

/// What the relay learned of a run's baked store as the run began (B-63): what its backend
/// vouched for, and whether a manifest of one of its checks is newer.
pub(crate) struct Checked {
    /// The rows the backend serves in the repository's trusted scope with that row's outputs,
    /// SHA-256 and size, as the store has them (its class, cost and link environment are the
    /// store's claim).
    pub(crate) verified: Vec<PackRow>,
    /// The repository's trusted scope, as `open` named it.
    pub(crate) scope: String,
}

impl Relay {
    /// B-63: as a run begins, within `within`: waits for the check's `open`, then looks every
    /// pack of `baked` up (without fetching) for the run's repository, keeping each row the
    /// backend serves in the repository's trusted scope (the `protected/…` scope `open` named
    /// among those the run reads) with that row's outputs, SHA-256 and size, and asks `open` for
    /// each part of each check, which marks the store replaced in this machine's store when
    /// another, not empty, is named. Evidence is per block: a lookup that fails, or the time
    /// running out, leaves out only the blocks it hadn't vouched for yet. `None`: `open` didn't
    /// answer in time, or named no trusted scope, which is no evidence and no replacement.
    pub(crate) fn check_baked(
        self: &Arc<Relay>,
        baked: &Baked,
        within: Duration,
    ) -> Option<Checked> {
        let started = Instant::now();
        let (verified, scope) = self.vouched_rows(&baked.packs, within)?;
        let left = within.saturating_sub(started.elapsed());
        let newer = Arc::new(std::sync::Mutex::new(None::<Hex32>));
        let relay = self.clone();
        let owned = baked.clone();
        let newer_into = newer.clone();
        self.runtime.block_on(async move {
            let baked = owned;
            let _ = tokio::time::timeout(left, async move {
                // A check of none (the repository's newest build of any check) is never asked:
                // the newest of any check would replace it at every other run.
                for entry in baked.checks.iter().filter(|c| c.check.is_some()) {
                    for part in &entry.parts {
                        let body = OpenRequest {
                            kind: part.kind.clone(),
                            repository: relay.config.repository.clone(),
                            commit: relay.config.commit.clone(),
                            check: entry.check,
                            platform: part.platform.clone(),
                        };
                        let Ok(answered) = relay
                            .client
                            .post::<_, Policy>(
                                blocks::OPEN_PATH,
                                &body,
                                relay.config.tuning.open,
                                None,
                            )
                            .await
                        else {
                            continue;
                        };
                        let manifest = answered.body.manifest;
                        if !manifest.is_empty()
                            && baked::manifest_id(&part.kind, &manifest) != part.manifest
                            && let Ok(mut newer) = newer_into.lock()
                        {
                            *newer = Some(baked::manifest_id(&part.kind, &manifest));
                        }
                    }
                }
            })
            .await;
        });
        let newer = *newer.lock().ok()?;
        if let Some(newer) = newer
            && baked::mark_replaced(
                &self.config.store,
                &baked.manifest,
                &newer,
                SystemTime::now(),
            )
            .is_ok()
        {
            self.state().say(
                "build: the baked store has a newer manifest at its source: a block from it is reused only once the server vouches for it"
                    .to_string(),
            );
        }
        Some(Checked { verified, scope })
    }

    /// B-63, B-65: of `rows`, those the backend serves for the run's repository in its trusted
    /// scope (the one `protected/…` scope `open` named among those the run reads) with that
    /// row's outputs, SHA-256 and size, looked up (without fetching) within `within`, with that
    /// scope. Evidence is per block: a lookup that fails, or the time running out, leaves out only
    /// the blocks it hadn't vouched for yet. `None`: `open` didn't answer in time, or named no
    /// trusted scope.
    pub(crate) fn vouched_rows(
        self: &Arc<Relay>,
        rows: &[PackRow],
        within: Duration,
    ) -> Option<(Vec<PackRow>, String)> {
        let started = Instant::now();
        if !matches!(self.wait_open(within), Opened::Answering) {
            return None;
        }
        let (scope, limit) = {
            let state = self.state();
            let policy = state.policy.as_ref()?;
            let scope = trusted_scope(&policy.read)?.to_string();
            (scope, policy.limits.lookup.max(1) as usize)
        };
        let left = within.saturating_sub(started.elapsed());
        let verified = Arc::new(std::sync::Mutex::new(Vec::<PackRow>::new()));
        let relay = self.clone();
        // Every chunk of every kind, looked up a few at a time, so a large store's last rows get
        // their answer within the same wait as its first.
        let mut chunks: Vec<(Kind, Vec<PackRow>)> = Vec::new();
        for kind in baked::KINDS {
            let of_kind: Vec<PackRow> = rows
                .iter()
                .filter(|r| r.kind == kind.word())
                .cloned()
                .collect();
            chunks.extend(of_kind.chunks(limit).map(|c| (kind, c.to_vec())));
        }
        let (into, trusted) = (verified.clone(), scope.clone());
        self.runtime.block_on(async move {
            use futures_util::StreamExt as _;
            let lookups = futures_util::stream::iter(chunks)
                .map(|(kind, chunk)| {
                    let relay = relay.clone();
                    async move {
                        let addresses: Vec<Hex32> = chunk.iter().map(|r| r.address).collect();
                        // A lookup that fails vouches for none of these, and no other's.
                        let found = relay.lookup_in(kind, &addresses, false).await;
                        (chunk, found)
                    }
                })
                .buffer_unordered(LOOKUPS_AT_ONCE)
                .for_each(|(chunk, found)| {
                    for f in found.unwrap_or_default() {
                        let Some(row) = chunk.iter().find(|r| r.address == f.address) else {
                            continue;
                        };
                        if row.outputs == f.outputs
                            && row.sha256 == f.pack_sha256
                            && row.bytes == f.bytes
                            && f.scope == trusted
                            && let Ok(mut into) = into.lock()
                            && !into.iter().any(|kept| kept.address == row.address)
                        {
                            into.push(row.clone());
                        }
                    }
                    std::future::ready(())
                });
            let _ = tokio::time::timeout(left, lookups).await;
        });
        let verified = std::mem::take(&mut *verified.lock().ok()?);
        Some((verified, scope))
    }

    /// B-63: whether this run may write the repository's trusted blocks, as `open` answered
    /// within `within`: `None` when it didn't answer.
    pub(crate) fn writes_trusted_within(&self, within: Duration) -> Option<bool> {
        if !matches!(self.wait_open(within), Opened::Answering) {
            return None;
        }
        self.state().policy.as_ref().map(Policy::writes_trusted)
    }
}

/// How many lookups of a baked store's rows are in flight at once as a run begins (B-63).
const LOOKUPS_AT_ONCE: usize = 8;

/// B-63: the repository's trusted scope among the scopes `open` says a run reads: the one
/// `protected/…` scope there. None when there is none, or more than one: which is the
/// repository's would rest on their order alone.
pub(super) fn trusted_scope(read: &[String]) -> Option<&str> {
    let mut trusted = read.iter().filter(|scope| blocks::is_trusted_scope(scope));
    let one = trusted.next()?;
    trusted.next().is_none().then_some(one.as_str())
}

fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

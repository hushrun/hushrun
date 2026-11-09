//! The golden vectors of `testdata/tlog/vectors.json`, which `tools/reftlog.py` computes
//! independently (RFC 6962's recursive definitions, RFC 8032's reference Ed25519), and whose
//! notes and checkpoint Go's `golang.org/x/mod/sumdb/note` signs identically.

use hushrun_tlog::checkpoint::Checkpoint;
use hushrun_tlog::entry::{Entry, EntryKind};
use hushrun_tlog::listing::{Listing, SignedListing};
use hushrun_tlog::map::{Map, MapEntry, MapProof, keys_root};
use hushrun_tlog::note::{self, Signer, Verifier};
use hushrun_tlog::proof::{verify_consistency, verify_inclusion, verify_range};
use hushrun_tlog::{Hash, MemTree, SignedCheckpoint};
use serde_json::Value;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/tlog/vectors.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("vectors.json")).expect("json")
}

fn hex(value: &Value) -> Hash {
    Hash::from_hex(value.as_str().expect("a hex string")).expect("a hash")
}

fn hashes(value: &Value) -> Vec<Hash> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(hex)
        .collect()
}

fn tree(v: &Value) -> MemTree {
    MemTree::from_leaves(hashes(&v["rfc6962"]["leaf_hashes"]))
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

// covers: L-11
#[test]
fn rfc_6962_leaves_and_roots() {
    let v = vectors();
    let leaves = v["rfc6962"]["leaves"].as_array().unwrap();
    for (data, expected) in leaves.iter().zip(hashes(&v["rfc6962"]["leaf_hashes"])) {
        assert_eq!(Hash::leaf(&decode_hex(data.as_str().unwrap())), expected);
    }
    let t = tree(&v);
    for (size, root) in hashes(&v["rfc6962"]["roots"]).into_iter().enumerate() {
        assert_eq!(t.root_at(size as u64), Some(root), "size {size}");
    }
}

// covers: L-11
#[test]
fn rfc_6962_inclusion_proofs() {
    let v = vectors();
    let t = tree(&v);
    let roots = hashes(&v["rfc6962"]["roots"]);
    for case in v["rfc6962"]["inclusion"].as_array().unwrap() {
        let index = case["index"].as_u64().unwrap();
        let size = case["size"].as_u64().unwrap();
        let expected = hashes(&case["proof"]);
        assert_eq!(
            t.inclusion_proof(index, size).unwrap(),
            expected,
            "{index} in {size}"
        );
        let leaf = t.leaf(index).unwrap();
        verify_inclusion(&leaf, index, size, &expected, &roots[size as usize]).unwrap();
    }
}

// covers: L-11
#[test]
fn rfc_6962_consistency_proofs() {
    let v = vectors();
    let t = tree(&v);
    let roots = hashes(&v["rfc6962"]["roots"]);
    for case in v["rfc6962"]["consistency"].as_array().unwrap() {
        let old = case["old"].as_u64().unwrap();
        let new = case["new"].as_u64().unwrap();
        let expected = hashes(&case["proof"]);
        assert_eq!(
            t.consistency_proof(old, new).unwrap(),
            expected,
            "{old} -> {new}"
        );
        verify_consistency(
            old,
            new,
            &expected,
            &roots[old as usize],
            &roots[new as usize],
        )
        .unwrap();
    }
}

// covers: L-11
#[test]
fn notes_like_go() {
    let v = vectors();
    let peter = &v["notes"]["peter"];
    let signer = Signer::parse(peter["skey"].as_str().unwrap()).unwrap();
    assert_eq!(signer.verifier().vkey(), peter["vkey"].as_str().unwrap());
    assert_eq!(
        signer.sign(peter["text"].as_str().unwrap()).unwrap(),
        peter["note"].as_str().unwrap()
    );
    let verifier = Verifier::parse(peter["vkey"].as_str().unwrap()).unwrap();
    let opened = note::open(peter["note"].as_str().unwrap().as_bytes(), &[&verifier]).unwrap();
    assert_eq!(opened.text, peter["text"].as_str().unwrap());
}

// covers: L-11
#[test]
fn checkpoints_and_listings_like_the_reference() {
    let v = vectors();
    let log = &v["notes"]["log"];
    let origin = log["origin"].as_str().unwrap();
    let seed: [u8; 32] = decode_hex(log["seed"].as_str().unwrap())
        .try_into()
        .unwrap();
    let signer = Signer::new(origin, seed).unwrap();
    assert_eq!(signer.skey(), log["skey"].as_str().unwrap());
    assert_eq!(signer.verifier().vkey(), log["vkey"].as_str().unwrap());
    let verifier = Verifier::parse(log["vkey"].as_str().unwrap()).unwrap();

    let t = tree(&v);
    let checkpoint = Checkpoint::new(origin, 8, t.root());
    let signed = SignedCheckpoint::sign(checkpoint.clone(), &signer).unwrap();
    assert_eq!(signed.note, log["checkpoint"].as_str().unwrap());
    let opened = SignedCheckpoint::open(log["checkpoint"].as_str().unwrap(), &verifier).unwrap();
    assert_eq!(opened.checkpoint, checkpoint);

    let keys: Vec<String> = log["listing_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    let prefix = keys[0].rsplit_once('/').unwrap().0.to_string() + "/";
    let listing = Listing::new(&checkpoint, &prefix, None, None, &keys).unwrap();
    let signed = SignedListing::sign(listing.clone(), &signer).unwrap();
    assert_eq!(signed.note, log["listing"].as_str().unwrap());
    let opened = SignedListing::open(log["listing"].as_str().unwrap(), &verifier).unwrap();
    assert_eq!(opened.listing, listing);
    assert!(opened.listing.matches(&keys));

    let paged = SignedListing::open(log["paged_listing"].as_str().unwrap(), &verifier).unwrap();
    assert_eq!(
        paged.listing.after.as_deref(),
        Some("v1/deployed/api/production/a")
    );
    assert_eq!(
        paged.listing.next.as_deref(),
        Some("v1/deployed/api/production/b")
    );
    assert!(paged.listing.matches(&["v1/deployed/api/production/b"]));
}

// covers: L-11
#[test]
fn entries_like_the_reference() {
    let v = vectors();
    for case in v["entries"].as_array().unwrap() {
        let kind = EntryKind::parse(case["kind"].as_str().unwrap()).unwrap();
        let entry = Entry {
            kind,
            name: case["name"].as_str().unwrap().to_string(),
            sha256: hex(&case["sha256"]),
            gc: case["gc"].as_str().map(|h| Hash::from_hex(h).unwrap()),
        };
        assert_eq!(
            String::from_utf8(entry.encode()).unwrap(),
            case["encoded"].as_str().unwrap()
        );
        assert_eq!(entry.leaf_hash(), hex(&case["leaf"]));
        assert_eq!(
            Entry::decode(case["encoded"].as_str().unwrap().as_bytes()).unwrap(),
            entry
        );
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect()
}

// covers: L-12
#[test]
fn the_prefix_map_like_the_reference() {
    let v = vectors();
    let mut entries = Vec::new();
    for case in v["map"]["entries"].as_array().unwrap() {
        let prefix = case["prefix"].as_str().unwrap();
        let keys = strings(&case["keys"]);
        let entry = MapEntry::of_keys(prefix, &keys).unwrap();
        assert_eq!(entry.text(), case["text"].as_str().unwrap());
        assert_eq!(entry.leaf_hash(), hex(&case["leaf"]));
        entries.push(entry);
    }
    let map = Map::from_entries(entries.iter().cloned());
    let root = hex(&v["map"]["root"]);
    assert_eq!(map.root(), root);
    assert_eq!(Map::from_entries(entries.into_iter().rev()).root(), root);
    for case in v["map"]["proofs"].as_array().unwrap() {
        let prefix = case["prefix"].as_str().unwrap();
        let leaf = case["leaf"].as_object().map(|leaf| MapEntry {
            prefix: leaf["prefix"].as_str().unwrap().to_string(),
            count: leaf["count"].as_u64().unwrap(),
            root: hex(&leaf["root"]),
        });
        let expected = MapProof {
            path: hashes(&case["path"]),
            leaf,
        };
        assert_eq!(map.prove(prefix), expected, "{prefix}");
        let found = expected.verify(prefix, &root).unwrap();
        assert_eq!(found.as_ref(), map.get(prefix), "{prefix}");
    }
}

// covers: L-12
#[test]
fn key_trees_and_their_pages_like_the_reference() {
    let v = vectors();
    let tree = &v["map"]["key_tree"];
    let keys = strings(&tree["keys"]);
    let root = hex(&tree["root"]);
    assert_eq!(keys_root(&keys), root);
    let leaves: Vec<Hash> = keys
        .iter()
        .map(|k| hushrun_tlog::map::key_leaf(k))
        .collect();
    let mem = MemTree::from_leaves(leaves.iter().copied());
    for page in tree["pages"].as_array().unwrap() {
        let begin = page["begin"].as_u64().unwrap();
        let end = page["end"].as_u64().unwrap();
        let (left, right) = (hashes(&page["left"]), hashes(&page["right"]));
        assert_eq!(
            mem.range_proof(begin, end, keys.len() as u64).unwrap(),
            (left.clone(), right.clone()),
            "{begin}..{end}"
        );
        verify_range(
            keys.len() as u64,
            begin,
            &leaves[begin as usize..end as usize],
            &left,
            &right,
            &root,
        )
        .unwrap();
    }
}

/// The reference agrees with itself, when Python is here to run it.
#[test]
fn the_reference_passes_its_selftest() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/reftlog.py");
    let Ok(output) = std::process::Command::new("python3")
        .args([script, "selftest"])
        .output()
    else {
        eprintln!("python3 isn't available; skipping the reference's selftest");
        return;
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

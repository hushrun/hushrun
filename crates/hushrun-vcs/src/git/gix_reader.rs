//! The gix object reader (D-9): parallel object reads, one repository handle per thread.

use std::path::Path;

use rayon::prelude::*;

use hushrun_digest::Sha256;
use hushrun_digest::hash::sha256;

use super::cli::TreeRecord;
use crate::Error;

fn gix_err(err: impl std::fmt::Display) -> Error {
    Error::Gix(err.to_string())
}

pub(super) fn open(root: &Path) -> Result<gix::ThreadSafeRepository, Error> {
    gix::open_opts(root, gix::open::Options::isolated())
        .map(gix::Repository::into_sync)
        .map_err(gix_err)
}

pub(super) fn object_format(repo: &gix::ThreadSafeRepository) -> String {
    match repo.to_thread_local().object_hash() {
        gix::hash::Kind::Sha1 => "sha1".to_string(),
        _ => "sha256".to_string(),
    }
}

/// The commit, tree and full ref name of `rev`.
pub(super) fn resolve(
    repo: &gix::ThreadSafeRepository,
    rev: &str,
) -> Result<(String, String, Option<String>), Error> {
    let repo = repo.to_thread_local();
    let unknown = |detail: String| Error::UnknownRevision {
        rev: rev.to_string(),
        detail,
    };
    let id = repo
        .rev_parse_single(rev)
        .map_err(|e| unknown(e.to_string()))?;
    let commit = id
        .object()
        .map_err(gix_err)?
        .peel_to_commit()
        .map_err(|e| unknown(e.to_string()))?;
    let tree = commit.tree_id().map_err(gix_err)?;
    let reference = if rev == "HEAD" {
        repo.head_name()
            .map_err(gix_err)?
            .map(|n| n.as_bstr().to_string())
    } else {
        repo.try_find_reference(rev)
            .ok()
            .flatten()
            .map(|r| r.name().as_bstr().to_string())
    };
    Ok((commit.id.to_string(), tree.to_string(), reference))
}

/// Every non-tree entry of `tree`, recursively.
pub(super) fn ls_tree(
    repo: &gix::ThreadSafeRepository,
    tree: &str,
) -> Result<Vec<TreeRecord>, Error> {
    let repo = repo.to_thread_local();
    let id = gix::ObjectId::from_hex(tree.as_bytes()).map_err(gix_err)?;
    let object = match repo.try_find_object(id).map_err(gix_err)? {
        Some(object) => object,
        None => {
            return Err(Error::MissingObjects {
                object: tree.to_string(),
            });
        }
    };
    let tree = object.peel_to_tree().map_err(gix_err)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder).map_err(|err| {
        let text = err.to_string();
        if text.contains("not found") || text.contains("could not be found") {
            Error::MissingObjects { object: text }
        } else {
            gix_err(text)
        }
    })?;
    Ok(recorder
        .records
        .into_iter()
        .filter(|e| !e.mode.is_tree())
        .map(|e| TreeRecord {
            mode: u32::from(e.mode.value()),
            oid: e.oid.as_slice().to_vec(),
            path: e.filepath.into(),
        })
        .collect())
}

/// SHA-256 and size of each blob, read in parallel, in order.
pub(super) fn hash_blobs(
    repo: &gix::ThreadSafeRepository,
    oids: &[&[u8]],
) -> Result<Vec<(Sha256, u64)>, Error> {
    oids.par_iter()
        .map_init(
            || repo.to_thread_local(),
            |repo, oid| {
                let id = gix::ObjectId::try_from(*oid).map_err(gix_err)?;
                let object = repo.try_find_object(id).map_err(gix_err)?.ok_or_else(|| {
                    Error::MissingObjects {
                        object: id.to_string(),
                    }
                })?;
                if object.kind != gix::object::Kind::Blob {
                    return Err(Error::Gix(format!("{id} is a {}, not a blob", object.kind)));
                }
                Ok((sha256(&object.data), object.data.len() as u64))
            },
        )
        .collect()
}

/// The bytes of one blob.
pub(super) fn read_blob(repo: &gix::ThreadSafeRepository, oid: &[u8]) -> Result<Vec<u8>, Error> {
    let repo = repo.to_thread_local();
    let id = gix::ObjectId::try_from(oid).map_err(gix_err)?;
    let object =
        repo.try_find_object(id)
            .map_err(gix_err)?
            .ok_or_else(|| Error::MissingObjects {
                object: id.to_string(),
            })?;
    Ok(object.detach().data)
}

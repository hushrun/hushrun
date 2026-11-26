//! Writing a package store's rows and `packages.json` from the parts' folders a bake filled.

use std::fs;
use std::io;
use std::path::Path;

use hushrun_core::blocks::Hex32;

use crate::store::{
    self, Class, PACKAGES_JSON, PARTS, Packages, PartEntry, ROWS, Row, RowsFile, SCHEMA, VerifiedBy,
};

/// What gives a row its `package` and `locked` (K-3), from the part's name and the row's path.
pub type Label<'a> = &'a dyn Fn(&str, &str) -> (Option<String>, Option<String>);

/// What a bake knows of one part before its files are walked.
#[derive(Debug, Clone)]
pub struct PartMeta {
    /// Its name.
    pub part: String,
    /// What its files are.
    pub class: Class,
    /// The tool's version line, or `harvested`.
    pub tool: String,
    /// What a built part is bound to.
    pub runtime: Option<String>,
    /// Its manifest set, as paths from the checkout's root with the SHA-256 of their bytes.
    pub manifests: Vec<(String, Hex32)>,
    /// What checks its bytes.
    pub verified_by: VerifiedBy,
}

/// What a store is for.
#[derive(Debug, Clone)]
pub struct StoreMeta {
    /// The repository, as a run names it, when known.
    pub repository: Option<String>,
    /// The platform, in a tag's words.
    pub platform: String,
    /// When it was baked.
    pub baked_ms: u64,
}

/// Walks `folder/parts/<part>/` for each of `parts`, writes `rows/<part>.jsonl` and
/// `packages.json`, and returns what it wrote. `label` gives a row its `package` and `locked`
/// (K-3) from its path, where the part knows them.
pub fn seal(
    folder: &Path,
    store: &StoreMeta,
    parts: &[PartMeta],
    label: Label<'_>,
) -> io::Result<Packages> {
    fs::create_dir_all(folder.join(ROWS))?;
    let mut entries = Vec::new();
    let mut sorted: Vec<&PartMeta> = parts.iter().collect();
    sorted.sort_by(|a, b| a.part.as_bytes().cmp(b.part.as_bytes()));
    for meta in sorted {
        let dir = folder.join(PARTS).join(&meta.part);
        fs::create_dir_all(&dir)?;
        let mut rows = Vec::new();
        walk(&dir, "", &mut rows)?;
        for row in &mut rows {
            let (package, locked) = label(&meta.part, &row.path);
            row.package = package;
            row.locked = locked;
        }
        rows.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let bytes = store::rows_bytes(&rows);
        fs::write(
            folder.join(ROWS).join(format!("{}.jsonl", meta.part)),
            &bytes,
        )?;
        let mut manifests: Vec<String> = meta.manifests.iter().map(|(p, _)| p.clone()).collect();
        manifests.sort();
        entries.push(PartEntry {
            part: meta.part.clone(),
            class: meta.class,
            tool: meta.tool.clone(),
            runtime: meta.runtime.clone(),
            inputs: store::inputs_digest(&meta.part, &meta.tool, &store.platform, &meta.manifests),
            manifests,
            identity: store::part_identity(&meta.part, meta.class, meta.runtime.as_deref(), &rows),
            rows: RowsFile {
                sha256: Hex32::sha256(&bytes),
                bytes: bytes.len() as u64,
                count: rows.len() as u64,
            },
            bytes: rows.iter().map(|r| r.bytes).sum(),
            verified_by: meta.verified_by,
        });
    }
    let ids: Vec<(&str, Hex32)> = entries
        .iter()
        .map(|e| (e.part.as_str(), e.identity))
        .collect();
    let packages = Packages {
        schema: SCHEMA.to_string(),
        repository: store.repository.clone(),
        platform: store.platform.clone(),
        identity: store::store_identity(&ids),
        baked_ms: store.baked_ms,
        parts: entries,
    };
    fs::write(folder.join(PACKAGES_JSON), packages.to_bytes())?;
    Ok(packages)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<Row>) -> io::Result<()> {
    let mut names: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), &path, out)?;
        } else if let Some(row) = store::row_of(&entry.path(), &path)? {
            out.push(row);
        } else {
            return Err(io::Error::other(format!(
                "{path} is a special file, which a package store never holds"
            )));
        }
    }
    Ok(())
}

//! B-172: a module's `buildinfo.xml`, read as untrusted bytes, checked against the path it came
//! under, and kept in a canonical form.
//!
//! The extension writes the file with its own model's writer: a `build` root whose
//! `cacheImplementationVersion`, `projectsInputInfo/checksum` and `artifact` name the module,
//! whose `artifact` and `attachedArtifacts/attachedArtifact` elements name the files of the entry
//! (`fileName`, `fileSize`), and whose `buildTime` and `buildServer` change with every execution.
//! The canonical form is the same bytes with those two fixed, so that two executions that made
//! the same files make the same block. XML is read as a stream with quick-xml, which reads no DTD
//! and expands no entity; a document with a DTD is refused.

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{BUILDINFO, LAYOUT, MOST_FILES, Module, is_plain_name};

/// The most bytes a `buildinfo.xml` may hold: it holds the module's effective POM and the list
/// of its input files.
pub const MOST_BYTES: usize = 16 << 20;

/// What `buildTime` reads in the canonical form.
pub const BUILD_TIME: &str = "1970-01-01T00:00:00.000";
/// What `buildServer` reads in the canonical form.
pub const BUILD_SERVER: &str = "hushrun";

/// One file the entry names beside `buildinfo.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub size: u64,
}

/// A `buildinfo.xml` that passed its checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The files it names, in its order: the main artifact's first, when it has one.
    pub files: Vec<Named>,
    /// The extension's hash function (`hashFunction`), as written.
    pub hash_function: String,
    /// Its canonical form.
    pub canonical: Vec<u8>,
}

impl Checked {
    /// Whether the canonical `buildinfo.xml` is `bytes`: what a stored block must be (B-172).
    pub fn is_canonical(&self, bytes: &[u8]) -> bool {
        self.canonical == bytes
    }
}

/// The fields read, by their path below the root.
#[derive(Default)]
struct Fields {
    layout: Option<String>,
    checksum: Option<String>,
    group: Option<String>,
    artifact: Option<String>,
    hash_function: Option<String>,
    /// The file of the element being read (`artifact` or an `attachedArtifact`).
    file: Option<String>,
    size: Option<String>,
    files: Vec<(Option<String>, Option<String>)>,
}

/// Reads `bytes` as the `buildinfo.xml` of `module` (B-172): UTF-8, at most [`MOST_BYTES`], one
/// `build` root, every element closed, no DTD; its layout [`LAYOUT`]; its checksum, group and
/// artifact the module's; each file it names a plain name, not `buildinfo.xml`, named once, with
/// a size; at most [`MOST_FILES`] of them.
pub fn check(bytes: &[u8], module: &Module) -> Result<Checked, String> {
    if bytes.len() > MOST_BYTES {
        return Err("its buildinfo.xml is larger than 16 MiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "its buildinfo.xml isn't UTF-8")?;
    let mut reader = Reader::from_str(text);
    let config = reader.config_mut();
    config.check_end_names = true;
    config.expand_empty_elements = false;
    config.trim_text(false);
    let mut path: Vec<String> = Vec::new();
    let mut fields = Fields::default();
    let mut roots = 0;
    // The text being read of a field, and whether it held anything but text.
    let mut value = String::new();
    let mut mixed = false;
    // The byte ranges of `buildTime`'s and `buildServer`'s contents, to replace.
    let mut replace: Vec<(usize, usize, &str)> = Vec::new();
    let mut opened_at = 0usize;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|e| format!("its buildinfo.xml doesn't parse: {e}"))?;
        match event {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                if path.is_empty() {
                    roots += 1;
                    if name != "build" || roots > 1 {
                        return Err("its buildinfo.xml has no one build element".into());
                    }
                }
                path.push(name);
                value.clear();
                mixed = false;
                opened_at = reader.buffer_position() as usize;
                if matches!(
                    at(&path).as_str(),
                    "artifact" | "attachedArtifacts/attachedArtifact"
                ) {
                    fields.file = None;
                    fields.size = None;
                }
            }
            Event::Empty(e) => {
                if path.is_empty() {
                    return Err("its buildinfo.xml has no one build element".into());
                }
                let name = e.local_name().as_ref().to_string();
                path.push(name);
                if matches!(
                    at(&path).as_str(),
                    "artifact" | "attachedArtifacts/attachedArtifact"
                ) {
                    fields.files.push((None, None));
                }
                path.pop();
            }
            Event::Text(e) => {
                // Read from a `str`: UTF-8 already.
                let text: &str = e.as_ref();
                value.push_str(text);
            }
            Event::CData(_) | Event::GeneralRef(_) | Event::Comment(_) | Event::PI(_) => {
                mixed = true;
            }
            Event::End(_) => {
                let here = at(&path);
                let read = (!mixed).then(|| value.trim().to_string());
                match here.as_str() {
                    "cacheImplementationVersion" => fields.layout = read,
                    "hashFunction" => fields.hash_function = read,
                    "projectsInputInfo/checksum" => fields.checksum = read,
                    "artifact/groupId" => fields.group = read,
                    "artifact/artifactId" => fields.artifact = read,
                    "artifact/fileName" | "attachedArtifacts/attachedArtifact/fileName" => {
                        fields.file = Some(read.ok_or("a file name that isn't plain text")?);
                    }
                    "artifact/fileSize" | "attachedArtifacts/attachedArtifact/fileSize" => {
                        fields.size = Some(read.ok_or("a file size that isn't plain text")?);
                    }
                    "artifact" | "attachedArtifacts/attachedArtifact" => {
                        fields.files.push((fields.file.take(), fields.size.take()));
                    }
                    "buildTime" => replace.push((opened_at, before, BUILD_TIME)),
                    "buildServer" => replace.push((opened_at, before, BUILD_SERVER)),
                    _ => {}
                }
                value.clear();
                mixed = false;
                path.pop();
            }
            Event::DocType(_) => return Err("its buildinfo.xml has a DTD".into()),
            Event::Decl(_) => {}
            Event::Eof => break,
        }
    }
    if !path.is_empty() || roots != 1 {
        return Err("its buildinfo.xml ends before its elements do".into());
    }
    if fields.layout.as_deref() != Some(LAYOUT) {
        return Err(format!("its buildinfo.xml isn't of the layout {LAYOUT}"));
    }
    if fields.checksum.as_deref() != Some(module.checksum.as_str()) {
        return Err("its buildinfo.xml is of another checksum".into());
    }
    if fields.group.as_deref() != Some(module.group.as_str())
        || fields.artifact.as_deref() != Some(module.artifact.as_str())
    {
        return Err("its buildinfo.xml is of another module".into());
    }
    let mut files: Vec<Named> = Vec::new();
    for (name, size) in fields.files {
        // An artifact with no file (a `pom` module's) is no file of the entry.
        let Some(name) = name else {
            continue;
        };
        if !is_plain_name(&name) || name == BUILDINFO {
            return Err(format!("its buildinfo.xml names the file {name:?}"));
        }
        if files.iter().any(|f| f.name == name) {
            return Err(format!("its buildinfo.xml names {name} twice"));
        }
        let size: u64 = size
            .as_deref()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("its buildinfo.xml gives {name} no size"))?;
        files.push(Named { name, size });
    }
    if files.len() > MOST_FILES {
        return Err(format!(
            "its buildinfo.xml names more than {MOST_FILES} files"
        ));
    }
    let mut canonical = Vec::with_capacity(bytes.len());
    let mut from = 0usize;
    replace.sort_by_key(|(start, _, _)| *start);
    for (start, end, with) in replace {
        if start < from || end < start {
            return Err("its buildinfo.xml repeats its build time".into());
        }
        canonical.extend_from_slice(&bytes[from..start]);
        canonical.extend_from_slice(with.as_bytes());
        from = end;
    }
    canonical.extend_from_slice(&bytes[from..]);
    Ok(Checked {
        files,
        hash_function: fields.hash_function.unwrap_or_default(),
        canonical,
    })
}

/// The path below the root of the element `path` ends with.
fn at(path: &[String]) -> String {
    path.get(1..).unwrap_or_default().join("/")
}

#[cfg(test)]
mod tests;

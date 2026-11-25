//! \[ADR 0107\] What the adapters of Node's test runner and Mocha share: the sources they observe
//! being read, and how COMMAND's Node loads hushrun's adapter. Here, not in either adapter's file,
//! so that a test of one reaches nothing of the other (CONTRIBUTING.md, "Which tests CI runs
//! again").

use std::ffi::OsString;
use std::path::Path;

/// What the adapters observe being read: their sources (C-30). Every other kind is rest.
pub const SOURCE_KINDS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];

/// `NODE_OPTIONS` with ` --require <file>` after what it had (C-54, C-56): in double quotes,
/// backslashes doubled, when the path has whitespace, as Node splits it; a path with a quote can't
/// be held.
pub fn require(file: &Path, node_options: Option<OsString>) -> Result<OsString, String> {
    let text = file.to_string_lossy().into_owned();
    if text.contains('"') {
        return Err(format!(
            "its file's path, {text}, has a quote, which NODE_OPTIONS can't hold"
        ));
    }
    let word = if text.chars().any(char::is_whitespace) {
        format!("\"{}\"", text.replace('\\', "\\\\"))
    } else {
        text
    };
    let mut options = node_options.filter(|o| !o.is_empty()).unwrap_or_default();
    if !options.is_empty() {
        options.push(" ");
    }
    options.push(format!("--require {word}"));
    Ok(options)
}

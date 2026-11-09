//! What init needs from a clone's remotes: the default branch the clone recorded, without
//! asking the remote.

use std::path::Path;

use super::cli::run_text;

/// The default branch of the remote named `remote` as the checkout recorded it when it was
/// cloned (`refs/remotes/<remote>/HEAD`), without asking the remote. `None` when it recorded
/// none.
pub fn recorded_default_branch(root: &Path, remote: &str) -> Option<String> {
    if remote.is_empty() || remote.starts_with('-') || remote.contains(['/', ':']) {
        return None;
    }
    let local = format!("refs/remotes/{remote}/HEAD");
    let target = run_text(root, ["symbolic-ref", "--quiet", &local], &[]).ok()?;
    target
        .strip_prefix(&format!("refs/remotes/{remote}/"))
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
}

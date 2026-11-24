//! B-77 to B-83: Gradle's tasks as build blocks, through Gradle's own HTTP build cache.
//!
//! Gradle keys each cacheable task by its own **build cache key** (the hash of the task's
//! implementation, its inputs by content and its normalized paths), keeps entries in a local
//! cache of its own, and asks a **remote** cache on a miss with `GET {url}{key}` and stores with
//! `PUT {url}{key}` ([`HttpBuildCache`](https://docs.gradle.org/current/userguide/build_cache.html)).
//! hushrun is that remote for a run that shares (B-26): a listener on the loopback in the
//! `hushrun run` process ([`cache`]) answers from this machine's store, the run's relay and,
//! through it, the org's shared store, as kind `gradle/1`. Gradle's local cache stays Gradle's:
//! plain `./gradlew` and `hushrun run` are as warm as each other on one machine, and hushrun
//! only adds what other machines built.
//!
//! - **Loading** is the line ADR 0087 already asks for in the settings file: `HUSHRUN_GRADLE`
//!   names a script that, after the settings file is evaluated, gives the build a remote cache
//!   named by [`CACHE_VARIABLE`] and [`TOKEN_VARIABLE`], unless the build set one itself
//!   ([`SCRIPT`]).
//! - **The address** ([`endpoint`]) is the same from run to run, a port and a token kept in the
//!   store, so Gradle's configuration cache, which records the variables a settings script
//!   reads, is reused from run to run.
//! - **An entry** ([`entry`]) is checked whole and kept in a canonical form, whose digest of
//!   outputs is the same wherever a task made the same outputs.

pub mod cache;
pub mod endpoint;
pub mod entry;
pub mod http;

use hushrun_core::Digest;

/// The kind on the wire (B-78).
pub const KIND: &str = "gradle/1";

/// The block's one file: the canonical entry (B-78).
pub const ENTRY: &str = "entry.tar";

/// What the run sets for Gradle: the URL of hushrun's cache (B-77).
pub const CACHE_VARIABLE: &str = "HUSHRUN_GRADLE_CACHE";

/// The password Gradle sends hushrun's cache, as Basic authentication with the user
/// [`USER`] (B-77).
pub const TOKEN_VARIABLE: &str = "HUSHRUN_GRADLE_CACHE_TOKEN";

/// The variable that loads hushrun's Gradle script, ADR 0087's.
pub const SCRIPT_VARIABLE: &str = "HUSHRUN_GRADLE";

/// The user name Gradle sends.
pub const USER: &str = "hushrun";

/// The path the cache answers under: Gradle asks `{url}{key}`.
pub const PATH: &str = "/cache/";

/// The script that gives a build hushrun's cache (B-77). ADR 0087's test script ends with the
/// same text, so a run that sets either gets it.
pub const SCRIPT: &str = include_str!("hushrun_gradle_cache.gradle");

/// Writes [`SCRIPT`] whole into the store, when it isn't there yet, at a path named by its
/// digest: `<store>/gradle/cache-<first 16 hex>.gradle`. The same path from run to run of one
/// hushrun, so Gradle's configuration cache holds (B-77).
pub fn install_script(store: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let digest = Digest::of(SCRIPT.as_bytes()).hex();
    let dir = store.join("gradle");
    let path = dir.join(format!("cache-{}.gradle", &digest[..16]));
    if std::fs::read(&path).ok().as_deref() == Some(SCRIPT.as_bytes()) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!(".cache.{}.tmp", std::process::id()));
    std::fs::write(&temp, SCRIPT)?;
    std::fs::rename(&temp, &path)?;
    Ok(path)
}

/// The platform a `gradle/1` manifest is asked and closed under (B-82): Gradle's keys hold what
/// a task's outputs depend on, the operating system included where a task declares it, so one
/// manifest serves every machine.
pub const PLATFORM: &str = "jvm";

/// The key of the block for Gradle's key `gradle_key` (B-78), which is also its first key:
/// `None` for a key that isn't 32 to 128 lowercase hex characters, which Gradle never sends.
pub fn key(gradle_key: &str) -> Option<Digest> {
    is_gradle_key(gradle_key)
        .then(|| Digest::of(format!("hushrun.gradle-key/v1\n{gradle_key}").as_bytes()))
}

/// Whether `text` has the form of a Gradle build cache key: 32 to 128 lowercase hex characters,
/// an even number of them (Gradle's keys are MD5 today, 32).
pub fn is_gradle_key(text: &str) -> bool {
    (32..=128).contains(&text.len())
        && text.len().is_multiple_of(2)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A Gradle block's digest of outputs (B-27, B-78): its one file, the canonical entry.
pub fn outputs_of(size: u64, sha256: &Digest) -> hushrun_core::blocks::Hex32 {
    hushrun_core::blocks::outputs_digest(&[hushrun_core::blocks::OutputFile {
        name: ENTRY,
        size,
        sha256: hushrun_core::blocks::Hex32(*sha256.as_bytes()),
    }])
}

/// What a run keeps for Gradle in `session.json` (B-79): the folders an entry is searched for
/// before it may leave the machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct GradleRun {
    /// The checkout's folder, as given and resolved.
    pub roots: Vec<String>,
    /// Gradle's user home (`GRADLE_USER_HOME`, else `~/.gradle`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_home: Option<String>,
}

/// Why a Gradle entry this run stored stays on this machine (B-79), in the run's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Local {
    /// Its outputs hold the checkout's folder.
    Checkout,
    /// They hold the home folder.
    Home,
    /// They hold Gradle's user home.
    GradleHome,
    /// They hold the store's folder.
    Store,
}

impl Local {
    pub fn words(self) -> &'static str {
        match self {
            Local::Checkout => "its outputs hold the checkout's folder",
            Local::Home => "its outputs hold the home folder",
            Local::GradleHome => "its outputs hold Gradle's user home",
            Local::Store => "its outputs hold hushrun's store",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-78
    #[test]
    fn a_key_is_gradle_s_in_its_own_namespace_and_only_its_form_is_taken() {
        let gradle = "49b7acd36407da65ca900b05e84a6241";
        let key = key(gradle).unwrap();
        assert_eq!(
            key,
            Digest::of(b"hushrun.gradle-key/v1\n49b7acd36407da65ca900b05e84a6241")
        );
        assert!(is_gradle_key(&"a".repeat(64)));
        for bad in [
            "",
            "49B7ACD36407DA65CA900B05E84A6241",
            "49b7acd36407da65ca900b05e84a624",
            "../../etc/passwd/aaaaaaaaaaaaaaaaaaaaaa",
            &"a".repeat(130),
        ] {
            assert!(super::key(bad).is_none(), "{bad}");
        }
    }
}

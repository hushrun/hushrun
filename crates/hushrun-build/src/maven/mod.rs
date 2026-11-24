//! B-170 to B-177: Maven's modules as build blocks, through the remote cache of Apache's
//! [build cache extension](https://maven.apache.org/extensions/maven-build-cache-extension/).
//!
//! The extension keys each module of a reactor by a **checksum** of its inputs (its effective
//! POM, its source files by content and relative path, the checksums of the reactor modules it
//! depends on), keeps entries in a local cache of its own (`~/.m2/build-cache`), and asks a
//! **remote** cache over HTTP: `GET {url}/v1.1/<group>/<artifact>/<checksum>/buildinfo.xml`, then
//! each artifact file that `buildinfo.xml` names; after it built a module it `PUT`s each artifact
//! file, then `buildinfo.xml`. hushrun is that remote for a run that shares (B-26): a listener on
//! the loopback in the `hushrun run` process ([`cache`]) answers from this machine's store, the
//! run's relay and, through it, the org's shared store, as kind `maven/1`. The extension's local
//! cache stays its own.
//!
//! - **The switch** is the build's: a build whose `.mvn/extensions.xml` loads the extension.
//!   hushrun loads nothing of its own into Maven for it.
//! - **The address** is given through `MAVEN_OPTS` ([`properties`]): the URL holds a token made
//!   for the run, since Maven sends no credential of its own to a remote it is given this way and
//!   prints the URL of what it downloads.
//! - **A block** ([`buildinfo`]) is one module's entry: `buildinfo.xml`, checked whole and kept
//!   in a canonical form, and the files it names, each as Maven sent it.

pub mod buildinfo;
pub mod cache;

use hushrun_core::Digest;

/// The kind on the wire (B-171).
pub const KIND: &str = "maven/1";

/// The one entry layout this cache answers: the extension's `cacheImplementationVersion`
/// (B-171). The extension puts it first in every path.
pub const LAYOUT: &str = "v1.1";

/// The block's file that describes the module's build (B-172).
pub const BUILDINFO: &str = "buildinfo.xml";

/// The file the extension puts at the end of a build, under a build's id: answered and dropped
/// (B-175).
pub const REPORT: &str = "build-cache-report.xml";

/// The extension's properties that give it hushrun's cache (B-170).
pub const ENABLED_PROPERTY: &str = "maven.build.cache.remote.enabled";
pub const URL_PROPERTY: &str = "maven.build.cache.remote.url";
pub const SAVE_PROPERTY: &str = "maven.build.cache.remote.save.enabled";

/// The variable that carries the properties to Maven: the `mvn` scripts and the Maven
/// Wrapper's both pass it to the JVM, so they are system properties, which the extension reads
/// (B-170).
pub const OPTS_VARIABLE: &str = "MAVEN_OPTS";

/// The extension's coordinates, as `.mvn/extensions.xml` names it (B-170).
pub const EXTENSION_ARTIFACT: &str = "maven-build-cache-extension";

/// The platform a `maven/1` manifest is asked and closed under (B-176): the extension's
/// checksum holds the effective POM, profiles activated by the operating system included, so one
/// manifest serves every machine, as Gradle's does.
pub const PLATFORM: &str = "jvm";

/// The most files a module's entry may name beside `buildinfo.xml` (B-172).
pub const MOST_FILES: usize = 64;

/// The words `MAVEN_OPTS` gains for the cache at `url` (B-170): the remote on, at `url`, with
/// saving on, so that what the build stores comes to hushrun, which decides what leaves.
pub fn properties(url: &str) -> String {
    format!("-D{ENABLED_PROPERTY}=true -D{URL_PROPERTY}={url} -D{SAVE_PROPERTY}=true")
}

/// B-170: `opts`, a `MAVEN_OPTS` another part of hushrun made from the person's (the Maven test
/// adapter's, ADR 0088), with the cache's properties that `build`, the build's `MAVEN_OPTS`,
/// holds, when it doesn't hold them already: COMMAND gets one `MAVEN_OPTS`.
pub fn with_cache_properties(opts: &str, build: &str) -> String {
    let mut out = opts.trim().to_string();
    for word in build
        .split_whitespace()
        .filter(|w| w.starts_with("-Dmaven.build.cache."))
    {
        if !out.split_whitespace().any(|w| w == word) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
        }
    }
    out
}

/// B-175: the property among `words` (a command's, or a `MAVEN_OPTS`'s split at spaces) that
/// turns Maven's tests off, `skipTests` or `maven.test.skip`, when one does: `-Dname`,
/// `-Dname=true` (any case), `-D name…` and `--define name…`, the last of each winning.
pub fn skips_tests<'a>(words: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let mut skipped: Vec<(&'static str, bool)> = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        let set = if word == "-D" || word == "--define" {
            words.next()
        } else if let Some(rest) = word.strip_prefix("--define=") {
            Some(rest)
        } else {
            word.strip_prefix("-D")
        };
        let Some(set) = set else {
            continue;
        };
        let (name, value) = set.split_once('=').unwrap_or((set, "true"));
        for property in ["skipTests", "maven.test.skip"] {
            if name == property {
                skipped.retain(|(p, _)| *p != property);
                skipped.push((property, value.eq_ignore_ascii_case("true")));
            }
        }
    }
    skipped
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(property, _)| property)
}

/// A request's path, read (B-171): the run's token, then the layout, group, artifact, checksum
/// and file name. Empty parts are read past, since Maven joins a URL ending in `/` with one more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked<'a> {
    pub token: &'a str,
    pub module: Module,
    pub file: &'a str,
}

/// One module's build, as the extension names it: what a block is the entry of.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Module {
    pub group: String,
    pub artifact: String,
    pub checksum: String,
}

impl Module {
    /// The key of the module's block (B-171), which is also its first key.
    pub fn key(&self) -> Digest {
        Digest::of(
            format!(
                "hushrun.maven-key/v1\n{LAYOUT}\n{}\n{}\n{}",
                self.group, self.artifact, self.checksum
            )
            .as_bytes(),
        )
    }

    /// `group:artifact`, the module's name in the run's log.
    pub fn name(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

/// Reads `path` as a request of the extension's, or `None` (a 404): a layout other than
/// [`LAYOUT`], a group or artifact of other characters than Maven's (letters, digits, `.`, `-`
/// and `_`), a checksum that isn't 16 to 128 lowercase hex characters, or a file name that isn't
/// [`is_plain_name`]. The token is returned unchecked.
pub fn read_path(path: &str) -> Option<Asked<'_>> {
    let path = path.split(['?', '#']).next()?;
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let token = parts.next()?;
    let layout = parts.next()?;
    let group = parts.next()?;
    let artifact = parts.next()?;
    let checksum = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some()
        || layout != LAYOUT
        || !is_coordinate(group)
        || !is_coordinate(artifact)
        || !is_checksum(checksum)
        || !is_plain_name(file)
    {
        return None;
    }
    Some(Asked {
        token,
        module: Module {
            group: group.to_string(),
            artifact: artifact.to_string(),
            checksum: checksum.to_string(),
        },
        file,
    })
}

/// Whether `text` is a group or artifact id as Maven allows them.
pub fn is_coordinate(text: &str) -> bool {
    (1..=255).contains(&text.len())
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Whether `text` has the form of the extension's checksum of a module: 16 to 128 lowercase hex
/// characters (its default hash, `XX`, gives 16; `SHA-256` 64).
pub fn is_checksum(text: &str) -> bool {
    (16..=128).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `text` is a file name a block may hold: one part, of letters, digits, `.`, `-`, `_`
/// and `+`, neither `.` nor `..`, at most 255 bytes.
pub fn is_plain_name(text: &str) -> bool {
    (1..=255).contains(&text.len())
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

/// A Maven block's digest of outputs (B-27, B-172): every file it holds.
pub fn outputs_of(block: &crate::store::Block) -> hushrun_core::blocks::Hex32 {
    let files: Vec<hushrun_core::blocks::OutputFile<'_>> = block
        .files
        .iter()
        .map(|file| hushrun_core::blocks::OutputFile {
            name: &file.name,
            size: file.size,
            sha256: hushrun_core::blocks::Hex32(*file.sha256.as_bytes()),
        })
        .collect();
    hushrun_core::blocks::outputs_digest(&files)
}

/// What a run keeps for Maven in `session.json` (B-173): the folders an entry is searched for
/// before it may leave the machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MavenRun {
    /// The checkout's folder, as given and resolved.
    pub roots: Vec<String>,
    /// Maven's local repository, when the run knows it (`maven.repo.local`, else `~/.m2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

/// Why a Maven entry this run stored stays on this machine (B-173), in the run's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Local {
    /// Its files hold the checkout's folder.
    Checkout,
    /// They hold Maven's local repository.
    Repository,
    /// They hold the store's folder.
    Store,
    /// They hold the home folder.
    Home,
}

impl Local {
    pub fn words(self) -> &'static str {
        match self {
            Local::Checkout => "its files hold the checkout's folder",
            Local::Repository => "its files hold Maven's local repository",
            Local::Store => "its files hold hushrun's store",
            Local::Home => "its files hold the home folder",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-171
    #[test]
    fn a_path_is_the_extension_s_and_its_key_names_the_module_in_hushrun_s_namespace() {
        let asked =
            read_path("/0123abcd/v1.1/demo.shop/core/07b43d4938bbe3e1/buildinfo.xml").unwrap();
        assert_eq!(asked.token, "0123abcd");
        assert_eq!(asked.file, "buildinfo.xml");
        assert_eq!(asked.module.name(), "demo.shop:core");
        assert_eq!(
            asked.module.key(),
            Digest::of(b"hushrun.maven-key/v1\nv1.1\ndemo.shop\ncore\n07b43d4938bbe3e1")
        );
        // Maven joins a URL that ends in `/` with another.
        let doubled = read_path("/0123abcd//v1.1/demo.shop/core/07b43d4938bbe3e1/core.jar");
        assert_eq!(doubled.unwrap().file, "core.jar");
        for bad in [
            "/t/v1/demo/core/07b43d4938bbe3e1/buildinfo.xml",
            "/t/v1.1/demo/core/07B43D4938BBE3E1/buildinfo.xml",
            "/t/v1.1/demo/core/07b43d49/buildinfo.xml",
            "/t/v1.1/de%2fmo/core/07b43d4938bbe3e1/buildinfo.xml",
            "/t/v1.1/../core/07b43d4938bbe3e1/buildinfo.xml",
            "/t/v1.1/demo/core/07b43d4938bbe3e1/..",
            "/t/v1.1/demo/core/07b43d4938bbe3e1/a/b.jar",
            "/t/v1.1/demo/core/07b43d4938bbe3e1",
        ] {
            assert!(read_path(bad).is_none(), "{bad}");
        }
        // Another module, another checksum, another key.
        let other = Module {
            checksum: "07b43d4938bbe3e2".into(),
            ..asked.module.clone()
        };
        assert_ne!(other.key(), asked.module.key());
    }

    // covers: B-170
    #[test]
    fn the_test_adapter_s_maven_opts_gains_the_cache_s_properties() {
        let build = format!("-Xmx1g {}", properties("http://127.0.0.1:4000/abc"));
        let joined = with_cache_properties("-Xmx1g -Dmaven.ext.class.path=/c/ext.jar", &build);
        assert_eq!(
            joined,
            format!(
                "-Xmx1g -Dmaven.ext.class.path=/c/ext.jar {}",
                properties("http://127.0.0.1:4000/abc")
            )
        );
        assert_eq!(with_cache_properties(&joined, &build), joined);
        assert_eq!(with_cache_properties("-Da=b", "-Xmx1g"), "-Da=b");
    }

    // covers: B-175
    #[test]
    fn a_command_that_skips_tests_is_seen_in_each_form() {
        let skips = |text: &str| skips_tests(text.split_whitespace());
        assert_eq!(skips("mvn -B -DskipTests package"), Some("skipTests"));
        assert_eq!(skips("mvn -DskipTests=TRUE verify"), Some("skipTests"));
        assert_eq!(
            skips("mvn -D maven.test.skip package"),
            Some("maven.test.skip")
        );
        assert_eq!(
            skips("mvn --define=maven.test.skip=true"),
            Some("maven.test.skip")
        );
        assert_eq!(skips("mvn -DskipTests -DskipTests=false verify"), None);
        assert_eq!(skips("mvn -DskipTests=false -DskipITs verify"), None);
        assert_eq!(skips("mvn verify"), None);
    }

    // covers: B-170
    #[test]
    fn maven_is_given_the_remote_with_saving_on() {
        assert_eq!(
            properties("http://127.0.0.1:4000/abc"),
            "-Dmaven.build.cache.remote.enabled=true -Dmaven.build.cache.remote.url=http://127.0.0.1:4000/abc -Dmaven.build.cache.remote.save.enabled=true"
        );
    }
}

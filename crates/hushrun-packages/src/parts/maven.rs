//! The `maven` part: a Maven local repository, read in place as `maven.repo.local.tail` (Maven 3.9
//! or later), a JVM system property carried in `MAVEN_OPTS`. Not `MAVEN_ARGS`: the Maven Wrapper's
//! launcher doesn't read it. An older Maven ignores the property, and downloads as it does today.

use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Env, Part, Reader, file_name, has_snapshot, splits_safely};
use crate::store::VerifiedBy;

/// The property that names a read-only tail of the local repository.
pub const TAIL: &str = "maven.repo.local.tail";
/// The part's folder under which the repository is.
pub const REPOSITORY: &str = "repository";

/// The `maven` part.
pub struct Maven;

impl Part for Maven {
    fn name(&self) -> &'static str {
        "maven"
    }

    fn tool(&self) -> &'static str {
        "Maven"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                file_name(path) == "pom.xml"
                    || matches!(
                        path.as_str(),
                        ".mvn/maven.config"
                            | ".mvn/extensions.xml"
                            | ".mvn/wrapper/maven-wrapper.properties"
                    )
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        cache.is_dir().then(|| (cache.to_path_buf(), REPOSITORY))
    }

    fn keep(&self, path: &str) -> bool {
        let name = file_name(path);
        !(name == "_remote.repositories"
            || name.ends_with(".lastUpdated")
            || name == "resolver-status.properties"
            || name.ends_with(".lock")
            || name.ends_with(".part")
            || path.split('/').any(|c| c.starts_with('.'))
            || has_snapshot(path))
    }

    fn package(&self, path: &str) -> Option<String> {
        // <group path>/<artifact>/<version>/<artifact>-<version>[-classifier].<ext>
        let parts: Vec<&str> = path.split('/').collect();
        let [group @ .., artifact, version, file] = parts.as_slice() else {
            return None;
        };
        if group.is_empty() || !file.starts_with(&format!("{artifact}-{version}")) {
            return None;
        }
        Some(format!("{}:{artifact}:{version}", group.join(".")))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn reader(&self, dir: &Path, checkout: &Path, env: Env<'_>) -> Reader {
        if let Some(place) = tail_named(checkout, env) {
            return Reader::LeftAlone(format!("{TAIL} is set already ({place})"));
        }
        let repository = dir.join(REPOSITORY);
        if !splits_safely(&repository) {
            return Reader::LeftAlone(format!(
                "the store's folder, {}, has a space, which MAVEN_OPTS can't hold",
                dir.display()
            ));
        }
        Reader::MavenOpts(format!("-D{TAIL}={}", repository.display()))
    }
}

/// Where the person already names the tail, if anywhere: `MAVEN_OPTS`, `MAVEN_ARGS`,
/// `.mvn/maven.config` or `.mvn/jvm.config`.
pub fn tail_named(checkout: &Path, env: Env<'_>) -> Option<String> {
    let names = |text: &str| text.contains(TAIL);
    for variable in ["MAVEN_OPTS", "MAVEN_ARGS"] {
        if env(variable).is_some_and(|v| names(&v.to_string_lossy())) {
            return Some(variable.to_string());
        }
    }
    for file in [".mvn/maven.config", ".mvn/jvm.config"] {
        if fs::read_to_string(checkout.join(file)).is_ok_and(|t| names(&t)) {
            return Some(file.to_string());
        }
    }
    None
}

/// K-10: `MAVEN_OPTS` as the person had it (`person`), then `word`: what `hushrun packages env`
/// and `exec` give Maven outside `hushrun run`.
pub fn opts_with(person: Option<&str>, word: &str) -> String {
    match person.map(str::trim).filter(|p| !p.is_empty()) {
        Some(person) => format!("{person} {word}"),
        None => word.to_string(),
    }
}

//! The `gradle` part: `caches/modules-2` of a Gradle user home, read in place through
//! `GRADLE_RO_DEP_CACHE` (Gradle 6.2 or later), which names the folder that holds `modules-2`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::part::{Env, Part, Reader, file_name, has_snapshot};
use crate::store::VerifiedBy;

/// Gradle's variable for a read-only dependency cache.
pub const VARIABLE: &str = "GRADLE_RO_DEP_CACHE";

/// The `gradle` part.
pub struct Gradle;

impl Part for Gradle {
    fn name(&self) -> &'static str {
        "gradle"
    }

    fn tool(&self) -> &'static str {
        "Gradle"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                let name = file_name(path);
                matches!(
                    name,
                    "settings.gradle" | "settings.gradle.kts" | "build.gradle" | "build.gradle.kts"
                ) || matches!(
                    path.as_str(),
                    "gradle/libs.versions.toml"
                        | "gradle/wrapper/gradle-wrapper.properties"
                        | "gradle.lockfile"
                        | "gradle/verification-metadata.xml"
                )
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join("caches/modules-2");
        root.is_dir().then_some((root, "modules-2"))
    }

    fn keep(&self, path: &str) -> bool {
        let name = file_name(path);
        !(name.ends_with(".lock") || name == "gc.properties" || has_snapshot(path))
    }

    fn package(&self, path: &str) -> Option<String> {
        // files-2.1/<group>/<name>/<version>/<sha1>/<file>
        let mut parts = path.split('/');
        (parts.next() == Some("files-2.1")).then_some(())?;
        let (group, name, version) = (parts.next()?, parts.next()?, parts.next()?);
        parts.next()?;
        parts.next()?;
        Some(format!("{group}:{name}:{version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn reader(&self, dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        if env(VARIABLE).is_some() {
            return Reader::LeftAlone(format!("{VARIABLE} is set already"));
        }
        Reader::InPlace(vec![(VARIABLE.to_string(), OsString::from(dir))])
    }
}

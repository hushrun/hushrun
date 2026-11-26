//! B-170 to B-177: Maven's build cache extension, served hushrun's cache.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_build::session::Session;

use crate::cmd::build_session::checkout_root;

/// B-170: the Maven build COMMAND runs in, when it loads Apache's build cache extension and names
/// no remote cache of its own: the nearest folder at or above the one COMMAND runs in, up to the
/// checkout's, that holds `.mvn`, else `pom.xml`. One that doesn't load the extension, or names a
/// remote of its own, is said in one line; a folder in no Maven build is silent.
pub(super) fn build(
    session: &mut Session,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if cfg!(not(unix)) {
        return None;
    }
    let folder = std::env::current_dir().ok()?;
    let checkout = checkout_root(&folder, env);
    let within = || {
        folder
            .ancestors()
            .take_while(|dir| dir.starts_with(&checkout) || *dir == checkout)
    };
    let build = within()
        .find(|dir| dir.join(".mvn").is_dir())
        .or_else(|| within().find(|dir| dir.join("pom.xml").is_file()))?
        .to_path_buf();
    let read = |path: &str| std::fs::read_to_string(build.join(path)).unwrap_or_default();
    let extension = hushrun_build::maven::EXTENSION_ARTIFACT;
    if !read(".mvn/extensions.xml").contains(extension) && !read("pom.xml").contains(extension) {
        session.add_line(format!(
            "build: Maven's modules aren't shared in this run: the build doesn't load Apache's build cache extension; add {extension} to .mvn/extensions.xml"
        ));
        return None;
    }
    // The build's own remote stays the build's, as a Gradle build's does.
    let property = hushrun_build::maven::URL_PROPERTY;
    let opts = env(hushrun_build::maven::OPTS_VARIABLE)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    let config = read(".mvn/maven-build-cache-config.xml");
    let own = [
        opts,
        read(".mvn/maven.config"),
        read(".mvn/jvm.config"),
        env("MAVEN_ARGS")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ]
    .iter()
    .any(|text| text.contains(property))
        || (config.contains("<remote") && config.contains("<url>"));
    if own {
        session.add_line(
            "build: maven: the build names a remote cache of its own, which stays: Maven's modules aren't shared through hushrun".to_string(),
        );
        return None;
    }
    Some(build)
}

/// B-170: starts hushrun's Maven cache for the session and gives COMMAND what points the
/// extension at it. Whether it serves; what can't be done is one line, and the run goes on
/// without it.
pub(super) fn give(
    session: &mut Session,
    build: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> bool {
    let checkout = checkout_root(build, env);
    let mut roots = vec![checkout.to_string_lossy().into_owned()];
    if let Ok(resolved) = std::fs::canonicalize(&checkout)
        && resolved != checkout
    {
        roots.push(resolved.to_string_lossy().into_owned());
    }
    let repository = env("HOME").map(|home| {
        PathBuf::from(home)
            .join(".m2")
            .to_string_lossy()
            .into_owned()
    });
    let run = hushrun_build::maven::MavenRun { roots, repository };
    match session.set_maven(run, env) {
        Ok(_) => {
            // B-175: and where the build's own settings skip tests.
            let read = |path: &str| std::fs::read_to_string(build.join(path)).unwrap_or_default();
            let var = |name: &str| {
                env(name)
                    .map(|v| v.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            for (place, text) in [
                ("MAVEN_OPTS", var("MAVEN_OPTS")),
                ("MAVEN_ARGS", var("MAVEN_ARGS")),
                (".mvn/maven.config", read(".mvn/maven.config")),
                (".mvn/jvm.config", read(".mvn/jvm.config")),
            ] {
                if let Some(property) = hushrun_build::maven::skips_tests(text.split_whitespace()) {
                    session.maven_skips_tests(property, place);
                    break;
                }
            }
            true
        }
        Err(err) => {
            session.add_line(format!(
                "build: Maven's modules aren't shared in this run: {err}"
            ));
            false
        }
    }
}

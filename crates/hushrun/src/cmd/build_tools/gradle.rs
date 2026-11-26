//! B-77 to B-83: Gradle's remote build cache, served by hushrun.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_build::session::Session;

use crate::cmd::build_session::checkout_root;

/// B-77: the Gradle build COMMAND runs in, when its settings file loads hushrun's script: the
/// nearest folder at or above the one COMMAND runs in, up to the checkout's, that holds
/// `settings.gradle` or `settings.gradle.kts`. A settings file without the line is said in one
/// line, with the line to add; a folder in no Gradle build is silent.
pub(super) fn build(
    session: &mut Session,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if cfg!(not(unix)) {
        return None;
    }
    let folder = std::env::current_dir().ok()?;
    let checkout = checkout_root(&folder, env);
    let build = folder
        .ancestors()
        .take_while(|dir| dir.starts_with(&checkout) || *dir == checkout)
        .find(|dir| {
            ["settings.gradle", "settings.gradle.kts"]
                .iter()
                .any(|name| dir.join(name).is_file())
        })?
        .to_path_buf();
    let (file, line) = ["settings.gradle.kts", "settings.gradle"]
        .iter()
        .map(|name| build.join(name))
        .find(|path| path.is_file())
        .map(|path| {
            let line = if path.extension().is_some_and(|e| e == "kts") {
                crate::units::gradle::KOTLIN_LINE
            } else {
                crate::units::gradle::GROOVY_LINE
            };
            (path, line)
        })?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.contains(hushrun_build::gradle::SCRIPT_VARIABLE) {
        session.add_line(format!(
            "build: Gradle's tasks aren't shared in this run: {} doesn't load hushrun's script; add `{line}` after any pluginManagement {{}} and plugins {{}} block",
            file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
        return None;
    }
    Some(build)
}

/// B-77: starts hushrun's Gradle cache for the session and gives COMMAND what loads it. Whether
/// it serves; what can't be done is one line, and the run goes on without it.
pub(super) fn give(
    session: &mut Session,
    build: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> bool {
    let current = session.current().clone();
    let checkout = checkout_root(build, env);
    let given = checkout.to_string_lossy().into_owned();
    let mut roots = vec![given.clone()];
    if let Ok(resolved) = std::fs::canonicalize(&checkout)
        && resolved != checkout
    {
        roots.push(resolved.to_string_lossy().into_owned());
    }
    let user_home = env("GRADLE_USER_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".gradle")));
    let run = hushrun_build::gradle::GradleRun {
        roots,
        user_home: user_home.as_ref().map(|p| p.to_string_lossy().into_owned()),
    };
    let started = hushrun_build::gradle::install_script(&current.store).and_then(|script| {
        let bound = hushrun_build::gradle::bind(&current.store)?;
        let stable = bound.stable;
        session.set_gradle(run, &script, bound, env).map(|_| stable)
    });
    match started {
        Ok(stable) => {
            if !stable {
                session.add_line(
                    "build: gradle: another run is using this store's Gradle cache port, so this run's Gradle can't reuse its configuration cache".to_string(),
                );
            }
            if !caching_on(build, user_home.as_deref()) {
                session.add_line(
                    "build: gradle: the build cache is off for this build, so Gradle asks hushrun's cache nothing: set `org.gradle.caching=true` in gradle.properties, or pass --build-cache".to_string(),
                );
            }
            true
        }
        Err(err) => {
            session.add_line(format!(
                "build: Gradle's tasks aren't shared in this run: {err}"
            ));
            false
        }
    }
}

/// Whether Gradle's build cache is on for the build at `build` by its `gradle.properties` or
/// the user's (`org.gradle.caching=true`): the user's wins, as Gradle reads them. A command line
/// with `--build-cache` turns it on too, which this can't see.
fn caching_on(build: &Path, user_home: Option<&Path>) -> bool {
    let read = |path: PathBuf| -> Option<bool> {
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .filter_map(|line| line.trim().strip_prefix("org.gradle.caching"))
            .filter_map(|rest| rest.trim_start().strip_prefix(['=', ':']))
            .map(|value| value.trim().eq_ignore_ascii_case("true"))
            .next_back()
    };
    user_home
        .and_then(|home| read(home.join("gradle.properties")))
        .or_else(|| read(build.join("gradle.properties")))
        .unwrap_or(false)
}

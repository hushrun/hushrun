//! The per-user settings (B-1, B-21): the mode, how long blocks are kept, packages never
//! stored, what happens to programs. One file, changed under a lock and written whole.
//!
//! The file is the only home of the mode. `HUSHRUN_BUILD`, `HUSHRUN_BUILD_KEEP_DAYS` and
//! `HUSHRUN_BUILD_DIR` change one command's view of it ([`Overrides`]) and never the file.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Mode, Programs};

/// The version of the file this hushrun writes and reads.
pub const VERSION: u64 = 1;

/// Nothing is stored while the store's volume has less than this many gigabytes free, unless the
/// settings say otherwise (B-22).
pub const MIN_FREE_GB: u32 = 10;

/// What one person chose for build blocks (B-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The mode of reuse (B-1).
    pub mode: Mode,
    /// Blocks not used for more than this many days are deleted (B-14). Never 0.
    pub keep_days: u32,
    /// Packages whose compiles are never stored (B-4), by name.
    pub never_store: Vec<String>,
    /// Extra variables whose values go into the first key (B-5), by name.
    pub env: Vec<String>,
    /// Gigabytes the store's volume keeps free; 0 turns the rule off (B-22).
    pub min_free_gb: u32,
    /// What the wrapper does with a program on an Apple target (B-9).
    pub programs: Programs,
    /// How much of the org's shared store this machine takes part in (B-33): `on`, `observe` or
    /// `off`. It lowers the org's shared mode for this machine and never raises it.
    pub share: Mode,
    /// Whether someone chose the mode (`hushrun build on|observe|off`), so that the file names
    /// it. `false` only with the default mode, `off`: nobody chose, and the file names no mode,
    /// which a sandbox reads as its own default (B-25). Kept by [`load`] and [`update`] alone.
    pub(crate) mode_chosen: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::Off,
            keep_days: crate::KEEP_DAYS,
            never_store: Vec::new(),
            env: Vec::new(),
            min_free_gb: MIN_FREE_GB,
            programs: Programs::default(),
            share: Mode::On,
            mode_chosen: false,
        }
    }
}

/// Why the settings can't be read or saved, or a variable can't be used. Each names its file or
/// variable.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The file exists and can't be read.
    #[error("can't read {}: {source}", .path.display())]
    Read {
        /// The settings file.
        path: PathBuf,
        /// What reading it said.
        source: io::Error,
    },
    /// The file is not settings this hushrun understands, or a change made them invalid.
    #[error("{} is not valid build settings: {reason}", .path.display())]
    Invalid {
        /// The settings file.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// The file was written by a newer hushrun. It is not half understood.
    #[error("{} was written by a newer hushrun (version {found}); this one reads version {VERSION}", .path.display())]
    Newer {
        /// The settings file.
        path: PathBuf,
        /// The version the file says.
        found: u64,
    },
    /// The file, its folder or its lock can't be written.
    #[error("can't save {}: {source}", .path.display())]
    Write {
        /// The settings file.
        path: PathBuf,
        /// What writing said.
        source: io::Error,
    },
    /// A variable whose value can't be used. It is never ignored.
    #[error("{name}={value:?} is not valid: expected {expected}")]
    Variable {
        /// The variable.
        name: &'static str,
        /// Its value, made UTF-8 where it wasn't.
        value: String,
        /// What it should be.
        expected: &'static str,
    },
}

/// The file as read: every field but the version may be left out, and an unknown field is
/// refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    // Checked before this is read; here so that it isn't an unknown field.
    #[serde(rename = "version")]
    _version: u64,
    mode: Option<Mode>,
    keep_days: Option<u32>,
    never_store: Option<Vec<String>>,
    env: Option<Vec<String>>,
    min_free_gb: Option<u32>,
    programs: Option<Programs>,
    share: Option<Mode>,
}

/// The file as written: every field, but for a mode nobody chose (B-25). A file with no `mode`
/// reads as `off` on a person's machine, here and in every released hushrun.
#[derive(Serialize)]
struct Written<'a> {
    version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<Mode>,
    keep_days: u32,
    never_store: &'a [String],
    env: &'a [String],
    min_free_gb: u32,
    programs: Programs,
    share: Mode,
}

impl Settings {
    /// These settings for one command, with the variables of that command applied.
    pub fn with(&self, overrides: &Overrides) -> Settings {
        Settings {
            mode: overrides.mode.unwrap_or(self.mode),
            keep_days: overrides.keep_days.unwrap_or(self.keep_days),
            share: overrides.share.unwrap_or(self.share),
            ..self.clone()
        }
    }

    /// Sets the mode as someone's choice (`hushrun build on|observe|off`): the file names it
    /// from then on, `off` too.
    pub fn choose(&mut self, mode: Mode) {
        self.mode = mode;
        self.mode_chosen = true;
    }

    fn check(&self, path: &Path) -> Result<(), SettingsError> {
        if self.keep_days == 0 {
            return Err(SettingsError::Invalid {
                path: path.to_path_buf(),
                reason: "keep_days must be at least 1".into(),
            });
        }
        Ok(())
    }
}

/// The settings saved at `path`. A missing file is the defaults, and so is a missing field; a file that can't be read, isn't
/// valid or comes from a newer hushrun is an error.
pub fn load(path: &Path) -> Result<Settings, SettingsError> {
    let Some(stored) = read(path)? else {
        return Ok(Settings::default());
    };
    let defaults = Settings::default();
    let settings = Settings {
        mode_chosen: stored.mode.is_some(),
        mode: stored.mode.unwrap_or(defaults.mode),
        keep_days: stored.keep_days.unwrap_or(defaults.keep_days),
        never_store: stored.never_store.unwrap_or(defaults.never_store),
        env: stored.env.unwrap_or(defaults.env),
        min_free_gb: stored.min_free_gb.unwrap_or(defaults.min_free_gb),
        programs: stored.programs.unwrap_or(defaults.programs),
        share: stored.share.unwrap_or(defaults.share),
    };
    settings.check(path)?;
    Ok(settings)
}

/// B-25: the mode of reuse on a machine where no person is signed in (a sandbox), before the
/// command's variables: `on`, unless the file at `path` says `off`, which wins there as it does
/// everywhere. No file, or one that names no mode, leaves it `on`; nothing else in the file is
/// read for a sandbox. A file that can't be read or isn't valid is an error, as in [`load`]: it
/// can't say whether `off` is written. The one definition, for the run and `hushrun build status`.
pub fn sandbox_mode(path: &Path) -> Result<Mode, SettingsError> {
    Ok(match read(path)?.and_then(|stored| stored.mode) {
        Some(Mode::Off) => Mode::Off,
        Some(Mode::Observe | Mode::On) | None => Mode::On,
    })
}

/// The file at `path` as written; `None` when there is none.
fn read(path: &Path) -> Result<Option<Stored>, SettingsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let invalid = |reason: String| SettingsError::Invalid {
        path: path.to_path_buf(),
        reason,
    };
    // The version is looked at first, so a newer file says so instead of naming a field this
    // hushrun doesn't know.
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| invalid(err.to_string()))?;
    match value.get("version").and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(found) if found > VERSION => {
            return Err(SettingsError::Newer {
                path: path.to_path_buf(),
                found,
            });
        }
        _ => return Err(invalid(format!("\"version\" must be {VERSION}"))),
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|err| invalid(err.to_string()))
}

/// Changes the settings at `path` with `change` and saves them, while no other hushrun does:
/// under `<file>.lock`, it loads, applies, and writes the whole file to a temporary file in the
/// same folder, then renames it. Creates the folder if needed. What was saved.
///
/// A file that can't be read is not overwritten: the error comes back and nothing is saved.
pub fn update(path: &Path, change: impl FnOnce(&mut Settings)) -> Result<Settings, SettingsError> {
    let write_error = |source| SettingsError::Write {
        path: path.to_path_buf(),
        source,
    };
    let dir = folder(path);
    fs::create_dir_all(dir).map_err(write_error)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))
        .map_err(write_error)?;
    // A file system that can't lock is no reason not to save, as with the credentials: the
    // change goes in unlocked.
    let _ = lock.lock();
    let mut settings = load(path)?;
    let mode = settings.mode;
    change(&mut settings);
    // A mode that changed was chosen. One that didn't is written only if it was in the file,
    // or [`Settings::choose`] says someone chose it: a save of another setting writes no mode.
    settings.mode_chosen |= settings.mode != mode;
    settings.check(path)?;
    save(path, &settings).map_err(write_error)?;
    drop(lock);
    Ok(settings)
}

/// The folder that holds `path`.
fn folder(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Writes the file whole: a temporary file of this writer's own in the same folder, synced, then
/// renamed over the file. A failure leaves the old file as it was.
fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let written = Written {
        version: VERSION,
        mode: settings.mode_chosen.then_some(settings.mode),
        keep_days: settings.keep_days,
        never_store: &settings.never_store,
        env: &settings.env,
        min_free_gb: settings.min_free_gb,
        programs: settings.programs,
        share: settings.share,
    };
    let mut bytes = serde_json::to_vec_pretty(&written)?;
    bytes.push(b'\n');
    let dir = folder(path);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (temp, mut file) = create_temp(dir, &name)?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A new temporary file beside `name`, under a name no other writer holds.
fn create_temp(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    for n in 0..64 {
        let temp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary name",
    ))
}

/// What one command's variables change (B-1, B-13, B-14). An empty variable is unset, and so is
/// `HUSHRUN_BUILD=true` or `false`, which another command prints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `HUSHRUN_BUILD`: the mode for this command.
    pub mode: Option<Mode>,
    /// `HUSHRUN_BUILD_KEEP_DAYS`: how many days blocks are kept, at least 1.
    pub keep_days: Option<u32>,
    /// `HUSHRUN_BUILD_DIR`: the store's folder instead of `<cache directory>/build`. The settings
    /// file doesn't hold it, so whoever opens the store reads it from here.
    pub dir: Option<PathBuf>,
    /// `HUSHRUN_BUILD_SHARE`: the share setting for this command (B-33).
    pub share: Option<Mode>,
}

impl Overrides {
    /// Reads the variables through `env`. A value that doesn't parse is an error naming the
    /// variable.
    pub fn from(env: &dyn Fn(&str) -> Option<OsString>) -> Result<Overrides, SettingsError> {
        let mode = match text(env, "HUSHRUN_BUILD", "off, observe or on")? {
            None => None,
            // `hushrun plan --format env` and `hushrun artifact resolve` print `HUSHRUN_BUILD=true`
            // (or `false`) for a shell to evaluate: that variable is theirs, not a mode, and is
            // read as unset here, silently.
            Some(value) if value == "true" || value == "false" => None,
            Some(value) => Some(Mode::parse(&value).ok_or(SettingsError::Variable {
                name: "HUSHRUN_BUILD",
                value,
                expected: "off, observe or on",
            })?),
        };
        let keep_days = match text(
            env,
            "HUSHRUN_BUILD_KEEP_DAYS",
            "a number of days, at least 1",
        )? {
            None => None,
            Some(value) => match value.parse::<u32>() {
                Ok(days) if days > 0 && value.bytes().all(|b| b.is_ascii_digit()) => Some(days),
                _ => {
                    return Err(SettingsError::Variable {
                        name: "HUSHRUN_BUILD_KEEP_DAYS",
                        value,
                        expected: "a number of days, at least 1",
                    });
                }
            },
        };
        // The wrapper runs in many folders, so a relative store would be a different store in
        // each.
        let dir = match env("HUSHRUN_BUILD_DIR") {
            Some(value) if value.is_empty() => None,
            Some(value) => {
                let path = PathBuf::from(&value);
                if !path.is_absolute() {
                    return Err(SettingsError::Variable {
                        name: "HUSHRUN_BUILD_DIR",
                        value: value.to_string_lossy().into_owned(),
                        expected: "an absolute folder",
                    });
                }
                Some(path)
            }
            None => None,
        };
        let share = match text(env, "HUSHRUN_BUILD_SHARE", "off, observe or on")? {
            None => None,
            Some(value) => Some(Mode::parse(&value).ok_or(SettingsError::Variable {
                name: "HUSHRUN_BUILD_SHARE",
                value,
                expected: "off, observe or on",
            })?),
        };
        Ok(Overrides {
            mode,
            keep_days,
            dir,
            share,
        })
    }
}

/// A variable's value as text; unset or empty is nothing.
fn text(
    env: &dyn Fn(&str) -> Option<OsString>,
    name: &'static str,
    expected: &'static str,
) -> Result<Option<String>, SettingsError> {
    match env(name) {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => value
            .into_string()
            .map(Some)
            .map_err(|value| SettingsError::Variable {
                name,
                value: value.to_string_lossy().into_owned(),
                expected,
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::{Overrides, Settings, SettingsError, load, update};
    use crate::model::{Mode, Programs};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), OsString::from(value)))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/hushrun/build.json");
        (dir, path)
    }

    // covers: B-21, B-25
    #[test]
    fn a_save_of_another_setting_writes_no_mode_nobody_chose() {
        use super::sandbox_mode;
        let (_dir, path) = file();
        let written = |path: &Path| -> serde_json::Value {
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
        };
        update(&path, |s| s.share = Mode::Observe).unwrap();
        assert!(written(&path).get("mode").is_none(), "{}", written(&path));
        // Absent stays absent, save after save, and reads as before: `off` for a person, the
        // sandbox's own default in a sandbox.
        update(&path, |s| s.keep_days = 7).unwrap();
        assert!(written(&path).get("mode").is_none());
        assert_eq!(load(&path).unwrap().mode, Mode::Off);
        assert_eq!(sandbox_mode(&path).unwrap(), Mode::On);
        // A chosen `off` is written, though the mode's value didn't change, and later saves of
        // other settings keep it.
        update(&path, |s| s.choose(Mode::Off)).unwrap();
        assert_eq!(written(&path)["mode"], "off");
        update(&path, |s| s.share = Mode::On).unwrap();
        assert_eq!(written(&path)["mode"], "off");
        assert_eq!(sandbox_mode(&path).unwrap(), Mode::Off);
        // A mode in a file someone wrote by hand is kept too.
        fs::write(&path, br#"{"version": 1, "mode": "observe"}"#).unwrap();
        update(&path, |s| s.keep_days = 3).unwrap();
        assert_eq!(written(&path)["mode"], "observe");
    }

    // covers: B-25
    #[test]
    fn in_a_sandbox_only_a_written_off_turns_reuse_off() {
        use super::sandbox_mode;
        let (_dir, path) = file();
        // No file: nobody wrote anything.
        assert_eq!(sandbox_mode(&path).unwrap(), Mode::On);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A file that names no mode.
        fs::write(&path, br#"{"version": 1, "keep_days": 7}"#).unwrap();
        assert_eq!(sandbox_mode(&path).unwrap(), Mode::On);
        for (written, mode) in [("off", Mode::Off), ("observe", Mode::On), ("on", Mode::On)] {
            fs::write(&path, format!(r#"{{"version": 1, "mode": "{written}"}}"#)).unwrap();
            assert_eq!(sandbox_mode(&path).unwrap(), mode, "{written}");
        }
        // As `hushrun build off` writes it.
        update(&path, |s| s.choose(Mode::Off)).unwrap();
        assert_eq!(sandbox_mode(&path).unwrap(), Mode::Off);
        // A file that can't say whether `off` is written is an error, not a default.
        fs::write(&path, b"{").unwrap();
        assert!(matches!(
            sandbox_mode(&path),
            Err(SettingsError::Invalid { .. })
        ));
    }

    // covers: B-1
    // covers: B-21
    // covers: B-22
    #[test]
    fn a_missing_file_is_the_defaults() {
        let (_dir, path) = file();
        let settings = load(&path).unwrap();
        assert_eq!(settings.mode, Mode::Off);
        assert_eq!(settings.keep_days, 15);
        assert!(settings.never_store.is_empty() && settings.env.is_empty());
        assert_eq!(settings.min_free_gb, 10);
        assert_eq!(settings.programs, Programs::Movable);
        assert_eq!(settings, Settings::default());
    }

    // covers: B-21
    #[test]
    fn what_is_saved_is_what_is_loaded() {
        let (_dir, path) = file();
        let saved = update(&path, |settings| {
            settings.mode = Mode::Observe;
            settings.keep_days = 30;
            settings.never_store = vec!["openssl-sys".into()];
            settings.env = vec!["SQLX_OFFLINE".into()];
            settings.min_free_gb = 0;
            settings.programs = Programs::Tied;
        })
        .unwrap();
        assert_eq!(load(&path).unwrap(), saved);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"mode\": \"observe\""), "{text}");
        assert!(text.contains("\"programs\": \"tied\""), "{text}");
        // Only the file and its lock: no temporary file left.
        let mut names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["build.json", "build.json.lock"]);
    }

    // covers: B-21
    #[test]
    fn a_file_with_only_a_version_is_the_defaults() {
        let (_dir, path) = file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"version": 1, "mode": "on"}"#).unwrap();
        let settings = load(&path).unwrap();
        assert_eq!(settings.mode, Mode::On);
        assert_eq!(settings.keep_days, 15);
        assert_eq!(settings.programs, Programs::Movable);
        fs::write(&path, r#"{"version": 1, "programs": "tied"}"#).unwrap();
        assert_eq!(load(&path).unwrap().programs, Programs::Tied);
    }

    // covers: B-21
    #[test]
    fn an_unknown_field_is_refused() {
        let (_dir, path) = file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"version": 1, "mode": "on", "upload": true}"#).unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, SettingsError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("build.json"), "{err}");
        assert!(err.to_string().contains("upload"), "{err}");
    }

    // covers: B-21
    #[test]
    fn a_newer_version_is_refused_and_not_overwritten() {
        let (_dir, path) = file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{"version": 2, "mode": "on", "share": "team"}"#;
        fs::write(&path, newer).unwrap();
        let err = load(&path).unwrap_err();
        assert!(
            matches!(err, SettingsError::Newer { found: 2, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("build.json"), "{err}");
        let err = update(&path, |settings| settings.mode = Mode::Off).unwrap_err();
        assert!(matches!(err, SettingsError::Newer { .. }), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    }

    // covers: B-21
    #[test]
    fn invalid_files_are_errors_that_name_the_file() {
        let (_dir, path) = file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in [
            "",
            "{",
            r#"{"mode": "on"}"#,
            r#"{"version": 0}"#,
            r#"{"version": "1"}"#,
            r#"{"version": 1, "mode": "maybe"}"#,
            r#"{"version": 1, "keep_days": 0}"#,
            r#"{"version": 1, "keep_days": -3}"#,
            r#"{"version": 1, "programs": "loose"}"#,
            r#"{"version": 1, "programs": "Tied"}"#,
            r#"{"version": 1, "programs": 1}"#,
        ] {
            fs::write(&path, text).unwrap();
            let err = load(&path).unwrap_err();
            assert!(
                matches!(err, SettingsError::Invalid { .. }),
                "{text}: {err}"
            );
            assert!(err.to_string().contains("build.json"), "{err}");
        }
    }

    // covers: B-21
    #[test]
    fn a_change_to_zero_keep_days_is_refused_and_nothing_saved() {
        let (_dir, path) = file();
        update(&path, |settings| settings.mode = Mode::On).unwrap();
        let before = fs::read(&path).unwrap();
        let err = update(&path, |settings| settings.keep_days = 0).unwrap_err();
        assert!(matches!(err, SettingsError::Invalid { .. }), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    // covers: B-21
    #[test]
    fn changes_at_the_same_moment_all_land() {
        let (_dir, path) = file();
        let threads: Vec<_> = (0..16)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    update(&path, |settings| {
                        // As long as a change takes between reading and writing.
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        settings.never_store.push(format!("crate-{n}"));
                    })
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(load(&path).unwrap().never_store.len(), 16);
    }

    // covers: B-21
    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_old_file_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, path) = file();
        update(&path, |settings| settings.mode = Mode::Observe).unwrap();
        let before = fs::read(&path).unwrap();
        let folder = path.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
        // Someone who can write anyway (root) can't run this test.
        let probe = folder.join("probe");
        if fs::write(&probe, b"").is_ok() {
            let _ = fs::remove_file(&probe);
            fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let err = update(&path, |settings| settings.mode = Mode::On).unwrap_err();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(err, SettingsError::Write { .. }), "{err}");
        assert!(err.to_string().contains("build.json"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(load(&path).unwrap().mode, Mode::Observe);
    }

    // covers: B-1
    #[test]
    fn hushrun_build_sets_the_mode_for_one_command() {
        let (_dir, path) = file();
        update(&path, |settings| settings.mode = Mode::On).unwrap();
        let overrides = Overrides::from(&env(&[("HUSHRUN_BUILD", "off")])).unwrap();
        let settings = load(&path).unwrap();
        assert_eq!(settings.with(&overrides).mode, Mode::Off);
        // The file keeps its own.
        assert_eq!(load(&path).unwrap().mode, Mode::On);
        let none = Overrides::from(&env(&[("HUSHRUN_BUILD", "")])).unwrap();
        assert_eq!(settings.with(&none), settings);
    }

    // covers: B-1
    #[test]
    fn hushrun_build_true_or_false_from_a_plan_is_not_a_mode_and_counts_as_unset() {
        // `hushrun plan --format env` and `hushrun artifact resolve` print `HUSHRUN_BUILD=true`
        // for a shell to evaluate; the next `hushrun run` must not read it as a bad mode.
        for value in ["true", "false"] {
            let overrides = Overrides::from(&env(&[("HUSHRUN_BUILD", value)])).unwrap();
            assert_eq!(overrides, Overrides::default(), "{value}");
        }
        // Anything else that isn't a mode is still an error.
        assert!(Overrides::from(&env(&[("HUSHRUN_BUILD", "True")])).is_err());
    }

    // covers: B-14
    #[test]
    fn hushrun_build_keep_days_sets_the_days_for_one_command() {
        let overrides = Overrides::from(&env(&[("HUSHRUN_BUILD_KEEP_DAYS", "3")])).unwrap();
        let settings = Settings::default().with(&overrides);
        assert_eq!(settings.keep_days, 3);
        assert_eq!(settings.mode, Mode::Off);
    }

    // covers: B-33
    #[test]
    fn the_share_setting_is_saved_and_hushrun_build_share_sets_it_for_one_command() {
        let (_dir, path) = file();
        assert_eq!(load(&path).unwrap().share, Mode::On);
        update(&path, |settings| settings.share = Mode::Observe).unwrap();
        let settings = load(&path).unwrap();
        assert_eq!(settings.share, Mode::Observe);
        let overrides = Overrides::from(&env(&[("HUSHRUN_BUILD_SHARE", "off")])).unwrap();
        assert_eq!(settings.with(&overrides).share, Mode::Off);
        assert_eq!(load(&path).unwrap().share, Mode::Observe);
        let wrong = Overrides::from(&env(&[("HUSHRUN_BUILD_SHARE", "yes")])).unwrap_err();
        assert!(wrong.to_string().contains("HUSHRUN_BUILD_SHARE"), "{wrong}");
    }

    // covers: B-13
    #[test]
    fn hushrun_build_dir_names_the_store() {
        let overrides = Overrides::from(&env(&[("HUSHRUN_BUILD_DIR", "/srv/blocks")])).unwrap();
        assert_eq!(overrides.dir.as_deref(), Some(Path::new("/srv/blocks")));
        let empty = Overrides::from(&env(&[("HUSHRUN_BUILD_DIR", "")])).unwrap();
        assert_eq!(empty, Overrides::default());
        assert_eq!(Overrides::from(&env(&[])).unwrap(), Overrides::default());
    }

    // covers: B-1
    // covers: B-14
    #[test]
    fn a_bad_variable_is_an_error_that_names_it() {
        for (name, value) in [
            ("HUSHRUN_BUILD", "yes"),
            ("HUSHRUN_BUILD", "ON"),
            ("HUSHRUN_BUILD_KEEP_DAYS", "0"),
            ("HUSHRUN_BUILD_KEEP_DAYS", "-1"),
            ("HUSHRUN_BUILD_KEEP_DAYS", "+5"),
            ("HUSHRUN_BUILD_KEEP_DAYS", "two"),
            ("HUSHRUN_BUILD_KEEP_DAYS", "99999999999"),
            ("HUSHRUN_BUILD_DIR", "relative/store"),
        ] {
            let err = Overrides::from(&env(&[(name, value)])).unwrap_err();
            assert!(
                matches!(err, SettingsError::Variable { name: found, .. } if found == name),
                "{name}={value}: {err}"
            );
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    // covers: B-1
    #[cfg(unix)]
    #[test]
    fn a_variable_that_is_not_utf8_is_an_error() {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = |name: &str| {
            (name == "HUSHRUN_BUILD").then(|| OsString::from_vec(vec![b'o', 0xff, b'n']))
        };
        let err = Overrides::from(&bad).unwrap_err();
        assert!(
            matches!(
                err,
                SettingsError::Variable {
                    name: "HUSHRUN_BUILD",
                    ..
                }
            ),
            "{err}"
        );
    }
}

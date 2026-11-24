//! B-85: `hushrun build xcode on|off|status`, Xcode's compilation cache for the Xcode app on this
//! Mac. The app builds outside any `hushrun run`, so its service is a LaunchAgent of the person's
//! (`hushrun build xcode serve`, kept running by launchd), on a socket at a fixed place; and the
//! app learns of it through `XCODE_XCCONFIG_FILE`, which the agent sets in the login session's
//! environment (`launchctl setenv`) every time it starts, so every app opened after reads it.
//!
//! The service reads and writes the same cache as a run's (the store's `xcode/` folder): what a
//! `hushrun run … xcodebuild` compiled or downloaded on this Mac, the app reuses, and the other
//! way round. It asks no backend: a run is what downloads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::anyhow;

use crate::exit::{CmdResult, Exit, Failure};

/// The LaunchAgent's label, and its file's name.
pub(crate) const LABEL: &str = "com.hushrun.xcode-cache";

/// Where the app's service lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Places {
    /// The folder of its files: the xcconfig, the log, the socket when the path is short enough.
    pub(crate) folder: PathBuf,
    pub(crate) socket: PathBuf,
    pub(crate) xcconfig: PathBuf,
    pub(crate) log: PathBuf,
    /// The LaunchAgent's file.
    pub(crate) plist: PathBuf,
}

impl Places {
    /// From hushrun's cache directory and the home folder.
    pub(crate) fn of(cache_dir: &Path, home: &Path, uid: u32) -> Places {
        let folder = cache_dir.join("xcode-app");
        let socket = folder.join(super::SOCKET);
        let socket = if socket.as_os_str().len() <= hushrun_build::relay::MOST_SOCKET_PATH {
            socket
        } else {
            PathBuf::from(format!("/tmp/hushrun-xcode-{uid}")).join(super::SOCKET)
        };
        Places {
            xcconfig: folder.join("hushrun.xcconfig"),
            log: folder.join("serve.log"),
            plist: home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            socket,
            folder,
        }
    }

    fn here() -> Result<Places, Failure> {
        let cache = crate::context::cache_dir()
            .ok_or_else(|| unexpected("there is no cache directory for hushrun's files"))?;
        let home = dirs::home_dir().ok_or_else(|| unexpected("there is no home folder"))?;
        Ok(Places::of(&cache, &home, uid()))
    }
}

/// The LaunchAgent: `hushrun build xcode serve`, at login and kept running, its output in `log`.
pub(crate) fn plist(exe: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>build</string>
    <string>xcode</string>
    <string>serve</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml(&exe.to_string_lossy()),
        xml(&log.to_string_lossy()),
        xml(&log.to_string_lossy()),
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The app's xcconfig: the service's socket, and maps from the workspace's folder, whichever
/// checkout the app has open.
pub(crate) fn xcconfig_text(places: &Places) -> String {
    use hushrun_build::xcode::xcconfig;
    let maps = xcconfig::maps(&xcconfig::Root::Workspace).ok();
    xcconfig::text(&places.socket, maps.as_deref(), None)
}

fn unexpected(why: &str) -> Failure {
    Failure::new(Exit::Unexpected, anyhow!("{why}"))
}

fn usage(why: String) -> Failure {
    Failure::new(Exit::Usage, anyhow!("{why}"))
}

#[cfg(unix)]
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

fn launchctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|err| format!("launchctl: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "launchctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn mac_only() -> Result<(), Failure> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(usage(
            "the Xcode app runs on macOS only: on this machine, `hushrun run` serves xcodebuild's cache".into(),
        ))
    }
}

/// `hushrun build xcode on`.
pub(crate) fn on() -> CmdResult {
    mac_only()?;
    let places = Places::here()?;
    let exe = std::env::current_exe()
        .map_err(|err| unexpected(&format!("hushrun can't find its own binary: {err}")))?;
    std::fs::create_dir_all(&places.folder)
        .map_err(|err| unexpected(&format!("{}: {err}", places.folder.display())))?;
    std::fs::write(&places.xcconfig, xcconfig_text(&places))
        .map_err(|err| unexpected(&format!("{}: {err}", places.xcconfig.display())))?;
    if let Some(parent) = places.plist.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| unexpected(&format!("{}: {err}", parent.display())))?;
    }
    std::fs::write(&places.plist, plist(&exe, &places.log))
        .map_err(|err| unexpected(&format!("{}: {err}", places.plist.display())))?;
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]);
    // launchd may not have finished removing the old agent: `bootstrap` then fails for a moment.
    let plist_path = places.plist.to_string_lossy().into_owned();
    let mut loaded = launchctl(&["bootstrap", &domain, &plist_path]);
    for _ in 0..5 {
        if loaded.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        loaded = launchctl(&["bootstrap", &domain, &plist_path]);
    }
    loaded.map_err(|why| unexpected(&why))?;
    launchctl(&[
        "setenv",
        hushrun_build::xcode::XCCONFIG_VAR,
        &places.xcconfig.to_string_lossy(),
    ])
    .map_err(|why| unexpected(&why))?;
    println!(
        "hushrun: the Xcode app's builds on this Mac now use hushrun's compilation cache, the one `hushrun run` fills."
    );
    println!(
        "hushrun: quit Xcode and open it again once, so that it reads {}.",
        hushrun_build::xcode::XCCONFIG_VAR
    );
    println!(
        "hushrun: it needs Xcode {} (the settings are in {}); `hushrun build xcode off` undoes it.",
        hushrun_build::xcode::probe::KNOWN_MAJORS
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        places.xcconfig.display()
    );
    Ok(Exit::Ok)
}

/// `hushrun build xcode off`.
pub(crate) fn off() -> CmdResult {
    mac_only()?;
    let places = Places::here()?;
    let _ = launchctl(&["bootout", &format!("gui/{}/{LABEL}", uid())]);
    // Only ours: a file the person set stays theirs.
    let set = launchctl(&["getenv", hushrun_build::xcode::XCCONFIG_VAR]).unwrap_or_default();
    if Path::new(&set) == places.xcconfig {
        let _ = launchctl(&["unsetenv", hushrun_build::xcode::XCCONFIG_VAR]);
    }
    let _ = std::fs::remove_file(&places.plist);
    let _ = std::fs::remove_file(&places.xcconfig);
    let _ = std::fs::remove_file(&places.socket);
    println!(
        "hushrun: the Xcode app no longer uses hushrun's compilation cache: quit Xcode and open it again once. What is cached stays until it expires, or `hushrun build clean`."
    );
    Ok(Exit::Ok)
}

/// `hushrun build xcode status`.
pub(crate) fn status(store: Option<&Path>) -> CmdResult {
    let places = Places::here()?;
    let agent = places.plist.is_file();
    let answering = answers(&places.socket);
    let set = if cfg!(target_os = "macos") {
        launchctl(&["getenv", hushrun_build::xcode::XCCONFIG_VAR]).unwrap_or_default()
    } else {
        String::new()
    };
    println!(
        "xcode app: {}",
        match (agent, answering, Path::new(&set) == places.xcconfig) {
            (true, true, true) => "on: the Xcode app's builds use hushrun's cache",
            (true, true, false) => {
                "on, but XCODE_XCCONFIG_FILE isn't set in this login session: run `hushrun build xcode on` again"
            }
            (true, false, _) => {
                "on, but its service doesn't answer: see the log below, or run `hushrun build xcode on` again"
            }
            (false, _, _) => "off (`hushrun build xcode on` turns it on)",
        }
    );
    println!("  socket: {}", places.socket.display());
    println!("  settings: {}", places.xcconfig.display());
    println!("  log: {}", places.log.display());
    if let Some(store) = store {
        let cache = hushrun_build::xcode::cas::Cache::in_store(store);
        println!(
            "  cache: {} ({})",
            cache.root().display(),
            human_bytes(cache.size())
        );
    }
    Ok(Exit::Ok)
}

/// `hushrun build xcode serve`: what the LaunchAgent runs. Serves until it is stopped.
pub(crate) fn serve(store: Option<PathBuf>, keep_days: u32) -> CmdResult {
    use hushrun_build::xcode::service::{Config, Service};
    let places = Places::here()?;
    let store = store.ok_or_else(|| unexpected("there is no folder for hushrun's store"))?;
    if answers(&places.socket) {
        println!(
            "hushrun: the Xcode service is running already ({})",
            places.socket.display()
        );
        return Ok(Exit::Ok);
    }
    // A socket nobody answers on is a dead service's.
    let _ = std::fs::remove_file(&places.socket);
    std::fs::create_dir_all(&places.folder)
        .map_err(|err| unexpected(&format!("{}: {err}", places.folder.display())))?;
    let _ = std::fs::write(&places.xcconfig, xcconfig_text(&places));
    let service = Service::new(Config {
        store: store.clone(),
        current: None,
        platform: None,
        folders: Vec::new(),
        staging: places.folder.join("staging"),
    });
    let running = super::start_at(service, &places.socket).map_err(|why| unexpected(&why))?;
    if cfg!(target_os = "macos") {
        // Again at every start: a login session's environment is gone at the next login.
        if let Err(why) = launchctl(&[
            "setenv",
            hushrun_build::xcode::XCCONFIG_VAR,
            &places.xcconfig.to_string_lossy(),
        ]) {
            eprintln!("hushrun: {why}");
        }
    }
    eprintln!(
        "hushrun: the Xcode service is serving {} on {}",
        store.display(),
        places.socket.display()
    );
    loop {
        running.trim(SystemTime::now(), keep_days);
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Whether a service answers on `socket`.
fn answers(socket: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        false
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-85
    #[test]
    fn the_apps_service_has_fixed_places_and_a_short_socket() {
        let places = Places::of(
            Path::new("/Users/ana/Library/Caches/hushrun"),
            Path::new("/Users/ana"),
            501,
        );
        assert_eq!(
            places.socket,
            Path::new("/Users/ana/Library/Caches/hushrun/xcode-app/xcode.sock")
        );
        assert_eq!(
            places.plist,
            Path::new("/Users/ana/Library/LaunchAgents/com.hushrun.xcode-cache.plist")
        );
        let deep = format!("/Users/ana/{}", "a".repeat(120));
        let long = Places::of(Path::new(&deep), Path::new("/Users/ana"), 501);
        assert_eq!(long.socket, Path::new("/tmp/hushrun-xcode-501/xcode.sock"));
    }

    // covers: B-85
    #[test]
    fn the_agent_runs_serve_and_the_settings_map_the_workspace() {
        let text = plist(
            Path::new("/opt/hushrun & co/hushrun"),
            Path::new("/tmp/serve.log"),
        );
        assert!(text.contains("<string>/opt/hushrun &amp; co/hushrun</string>"));
        assert!(text.contains("<string>serve</string>"));
        assert!(text.contains("<key>KeepAlive</key>"));
        let places = Places::of(Path::new("/c"), Path::new("/h"), 1);
        let settings = xcconfig_text(&places);
        assert!(
            settings.contains("COMPILATION_CACHE_REMOTE_SERVICE_PATH = /c/xcode-app/xcode.sock")
        );
        assert!(settings.contains("$(WORKSPACE_DIR)=/^workspace"));
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2_500_000), "2.5 MB");
    }
}

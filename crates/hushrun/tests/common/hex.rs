//! \[ADR 0157\] Hex for the tests of the `hex` part of a package store (`packages_hex.rs`) and the
//! ExUnit adapter's keys with a seeded install (`units_exunit.rs`): a Hex repository of this
//! test's own, built with Hex's own `mix hex.build` and `mix hex.registry build`, and served on
//! 127.0.0.1 by a server that writes down every path asked of it and can refuse every tarball.
//! No test here reaches hex.pm.
//!
//! Needs Elixir (`HUSHRUN_TEST_MIX`, `common/exunit.rs`) and Hex: `HUSHRUN_TEST_HEX` names a
//! folder that holds a Hex archive, given to Mix as `MIX_ARCHIVES` (by default `~/.mix/archives`).
//! The tests say so and pass where either is missing, unless `HUSHRUN_TEST_TOOLS=required` and
//! `HUSHRUN_TEST_HEX` are both set, as CI's Linux job sets them.

#![allow(dead_code)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The tests' Hex: the `mix` it runs in, and the folder of its archive.
pub struct Hex {
    pub mix: PathBuf,
    pub archives: PathBuf,
}

/// The tests' Hex, when there is one; missing, the tests say so and pass, unless CI asked for it
/// by name.
pub fn hex() -> Option<Hex> {
    let required = std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required")
        && std::env::var_os("HUSHRUN_TEST_HEX").is_some();
    let found = find();
    if found.is_none() {
        if required {
            panic!("Hex is missing, and HUSHRUN_TEST_TOOLS=required names HUSHRUN_TEST_HEX");
        }
        eprintln!(
            "note: skipping: Elixir or Hex is missing (set HUSHRUN_TEST_MIX and HUSHRUN_TEST_HEX)"
        );
    }
    found
}

fn find() -> Option<Hex> {
    let mix = std::env::var_os("HUSHRUN_TEST_MIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mix"));
    let archives = std::env::var_os("HUSHRUN_TEST_HEX")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".mix/archives")))?;
    let home = tempfile::tempdir().ok()?;
    let ok = Command::new(&mix)
        .args(["hex.info"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("MIX_HOME", home.path().join("mix"))
        .env("MIX_ARCHIVES", &archives)
        .env("LANG", "C.UTF-8")
        .env("ELIXIR_ERL_OPTIONS", "+fnu")
        .env("HEX_OFFLINE", "1")
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then_some(Hex { mix, archives })
}

impl Hex {
    /// The variables that point Mix and Hex at `server` and at nothing of the machine's: the
    /// tests' Hex archive, the server as Hex's repository, whose registry this test signed with a
    /// key of its own, and no proxy. `HEX_HOME` is each step's own.
    pub fn vars(&self, server: &Server) -> Vec<(&'static str, String)> {
        vec![
            ("MIX_ARCHIVES", self.archives.display().to_string()),
            ("HEX_MIRROR_URL", server.url()),
            ("HEX_UNSAFE_REGISTRY", "1".into()),
            ("HEX_NO_VERIFY_REPO_ORIGIN", "1".into()),
            ("HEX_HTTP_TIMEOUT", "20".into()),
            ("NO_PROXY", "127.0.0.1,localhost".into()),
            ("no_proxy", "127.0.0.1,localhost".into()),
            ("LANG", "C.UTF-8".into()),
            ("ELIXIR_ERL_OPTIONS", "+fnu".into()),
        ]
    }

    /// The variables of a person's setup that would change what Hex does here: cleared.
    pub const CLEARED: &'static [&'static str] = &[
        "HEX_HOME",
        "HEX_OFFLINE",
        "HEX_MIRROR",
        "HEX_MIRROR_URL",
        "HEX_TRUSTED_MIRROR_URL",
        "HEX_CACERTS_PATH",
        "MIX_XDG",
        "MIX_DEPS_PATH",
        "MIX_BUILD_PATH",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
    ];

    /// `mix ARGS` in `dir`, with `home` as `HOME` and `HEX_HOME` under it, against `server`.
    pub fn mix(&self, dir: &Path, home: &Path, server: &Server, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.mix);
        cmd.args(args).current_dir(dir);
        for name in Self::CLEARED {
            cmd.env_remove(name);
        }
        cmd.envs(self.vars(server))
            .env("HOME", home)
            .env("MIX_HOME", home.join("mix"))
            .env("HEX_HOME", home.join("hex"));
        cmd
    }
}

/// A package of the tests' repository: `mix.exs` holds `top` before its project and `extra`
/// after it; `lib/` one module.
pub struct Package {
    pub name: &'static str,
    pub version: &'static str,
    pub top: String,
    pub extra: String,
}

impl Package {
    pub fn plain(name: &'static str, version: &'static str) -> Package {
        Package {
            name,
            version,
            top: String::new(),
            extra: String::new(),
        }
    }

    /// A package whose `mix.exs`, when Mix reads it, writes `read`, and whose compile writes
    /// `compiled`: what a bake's fetch must never do.
    pub fn canary(name: &'static str, read: &Path, compiled: &Path) -> Package {
        Package {
            name,
            version: "0.1.0",
            top: format!("File.write!({:?}, \"read\")\n", read.display().to_string()),
            extra: format!(
                "defmodule Mix.Tasks.Compile.{} do\n  use Mix.Task.Compiler\n  def run(_), do: (File.write!({:?}, \"compiled\"); :ok)\nend\n",
                capitalized(name),
                compiled.display().to_string()
            ),
        }
    }

    fn module(&self) -> String {
        capitalized(self.name)
    }
}

fn capitalized(name: &str) -> String {
    name.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

/// Builds the tests' Hex repository under `root`: each of `packages` built into a tarball by
/// `mix hex.build`, and the registry of them all by `mix hex.registry build`, signed with a key
/// made here. Returns the folder a server serves.
pub fn repository(hex: &Hex, root: &Path, packages: &[Package]) -> PathBuf {
    let public = root.join("public");
    std::fs::create_dir_all(public.join("tarballs")).unwrap();
    let home = root.join("home");
    let run = |cmd: &mut Command, what: &str| {
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{what}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let offline = Server::start(&public);
    for p in packages {
        let dir = root.join(format!("src/{}-{}", p.name, p.version));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        let compilers = if p.extra.contains("Mix.Tasks.Compile.") {
            format!(", compilers: [:{}] ++ Mix.compilers()", p.name)
        } else {
            String::new()
        };
        std::fs::write(
            dir.join("mix.exs"),
            format!(
                "{}defmodule {m}.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :{n}, version: \"{v}\", elixir: \"~> 1.14\", deps: [], description: \"{n}\", package: [licenses: [\"MIT\"], links: %{{}}]{compilers}]\n  end\nend\n{}",
                p.top,
                p.extra,
                m = p.module(),
                n = p.name,
                v = p.version,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("lib/{}.ex", p.name)),
            format!(
                "defmodule {} do\n  def version, do: \"{}\"\nend\n",
                p.module(),
                p.version
            ),
        )
        .unwrap();
        run(
            &mut hex.mix(&dir, &home, &offline, &["hex.build"]),
            "mix hex.build",
        );
        let tarball = format!("{}-{}.tar", p.name, p.version);
        std::fs::rename(dir.join(&tarball), public.join("tarballs").join(&tarball)).unwrap();
    }
    let elixir = hex.mix.with_file_name("elixir");
    let elixir = if elixir.is_file() {
        elixir
    } else {
        PathBuf::from("elixir")
    };
    run(
        Command::new(elixir)
            .current_dir(root)
            .env("ELIXIR_ERL_OPTIONS", "+fnu")
            .args([
                "-e",
                "k = :public_key.generate_key({:rsa, 2048, 65537}); File.write!(\"key.pem\", :public_key.pem_encode([:public_key.pem_entry_encode(:RSAPrivateKey, k)]))",
            ]),
        "a registry key",
    );
    run(
        &mut hex.mix(
            root,
            &home,
            &offline,
            &[
                "hex.registry",
                "build",
                "public",
                "--name",
                "hexpm",
                "--private-key",
                "key.pem",
            ],
        ),
        "mix hex.registry build",
    );
    public
}

/// A server of a folder on 127.0.0.1, for Hex: it writes down every path asked of it, and
/// refuses every tarball while told to.
pub struct Server {
    port: u16,
    asked: Arc<Mutex<Vec<String>>>,
    no_tarballs: Arc<AtomicBool>,
}

impl Server {
    pub fn start(public: &Path) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let no_tarballs = Arc::new(AtomicBool::new(false));
        let (log, refuse, root) = (asked.clone(), no_tarballs.clone(), public.to_path_buf());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, refuse, root) = (log.clone(), refuse.clone(), root.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(&stream);
                    let mut first = String::new();
                    if reader.read_line(&mut first).is_err() {
                        return;
                    }
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) if line == "\r\n" || line == "\n" => break,
                            Ok(_) => {}
                        }
                    }
                    let path = first
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/")
                        .to_string();
                    log.lock().unwrap().push(path.clone());
                    let refused = refuse.load(Ordering::SeqCst) && path.starts_with("/tarballs/");
                    let file = (!refused && !path.contains(".."))
                        .then(|| std::fs::read(root.join(path.trim_start_matches('/'))).ok())
                        .flatten();
                    let mut out = &stream;
                    let _ = match file {
                        Some(body) => out
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                                    body.len()
                                )
                                .as_bytes(),
                            )
                            .and_then(|()| out.write_all(&body)),
                        None => out.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        ),
                    };
                    let _ = out.flush();
                });
            }
        });
        Server {
            port,
            asked,
            no_tarballs,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Refuses every tarball from now on, or serves them again.
    pub fn refuse_tarballs(&self, refuse: bool) {
        self.no_tarballs.store(refuse, Ordering::SeqCst);
    }

    /// The tarballs asked for since the last call, by file name, sorted.
    pub fn tarballs_asked(&self) -> Vec<String> {
        let mut asked: Vec<String> = std::mem::take(&mut *self.asked.lock().unwrap())
            .into_iter()
            .filter_map(|p| p.strip_prefix("/tarballs/").map(str::to_string))
            .collect();
        asked.sort();
        asked
    }
}

/// A project's `mix.exs` that depends on `deps`, each `(name, version)`, from Hex.
pub fn mix_exs(app: &str, deps: &[(&str, &str)]) -> String {
    let deps: Vec<String> = deps
        .iter()
        .map(|(n, v)| format!("{{:{n}, \"{v}\"}}"))
        .collect();
    format!(
        "defmodule {}.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :{app}, version: \"0.1.0\", elixir: \"~> 1.14\", deps: [{}]]\n  end\nend\n",
        capitalized(app),
        deps.join(", ")
    )
}

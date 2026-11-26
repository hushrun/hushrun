//! `hushrun packages` with Bundler (K-6, K-10, K-45): a store baked by `bundle cache --no-install`
//! is seeded into the gem cache of an empty Bundler path, so that `bundle install` downloads none
//! of its gems. Needs Bundler 4 (`HUSHRUN_TEST_BUNDLER`, its `bundle`, with its
//! Ruby beside it). The gems come from a source in a folder the test writes: no network.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Output;

use repo::Repo;

use cli::Cli as _;

/// Writes a gem source at ARGV[0]: one gem of one file per `name:version` that follows, and the
/// full index Bundler reads from a `file://` source. A `name:version:ext` gem has an install
/// script, which writes the file `CANARY` names when it runs.
const SOURCE: &str = r##"require "rubygems/package"
require "tmpdir"
require "zlib"
require "fileutils"
dir = ARGV.fetch(0)
FileUtils.mkdir_p(File.join(dir, "gems"))
ARGV.drop(1).each do |pair|
  name, version, ext = pair.split(":")
  spec = Gem::Specification.new do |s|
    s.name = name
    s.version = version
    s.summary = name
    s.authors = ["hushrun"]
    s.license = "MIT"
    s.files = ["lib/#{name}.rb"]
    if ext
      s.files << "ext/extconf.rb"
      s.extensions = ["ext/extconf.rb"]
    end
  end
  Dir.mktmpdir do |src|
    FileUtils.mkdir_p(File.join(src, "lib"))
    File.write(File.join(src, "lib", "#{name}.rb"), "module #{name.capitalize}; VERSION = #{version.inspect}; end\n")
    if ext
      FileUtils.mkdir_p(File.join(src, "ext"))
      File.write(File.join(src, "ext", "extconf.rb"), "File.write(#{ENV.fetch("CANARY").inspect}, 'ran')\nFile.write('Makefile', \"all:\\n\\ninstall:\\n\\nclean:\\n\")\n")
    end
    Dir.chdir(src) { FileUtils.mv(Gem::Package.build(spec), File.join(dir, "gems")) }
  end
end
specs = Dir[File.join(dir, "gems", "*.gem")].sort.map { |g| Gem::Package.new(g).spec }
tuples = specs.map { |s| [s.name, s.version, s.platform.to_s] }
%w[specs latest_specs].each { |n| File.binwrite(File.join(dir, "#{n}.4.8.gz"), Zlib.gzip(Marshal.dump(tuples))) }
File.binwrite(File.join(dir, "prerelease_specs.4.8.gz"), Zlib.gzip(Marshal.dump([])))
FileUtils.mkdir_p(File.join(dir, "quick", "Marshal.4.8"))
specs.each do |s|
  File.binwrite(File.join(dir, "quick", "Marshal.4.8", "#{s.full_name}.gemspec.rz"), Zlib::Deflate.deflate(Marshal.dump(s)))
end
"##;

/// Bundler 4's `bundle`, from `HUSHRUN_TEST_BUNDLER`; none, said, where it is missing, and a
/// failure with `HUSHRUN_TEST_TOOLS=required`.
fn bundler() -> Option<PathBuf> {
    let found = std::env::var_os("HUSHRUN_TEST_BUNDLER")
        .map(PathBuf::from)
        .filter(|bundle| {
            hushrun_testkit::tool::command(bundle)
                .arg("--version")
                .output()
                .is_ok_and(|o| {
                    let text = String::from_utf8_lossy(&o.stdout);
                    // Bundler 4 says `4.0.18`; Bundler 2, `Bundler version 2.6.9`.
                    text.trim_start_matches("Bundler version ")
                        .starts_with("4.")
                })
        });
    if found.is_none() {
        assert!(
            std::env::var("HUSHRUN_TEST_TOOLS").as_deref() != Ok("required"),
            "Bundler 4 (HUSHRUN_TEST_BUNDLER) is missing, and HUSHRUN_TEST_TOOLS=required"
        );
        eprintln!(
            "note: skipping: Bundler 4 is missing (HUSHRUN_TEST_BUNDLER names its `bundle`; HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found
}

/// A repository whose Gemfile names `gems` of a source beside it, locked by Bundler with its
/// CHECKSUMS, and committed. The source has `tiny`, `other`, `extra`, and `evil`, whose install
/// script writes `canary` beside the repository.
fn fixture(bundle: &Path, gems: &[&str]) -> Repo {
    let bin = bundle.parent().expect("bundle's folder");
    let mut path = vec![bin.to_path_buf()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let r = Repo::new(&[(".gitignore".into(), ".bundle/\n".into())])
        .clear(&[
            "BUNDLE_PATH",
            "BUNDLE_GEMFILE",
            "BUNDLE_APP_CONFIG",
            "BUNDLE_GLOBAL_GEM_CACHE",
            "BUNDLE_USER_HOME",
            "BUNDLE_FROZEN",
            "BUNDLE_DEPLOYMENT",
            "GEM_HOME",
            "GEM_PATH",
            "RUBYOPT",
            "RUBYLIB",
            "HUSHRUN_PACKAGES_LOWER",
            "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
            "HUSHRUN_PACKAGES",
        ])
        .give("PATH", std::env::join_paths(path).expect("PATH"));
    let source = r.root.join("source");
    let script = r.root.join("source.rb");
    std::fs::write(&script, SOURCE).unwrap();
    let made = r
        .command(&bin.join("ruby").to_string_lossy())
        .arg(&script)
        .arg(&source)
        .args(["tiny:1.0.0", "other:2.0.0", "extra:1.0.0", "evil:1.0.0:ext"])
        .env("SOURCE_DATE_EPOCH", "0")
        .env("CANARY", r.root.join("canary"))
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", said(&made));
    gemfile(&r, gems);
    let locked = install(&r, bundle, &r.root.join("first"));
    assert!(locked.status.success(), "{}", said(&locked));
    let _ = std::fs::remove_file(r.root.join("canary"));
    let lock = std::fs::read_to_string(r.repo.join("Gemfile.lock")).unwrap();
    assert!(lock.contains("CHECKSUMS"), "{lock}");
    hushrun_testkit::git::commit_all(&r.repo, "lock");
    r
}

/// Writes a Gemfile that names `gems` of the source beside the repository.
fn gemfile(r: &Repo, gems: &[&str]) {
    let mut text = format!("source \"file://{}\"\n", r.root.join("source").display());
    for gem in gems {
        text.push_str(&format!("gem \"{gem}\"\n"));
    }
    r.write("Gemfile", &text);
}

/// `bundle install` with Bundler's path at `path`, outside hushrun.
fn install(r: &Repo, bundle: &Path, path: &Path) -> Output {
    r.command(&bundle.to_string_lossy())
        .arg("install")
        .env("BUNDLE_PATH", path)
        .output()
        .unwrap()
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The source's gems moved away, its index kept: a download of either fails from now on.
fn hide_the_sources_gems(r: &Repo) {
    let hidden = r.root.join("hidden");
    std::fs::create_dir_all(&hidden).unwrap();
    for gem in ["tiny-1.0.0.gem", "other-2.0.0.gem"] {
        std::fs::rename(r.root.join("source/gems").join(gem), hidden.join(gem)).unwrap();
    }
}

/// A bake with no command: Bundler's part fetches itself.
fn bake(r: &Repo, out: &Path) -> Output {
    r.hushrun()
        .args(["packages", "bake", "--force", "--out"])
        .arg(out)
        .output()
        .unwrap()
}

/// Where Ruby loads `tiny` from, with Bundler's path at `path`.
fn tiny_loads_from(r: &Repo, bundle: &Path, path: &Path) -> String {
    let out = r
        .command(&bundle.to_string_lossy())
        .args([
            "exec",
            "ruby",
            "-e",
            "require 'tiny'; print $LOADED_FEATURES.grep(/tiny\\.rb\\z/).first",
        ])
        .env("BUNDLE_PATH", path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", said(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// covers: K-6, K-10, K-45
#[test]
fn a_store_baked_by_bundle_cache_seeds_an_empty_bundler_path() {
    let Some(bundle) = bundler() else { return };
    let r = fixture(&bundle, &["tiny", "other"]);
    let store = r.root.join("packages");
    let baked = bake(&r, &store);
    let out = said(&baked);
    assert!(baked.status.success(), "{out}");
    assert!(
        out.contains("packages: bundler: 2 files"),
        "only the two downloads, not the installed gems: {out}"
    );
    assert!(
        out.contains("from fetched by `bundle cache --no-install` (Gemfile, Gemfile.lock)"),
        "{out}"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.join("packages.json")).unwrap()).unwrap();
    assert_eq!(json["parts"][0]["part"], "bundler");
    assert_eq!(
        json["parts"][0]["verified_by"], "lockfile",
        "the lockfile has CHECKSUMS"
    );
    let rows = std::fs::read_to_string(store.join("rows/bundler.jsonl")).unwrap();
    assert!(
        rows.contains(r#""path":"cache/tiny-1.0.0.gem""#)
            && rows.contains(r#""package":"tiny (1.0.0)""#)
            && rows.contains(r#""locked":"sha256="#),
        "{rows}"
    );
    // covers: K-10
    // The keys test: a gem loads from the same path whether it was downloaded or seeded, so what
    // the Ruby adapter records a test read is the same with and without the store.
    let downloaded = r.root.join("keys");
    assert!(install(&r, &bundle, &downloaded).status.success());
    let from_download = tiny_loads_from(&r, &bundle, &downloaded);
    assert!(
        from_download.starts_with(&downloaded.display().to_string()),
        "{from_download}"
    );
    std::fs::remove_dir_all(&downloaded).unwrap();
    hide_the_sources_gems(&r);
    let seeded = r
        .hushrun()
        .args([
            "packages",
            "exec",
            "--",
            &bundle.to_string_lossy(),
            "install",
        ])
        .env("BUNDLE_PATH", &downloaded)
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(seeded.status.success(), "{}", said(&seeded));
    assert_eq!(tiny_loads_from(&r, &bundle, &downloaded), from_download);
    // With the store and nothing else, not even the person's say-so: the lockfile verifies it.
    let exec = |path: &Path, lower: &Path| {
        r.hushrun()
            .args([
                "packages",
                "exec",
                "--",
                &bundle.to_string_lossy(),
                "install",
            ])
            .env("BUNDLE_PATH", path)
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let empty = r.root.join("empty");
    let read = exec(&empty, &store);
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(
        !out.contains("Fetching tiny") && !out.contains("Fetching other"),
        "{out}"
    );
    assert!(out.contains("Installing tiny 1.0.0"), "{out}");
    let cache: Vec<PathBuf> = std::fs::read_dir(empty.join("ruby"))
        .unwrap()
        .flatten()
        .map(|e| e.path().join("cache/tiny-1.0.0.gem"))
        .collect();
    assert!(cache.iter().all(|p| p.is_file()), "{cache:?}");
    // The control: without the store the same install fails.
    let control = exec(&r.root.join("control"), Path::new(""));
    assert!(!control.status.success(), "{}", said(&control));
    // covers: K-10
    // In `hushrun run` too; and the check's key is the same with and without the store: the
    // run without it skips.
    let run = |lower: &Path| {
        r.hushrun()
            .args([
                "run",
                "t",
                "--",
                &bundle.to_string_lossy(),
                "exec",
                "ruby",
                "-e",
                "require 'tiny'; exit(Tiny::VERSION == '1.0.0' ? 0 : 1)",
            ])
            .env("BUNDLE_PATH", &empty)
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let first = run(&store);
    assert!(first.status.success(), "{}", said(&first));
    assert!(said(&first).contains("t passed"), "{}", said(&first));
    let second = run(Path::new(""));
    assert!(said(&second).contains("t skipped"), "{}", said(&second));
}

// covers: K-45
#[test]
fn a_seed_refuses_what_the_lockfile_or_the_store_no_longer_vouches_for() {
    let Some(bundle) = bundler() else { return };
    let r = fixture(&bundle, &["tiny", "other"]);
    let store = r.root.join("packages");
    let baked = bake(&r, &store);
    assert!(baked.status.success(), "{}", said(&baked));
    let restore = |path: &Path, extra: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "restore"])
            .env("BUNDLE_PATH", path)
            .env("HUSHRUN_PACKAGES_LOWER", &store);
        for (n, v) in extra {
            cmd.env(n, v);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", said(&out));
        said(&out)
    };
    let out = restore(&r.root.join("a"), &[]);
    assert!(
        out.contains("packages: bundler: seeded 2 files into "),
        "{out}"
    );
    // Seeded again: nothing is written over.
    let out = restore(&r.root.join("a"), &[]);
    assert!(
        out.contains("packages: bundler: seeded already into"),
        "{out}"
    );
    // Its stamp gone, a seed again finds both there and writes over neither.
    for stamp in all_files(&r.root.join("a")) {
        if stamp
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".hushrun-packages-"))
        {
            std::fs::remove_file(stamp).unwrap();
        }
    }
    let out = restore(&r.root.join("a"), &[]);
    assert!(
        out.contains("seeded 0 files") && out.contains("(2 there already"),
        "{out}"
    );
    // The lockfile now hashes other bytes for tiny: tiny isn't seeded, other is.
    let lock = std::fs::read_to_string(r.repo.join("Gemfile.lock")).unwrap();
    let tiny = lock
        .lines()
        .find(|l| l.starts_with("  tiny (1.0.0) sha256="))
        .unwrap()
        .to_string();
    r.write(
        "Gemfile.lock",
        &lock.replace(&tiny, &format!("  tiny (1.0.0) sha256={}", "0".repeat(64))),
    );
    let out = restore(&r.root.join("b"), &[]);
    assert!(
        out.contains("seeded 1 files")
            && out.contains(
                "left out 1 files whose bytes changed since the bake, such as cache/tiny-1.0.0.gem"
            ),
        "{out}"
    );
    r.write("Gemfile.lock", &lock);
    // A file of the store changed in place, same size: found by its row's hash as it is copied.
    let gem = store.join("parts/bundler/cache/other-2.0.0.gem");
    let mut bytes = std::fs::read(&gem).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&gem, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(&gem, &bytes).unwrap();
    let out = restore(&r.root.join("c"), &[]);
    assert!(out.contains("such as cache/other-2.0.0.gem"), "{out}");
    let seeded = all_files(&r.root.join("c"));
    assert!(
        seeded.iter().any(|f| f.ends_with("tiny-1.0.0.gem"))
            && !seeded.iter().any(|f| f.ends_with("other-2.0.0.gem")),
        "{seeded:?}"
    );
}

// covers: K-7, K-10
#[test]
fn a_lockfile_without_checksums_makes_a_part_only_a_signature_verifies() {
    let Some(bundle) = bundler() else { return };
    let r = fixture(&bundle, &["tiny", "other"]);
    let lock = std::fs::read_to_string(r.repo.join("Gemfile.lock")).unwrap();
    let mut without = String::new();
    let mut skipping = false;
    for line in lock.lines() {
        if line == "CHECKSUMS" {
            skipping = true;
        } else if skipping && line.is_empty() {
            skipping = false;
            continue;
        }
        if !skipping {
            without.push_str(line);
            without.push('\n');
        }
    }
    r.write("Gemfile.lock", &without);
    hushrun_testkit::git::commit_all(&r.repo, "no checksums");
    let store = r.root.join("packages");
    let baked = bake(&r, &store);
    assert!(baked.status.success(), "{}", said(&baked));
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.join("packages.json")).unwrap()).unwrap();
    assert_eq!(json["parts"][0]["verified_by"], "signature");
    let restore = |extra: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "restore"])
            .env("BUNDLE_PATH", r.root.join("p"))
            .env("HUSHRUN_PACKAGES_LOWER", &store);
        for (n, v) in extra {
            cmd.env(n, v);
        }
        said(&cmd.output().unwrap())
    };
    let out = restore(&[]);
    assert!(!out.contains("seeded"), "unsigned and unaccepted: {out}");
    let out = restore(&[("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")]);
    assert!(out.contains("seeded 2 files"), "{out}");
    let out = restore(&[("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"), ("CI", "true")]);
    assert!(
        !out.contains("seeded 2"),
        "CI never reads an unverified store: {out}"
    );
}

// covers: K-45
#[test]
fn a_lockfile_changed_by_one_gem_downloads_that_gem_and_seeds_the_rest() {
    let Some(bundle) = bundler() else { return };
    let r = fixture(&bundle, &["tiny", "other"]);
    let store = r.root.join("packages");
    let baked = bake(&r, &store);
    assert!(baked.status.success(), "{}", said(&baked));
    hide_the_sources_gems(&r);
    // One more gem, which the source still has and the store doesn't.
    gemfile(&r, &["tiny", "other", "extra"]);
    let locked = r
        .command(&bundle.to_string_lossy())
        .arg("lock")
        .output()
        .unwrap();
    assert!(locked.status.success(), "{}", said(&locked));
    let read = r
        .hushrun()
        .args([
            "packages",
            "exec",
            "--",
            &bundle.to_string_lossy(),
            "install",
        ])
        .env("BUNDLE_PATH", r.root.join("p"))
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(out.contains("Fetching extra 1.0.0"), "{out}");
    assert!(
        !out.contains("Fetching tiny") && !out.contains("Fetching other"),
        "{out}"
    );
}

// covers: K-6
#[test]
fn no_gems_install_script_runs_in_a_bake() {
    let Some(bundle) = bundler() else { return };
    let r = fixture(&bundle, &["tiny", "evil"]);
    let canary = r.root.join("canary");
    let store = r.root.join("packages");
    let baked = bake(&r, &store);
    assert!(baked.status.success(), "{}", said(&baked));
    assert!(
        !canary.exists(),
        "the bake ran evil's install script: {}",
        said(&baked)
    );
    assert!(store.join("parts/bundler/cache/evil-1.0.0.gem").is_file());
    let files = all_files(&store.join("parts/bundler"));
    assert_eq!(
        files.len(),
        2,
        "the two .gem files and nothing else: {files:?}"
    );
    // The control: an install outside the bake runs it.
    assert!(install(&r, &bundle, &r.root.join("q")).status.success());
    assert!(canary.is_file());
}

/// Every file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

// covers: K-6
#[test]
fn with_no_bundle_on_path_the_part_is_left_out_with_a_line() {
    let r = Repo::new(&[
        ("Gemfile".into(), "source \"https://rubygems.org\"\n".into()),
        (
            "Gemfile.lock".into(),
            "GEM\n  specs:\n\nBUNDLED WITH\n  4.0.18\n".into(),
        ),
    ]);
    // PATH holds git, which the bake asks for the tracked files, and no bundle.
    let bin = r.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("git"))
        .find(|g| g.is_file())
        .expect("git");
    #[cfg(unix)]
    std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    let out = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(r.root.join("packages"))
        .env("PATH", &bin)
        .output()
        .unwrap();
    let said = said(&out);
    assert!(!out.status.success(), "nothing else to bake: {said}");
    assert!(
        said.contains("packages: bundler: left out: there's no bundle here to fetch with"),
        "{said}"
    );
}

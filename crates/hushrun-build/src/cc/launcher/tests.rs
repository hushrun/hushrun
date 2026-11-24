//! The launcher's flow over stand-in ports: what happens when the machinery fails.

use super::*;

/// Ports that record what the launcher did and fail where a test says.
struct Fake {
    vars: Vec<(String, String)>,
    cwd: PathBuf,
    execs: Vec<(OsString, Vec<OsString>)>,
    compiles: u32,
    panic_in_ask: bool,
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Ports for Fake {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| OsString::from(v))
    }
    fn var_names(&self) -> Vec<OsString> {
        self.vars.iter().map(|(n, _)| OsString::from(n)).collect()
    }
    fn cwd(&self) -> io::Result<PathBuf> {
        Ok(self.cwd.clone())
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn file_clock(&self, _dir: &Path) -> Option<SystemTime> {
        None
    }
    fn pid(&self) -> u32 {
        1
    }
    fn exec(&mut self, compiler: &OsStr, args: &[OsString]) -> i32 {
        self.execs.push((compiler.to_owned(), args.to_vec()));
        42
    }
    fn compile(&mut self, _run: &Run<'_>) -> io::Result<Ran> {
        self.compiles += 1;
        Ok(Ran::default())
    }
    fn ask(&self, _program: &Path, _args: &[String]) -> Option<Said> {
        assert!(!self.panic_in_ask, "the machinery fails");
        None
    }
    fn stdout(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }
    fn stderr(&mut self, bytes: &[u8]) {
        self.err.extend_from_slice(bytes);
    }
}

fn session(dir: &Path, cc: bool) -> PathBuf {
    let folder = dir.join("session");
    std::fs::create_dir_all(&folder).unwrap();
    let current = serde_json::json!({
        "mode": "on",
        "store": dir.join("store"),
        "keep_days": 15,
        "never_store": [],
        "env": [],
        "min_free_gb": 0,
        "repository": null,
        "started_ms": 0,
        "shared": [],
        "folder": folder,
        "cc": if cc { serde_json::json!({"roots": [dir.to_string_lossy()], "maps": true}) } else { serde_json::Value::Null },
    });
    std::fs::write(folder.join("session.json"), current.to_string()).unwrap();
    folder
}

fn fake(dir: &Path, folder: Option<&Path>) -> Fake {
    let mut vars = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
    if let Some(folder) = folder {
        vars.push((
            session::SESSION.into(),
            folder.to_string_lossy().into_owned(),
        ));
    }
    Fake {
        vars,
        cwd: dir.to_path_buf(),
        execs: Vec::new(),
        compiles: 0,
        panic_in_ask: false,
        out: Vec::new(),
        err: Vec::new(),
    }
}

fn argv(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

const COMPILE: &[&str] = &[
    "/usr/bin/cc",
    "-c",
    "a.c",
    "-o",
    "a.o",
    "-MD",
    "-MF",
    "a.o.d",
];

// covers: B-62
#[test]
fn without_a_session_or_its_launcher_part_the_compiler_runs_as_asked() {
    let dir = tempfile::tempdir().unwrap();
    for folder in [None, Some(session(dir.path(), false))] {
        let mut ports = fake(dir.path(), folder.as_deref());
        assert_eq!(call(&mut ports, &argv(COMPILE)), 42);
        assert_eq!(
            ports.execs,
            vec![(OsString::from("/usr/bin/cc"), argv(&COMPILE[1..]))]
        );
        assert_eq!(ports.compiles, 0);
        assert!(ports.out.is_empty() && ports.err.is_empty());
    }
}

// covers: B-58
#[test]
fn a_panic_before_any_output_runs_the_compiler_as_asked_and_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let folder = session(dir.path(), true);
    let mut ports = fake(dir.path(), Some(&folder));
    ports.panic_in_ask = true;
    std::panic::set_hook(Box::new(|_| {}));
    let code = call(&mut ports, &argv(COMPILE));
    let _ = std::panic::take_hook();
    assert_eq!(code, 42);
    assert_eq!(
        ports.execs,
        vec![(OsString::from("/usr/bin/cc"), argv(&COMPILE[1..]))]
    );
    assert_eq!(ports.compiles, 0);
    assert!(ports.out.is_empty() && ports.err.is_empty());
    // The run's log says why.
    let log = std::fs::read_to_string(folder.join("log.jsonl")).unwrap();
    assert!(log.contains("\"why\":\"panic\""), "{log}");
}

// covers: B-51, B-58
#[test]
fn a_call_passed_through_is_exec_ed_with_its_arguments_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let folder = session(dir.path(), true);
    let mut ports = fake(dir.path(), Some(&folder));
    let link = ["/usr/bin/cc", "a.o", "-o", "prog"];
    assert_eq!(call(&mut ports, &argv(&link)), 42);
    assert_eq!(ports.execs[0].1, argv(&link[1..]));
    // A compiler the identity can't find passes through too.
    let mut ports = fake(dir.path(), Some(&folder));
    let missing = ["/nonexistent/cc", "-c", "a.c", "-o", "a.o"];
    assert_eq!(call(&mut ports, &argv(&missing)), 42);
    assert_eq!(ports.compiles, 0);
    let log = std::fs::read_to_string(folder.join("log.jsonl")).unwrap();
    assert!(
        log.contains("\"link\"") && log.contains("\"compiler\""),
        "{log}"
    );
}

// covers: B-55
#[test]
fn the_compile_takes_its_own_depfile_with_the_call_s_targets_and_the_maps_last() {
    let compile = match args::classify(
        &OsString::from("cc"),
        &argv(&[
            "-MD", "-MT", "t.o", "-MF", "t.d", "-MP", "-c", "a.c", "-o", "a.o", "-MQa$b",
        ]),
        &[],
    ) {
        Classified::Compile(c) => *c,
        other => panic!("{other:?}"),
    };
    let args = compile_args(
        &compile,
        Path::new("/s/cc/1.d"),
        &["-ffile-prefix-map=/r=.".into()],
    );
    assert_eq!(
        args,
        argv(&[
            "-c",
            "a.c",
            "-o",
            "a.o",
            "-MD",
            "-MF",
            "/s/cc/1.d",
            "-MT",
            "t.o",
            "-MQ",
            "a$b",
            "-MP",
            "-ffile-prefix-map=/r=."
        ])
    );
    let places = Places::new(&["/r/ab".into(), "/private/r/ab".into()], &[], &[]);
    assert_eq!(
        added_flags(
            &CcRun {
                roots: places.roots().to_vec(),
                maps: true
            },
            &places
        ),
        [
            "-ffile-prefix-map=/r/ab=.",
            "-ffile-prefix-map=/private/r/ab=."
        ]
    );
    assert!(
        added_flags(
            &CcRun {
                roots: vec![],
                maps: false
            },
            &places
        )
        .is_empty()
    );
}

// covers: B-57
#[test]
fn a_file_put_in_place_is_new_dated_and_writable_or_not_there_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.o");
    std::fs::write(&dest, b"old").unwrap();
    let when = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    put_whole(&dest, when, |temp| std::fs::write(temp, b"new")).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(std::fs::metadata(&dest).unwrap().modified().unwrap(), when);
    assert!(!std::fs::metadata(&dest).unwrap().permissions().readonly());
    // A failure leaves the old file and no temporary one.
    let failed = put_whole(&dest, when, |temp| {
        std::fs::write(temp, b"half")?;
        Err(io::Error::other("no"))
    });
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

// covers: B-51
#[test]
fn cmake_s_own_configure_checks_are_probes() {
    assert!(cmake_probe(
        Path::new("/b/CMakeFiles/CMakeScratch/TryCompile-x"),
        "o.o"
    ));
    assert!(cmake_probe(Path::new("/b/CMakeFiles/CMakeTmp"), "o.o"));
    assert!(!cmake_probe(Path::new("/b"), "CMakeFiles/lib.dir/a.c.o"));
}

// covers: B-238
#[test]
fn autoconf_s_and_meson_s_own_checks_are_probes() {
    assert!(configure_probe(
        Path::new("/w/p"),
        "conftest.c",
        "conftest.o"
    ));
    assert!(configure_probe(
        Path::new("/w/p"),
        "conftest.cpp",
        "conftest.o"
    ));
    assert!(configure_probe(
        Path::new("/w/p"),
        "conftest.c",
        "conftest2.o"
    ));
    assert!(configure_probe(
        Path::new("/w/b/meson-private/tmpa1b2c3"),
        "testfile.c",
        "output.obj"
    ));
    assert!(configure_probe(
        Path::new("/w/b"),
        "../src/a.c",
        "meson-private/sanitycheckc.exe"
    ));
    assert!(!configure_probe(
        Path::new("/w/b"),
        "../src/a.c",
        "a.p/a.c.o"
    ));
    assert!(!configure_probe(
        Path::new("/w"),
        "src/conftest.c",
        "src/conftest_real.o"
    ));
}

// covers: B-57, B-58
#[test]
fn what_a_killed_launcher_left_is_replaced_and_a_gone_session_is_not_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.o");
    // A launcher killed between its copy and its rename left its temporary file.
    std::fs::write(dir.path().join(".a.o.hushrun.tmp"), b"half").unwrap();
    put_whole(&dest, SystemTime::now(), |temp| {
        std::fs::write(temp, b"whole")
    })
    .unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a.o"]);
    // The run's folder went with the run: the launcher's folder in it isn't made again.
    let session = dir.path().join("session");
    assert!(!work_folder(&session));
    assert!(!session.exists());
    std::fs::create_dir(&session).unwrap();
    assert!(work_folder(&session) && work_folder(&session));
}

// covers: B-60
#[test]
fn a_downloaded_hint_naming_a_folder_outside_the_roots_and_bases_is_passed_over() {
    let places = Places::new(&["/home/u/ck".into()], &["/sdk".into()], &[]);
    let identity = Identity {
        digest: Digest::of(b"x"),
        clang: true,
        target: String::new(),
        sdk: Some("/sdk".into()),
        sdk_resolved: None,
        toolchain: None,
        system: vec![],
    };
    let cwd = PathBuf::from("/home/u/ck/build");
    let seen = Seen {
        places: &places,
        identity: &identity,
        cwd: &cwd,
    };
    let read = |p: &str| key::HintRead {
        p: p.into(),
        s: false,
    };
    let hint = |reads: &[&str], places: &[&str], probes: &[&str]| {
        key::Hint::new(
            reads.iter().map(|r| read(r)).collect(),
            places.iter().map(|p| p.to_string()).collect(),
            probes.iter().map(|p| (p.to_string(), false)).collect(),
        )
    };
    let fine = hint(
        &["{root}/a.c", "../inc/a.h", "{sdk}/usr/include/stdio.h"],
        &["{root}/src/a.h"],
        &["{sdk}/usr/include/x.h"],
    );
    assert!(names_only_known_folders(&fine, &seen));
    for bad in [
        hint(&["{root}/a.c", "/etc/passwd"], &[], &[]),
        hint(&["{root}/../../etc/passwd"], &[], &[]),
        hint(&["{root}/a.c"], &["/home/u/secret/x.h"], &[]),
        hint(&["{root}/a.c"], &[], &["/home/u/.ssh/id_rsa"]),
    ] {
        assert!(!names_only_known_folders(&bad, &seen), "{bad:?}");
    }
}

// covers: B-56
#[test]
fn a_root_is_found_in_an_object_in_every_width_and_byte_order() {
    let root = "/home/u/ck";
    let wide = |units: Vec<u8>| {
        let mut object = b"\x7fELF....".to_vec();
        object.extend(units);
        object
    };
    let le16: Vec<u8> = root.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let be16: Vec<u8> = root.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let le32: Vec<u8> = root
        .chars()
        .flat_map(|c| u32::from(c).to_le_bytes())
        .collect();
    let be32: Vec<u8> = root
        .chars()
        .flat_map(|c| u32::from(c).to_be_bytes())
        .collect();
    for units in [root.as_bytes().to_vec(), le16, be16, le32, be32] {
        assert!(holds_text(&wide(units.clone()), root), "{units:?}");
    }
    assert!(!holds_text(&wide(b"/home/u/other".to_vec()), root));
    assert!(!holds_text(b"anything", ""));
    // A macro whose value holds a root ties whatever the object holds.
    let roots = [root.to_string()];
    let keyed = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert!(defines_a_root(
        &keyed(&["-DDIR=\"/home/u/ck/data\""]),
        &roots
    ));
    assert!(defines_a_root(&keyed(&["-D", "DIR=/home/u/ck"]), &roots));
    assert!(!defines_a_root(
        &keyed(&["-I/home/u/ck/inc", "-DX=1"]),
        &roots
    ));
}

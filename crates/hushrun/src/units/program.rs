//! \[ADR 0077\] Programs on COMMAND's `PATH` that are hushrun itself (RFC 0004 §4.7, §4.8): how an
//! adapter gets inside a runner that takes nothing from the environment, as hushrun's target
//! runner gets inside Cargo ([`super::runner`]), with COMMAND byte for byte as written.
//!
//! For a run, an adapter [makes](make) a directory in the run's own directory holding a program
//! for each name it asks for, `swift` or `ctest`, and puts it first on COMMAND's `PATH`
//! ([`variables`]). Each program is hushrun in a hidden mode: on Unix a script that starts
//! `<hushrun> __hushrun-program <name> <argument>…`; on Windows a hard link to (or a copy of)
//! hushrun's executable named `<name>.exe`, which knows its name from its own file. In that mode
//! hushrun finds the **real** program, the first `<name>` on `PATH` as COMMAND's environment had
//! it that isn't in a directory of programs like this one ([`real`]), asks the adapter's
//! [`Program`] for the arguments to start it with, and starts it with the environment COMMAND
//! had: `PATH` as it was, and neither of this module's variables. Stdio, the exit status and the
//! signals that stop a process pass through untouched, as the target runner passes them.
//!
//! Any doubt starts the real program with its arguments unchanged: a name no [`Program`] is for,
//! or a run with no directory of its own. A real program that can't be found exits 127, as a
//! shell does.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The hidden subcommand a program on `PATH` starts hushrun with: never shown, never typed.
pub const SUBCOMMAND: &str = "__hushrun-program";
/// The variable that names the directory of programs hushrun put first on `PATH`.
pub const DIR_VAR: &str = "HUSHRUN_PROGRAMS";
/// The variable that holds `PATH` as COMMAND's environment had it: `=` and its value, or empty
/// when it had none.
pub const PATH_WAS: &str = "HUSHRUN_PROGRAMS_PATH";
/// A file in every directory of programs, which [`real`] passes over: so one run's program never
/// starts another's, or itself, for ever.
pub const MARKER: &str = ".hushrun-programs";

/// What an adapter does with one program it puts on `PATH`.
pub trait Program: Sync {
    /// The program's name: `swift`, `ctest`.
    fn name(&self) -> &'static str;

    /// The arguments to start the real program with, from those COMMAND's tools gave it.
    fn args(&self, call: &Call<'_>) -> Vec<OsString>;

    /// After the real program ended, with the arguments it was started with and its exit code
    /// (`None` when a signal stopped it): an adapter may read what it wrote then.
    fn ended(&self, call: &Call<'_>, args: &[OsString], exit: Option<i32>) {
        let _ = (call, args, exit);
    }

    /// \[ADR 0086\] Runs the real program itself, for an adapter that needs more than changed
    /// arguments: its output as it goes by ([`run_copying_stdout`]), or the real program started
    /// again after it ([`real_command`]). The exit code this program on `PATH` ends with; `None`,
    /// the default, leaves the start to [`Program::args`] and [`Program::ended`]. Asked only in a
    /// run with a directory of its own.
    fn run(&self, call: &Call<'_>) -> Option<ExitCode> {
        let _ = call;
        None
    }
}

/// \[ADR 0077\] The programs adapters put on `PATH`, by name. A new one is one entry here.
pub const PROGRAMS: &[&dyn Program] = &[
    &super::ctest::Ctest,
    &super::swift::SWIFT,
    &super::swift::SWIFT_TEST,
];

/// \[ADR 0086\] A command that starts the real program `real` as a program on `PATH` starts it:
/// with `PATH` as COMMAND's environment had it and neither of this module's variables, so that
/// nothing it starts reaches a program of hushrun's.
pub fn real_command(real: &Path) -> Command {
    let mut command = Command::new(real);
    as_command_had(&mut command);
    command
}

/// \[C-32\] Gives `command` the environment COMMAND had, as [`real_command`] does: `PATH` as it
/// was, and neither of this module's variables. Nothing changes outside a program on `PATH`.
pub fn as_command_had(command: &mut Command) {
    match path_was(std::env::var_os(PATH_WAS)) {
        Some(Some(path)) => {
            command.env("PATH", path);
        }
        Some(None) => {
            command.env_remove("PATH");
        }
        None => {}
    }
    command.env_remove(DIR_VAR).env_remove(PATH_WAS);
}

/// \[ADR 0086\] How a real program run by [`run_copying_stdout`] ended, and what it wrote on its
/// standard output.
#[derive(Debug, Default)]
pub struct Copied {
    /// Its exit code, when it exited.
    pub exit: Option<i32>,
    /// The signal that stopped it, when one did.
    pub signal: Option<i32>,
    /// The lines of its standard output the caller wanted, each whole with its ending, as
    /// written, until it ended, or until its output was handed to a forwarder ([`Copied::held`]).
    pub stdout: Vec<u8>,
    /// How many bytes those lines came to, kept or not.
    pub wanted: u64,
    /// \[ADR 0086\] Something it started still held its standard output after it ended: what it
    /// printed can't be read whole, so no pass of it is recorded. What comes later is passed on
    /// all the same, by a forwarder of its own ([`FORWARD`]).
    pub held: bool,
    /// The lines wanted came to more than [`KEEP_MOST`]: those after weren't kept (they were
    /// passed on), so what it printed can't be read whole and no pass of it is recorded.
    pub cut: bool,
}

/// \[ADR 0086\] The most of a program's output [`run_copying_stdout`] keeps to read.
pub const KEEP_MOST: usize = 16 << 20;

/// \[ADR 0086\] The longest line [`run_copying_stdout`] keeps whole: what follows on that line
/// isn't kept.
const LINE_MOST: usize = 64 << 10;

/// \[ADR 0086\] What [`run_copying_stdout`] keeps of a program's output, as it goes by: each line
/// `wanted` takes, whole with its ending (`\n` or `\r`), up to [`KEEP_MOST`] in all.
struct Lines {
    wanted: fn(&[u8]) -> bool,
    line: Vec<u8>,
    kept: Vec<u8>,
    bytes: u64,
    cut: bool,
}

impl Lines {
    fn new(wanted: fn(&[u8]) -> bool) -> Lines {
        Lines {
            wanted,
            line: Vec::new(),
            kept: Vec::new(),
            bytes: 0,
            cut: false,
        }
    }

    fn take(&mut self, mut bytes: &[u8]) {
        while let Some(at) = bytes.iter().position(|b| *b == b'\n' || *b == b'\r') {
            self.push(&bytes[..at]);
            self.end(bytes[at]);
            bytes = &bytes[at + 1..];
        }
        self.push(bytes);
    }

    fn push(&mut self, part: &[u8]) {
        let room = LINE_MOST.saturating_sub(self.line.len());
        self.line.extend_from_slice(&part[..part.len().min(room)]);
    }

    fn end(&mut self, ending: u8) {
        if !self.line.is_empty() && (self.wanted)(&self.line) {
            let size = self.line.len() + 1;
            self.bytes += size as u64;
            if self.kept.len() + size <= KEEP_MOST {
                self.kept.extend_from_slice(&self.line);
                self.kept.push(ending);
            } else {
                self.cut = true;
            }
        }
        self.line.clear();
    }

    /// What was kept, what the wanted lines came to, and whether some weren't kept.
    fn finish(mut self) -> (Vec<u8>, u64, bool) {
        self.end(b'\n');
        (self.kept, self.bytes, self.cut)
    }
}

/// \[ADR 0086\] How long output must stay quiet after the program ended for [`run_copying_stdout`]
/// to tell "still held open by a program it left running" from "closing now": what its last
/// writers flush as they exit comes within milliseconds, so a quarter second is ample.
pub const QUIET: std::time::Duration = std::time::Duration::from_millis(250);
/// The longest [`run_copying_stdout`] goes on reading after the program ended, for output that
/// never goes quiet, before it hands the rest over. Only the Unix reader stops early: elsewhere
/// it reads to the end.
#[cfg(unix)]
pub const MOST_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
/// \[ADR 0086\] The hidden subcommand of the forwarder [`run_copying_stdout`] hands held output
/// to: hushrun copying its standard input to its standard output until the end. Never typed.
pub const FORWARD: &str = "__hushrun-forward";

impl Copied {
    /// The exit code a program on `PATH` ends with for it: its own, whole (on Windows past 255
    /// too), or the same signal.
    pub fn exit_code(&self) -> ExitCode {
        super::runner::end_with(self.exit, self.signal)
    }
}

/// \[ADR 0086\] Runs `command` with its standard output copied, byte for byte, to this process's
/// as it goes by, and its lines `wanted` takes kept, as they go by ([`Copied::stdout`]), so that
/// a program that prints much besides is still read: through a pseudo-terminal in raw mode, with the window's size, when
/// this process's standard output is a terminal (a program that colors only on one, Swift Testing,
/// still does), else through a pipe. Standard input and error are inherited untouched, and the
/// signals that stop a process are passed on, as [`Program::args`]'s start does. `Err` when it
/// can't be started.
///
/// Once the program ended, its output ends when every program holding it let it go. Output still
/// held open after it ([`QUIET`], at most [`MOST_AFTER`]) by a program it left running is handed,
/// with what the pipe holds, to a forwarder that hushrun starts and doesn't wait for (hushrun
/// again, [`FORWARD`], with the read end as its standard input and this process's standard output
/// as its own), as a shell lets that program go on printing: nothing waits, nothing is lost, and
/// the run is [`Copied::held`].
pub fn run_copying_stdout(
    mut command: Command,
    wanted: fn(&[u8]) -> bool,
) -> std::io::Result<Copied> {
    let (mut from, to) = output_channel()?;
    command.stdout(to);
    let mut child = command.spawn()?;
    // Only the child holds its end now: reading ends when it, and whatever it started, closes it.
    drop(command);
    let forwarding = super::runner::forward_signals(child.id());
    // The lines wanted kept up to KEEP_MOST to be read; every byte passed on.
    let mut lines = Lines::new(wanted);
    let (status, held) = copy_until_held(&mut from, &mut child, &mut |bytes| lines.take(bytes));
    drop(forwarding);
    if held {
        hand_over(from);
    }
    let (kept, wanted, cut) = lines.finish();
    Ok(match status {
        Some(status) => Copied {
            exit: status.code(),
            signal: super::runner::signal_of(&status),
            stdout: kept,
            wanted,
            held,
            cut,
        },
        None => Copied {
            stdout: kept,
            wanted,
            held,
            cut,
            ..Copied::default()
        },
    })
}

/// \[ADR 0083, 0086\] The one copy of a program's output for a stand-in (hushrun's `ctest` and
/// `swift` on `PATH`, and the runner of Go's test binaries): what comes from `from` goes on to
/// this process's standard output as it comes, and to `sink`, until it ends, or, once `child`
/// ended, until it has been quiet for [`QUIET`] (at most [`MOST_AFTER`]). How the child ended,
/// and whether its output was still held open then: [`hand_over`] then gives the rest to a
/// forwarder, so that what reads this process's output sees what it would without hushrun, the
/// program gone and its output still open.
#[cfg(unix)]
pub(super) fn copy_until_held(
    from: &mut std::fs::File,
    child: &mut std::process::Child,
    sink: &mut dyn FnMut(&[u8]),
) -> (Option<std::process::ExitStatus>, bool) {
    use std::io::{Read as _, Write as _};
    use std::time::Instant;
    // Read without blocking, so that the program's end is seen while its output stays open.
    let flags = rustix::fs::fcntl_getfl(&*from).ok();
    if let Some(flags) = flags {
        let _ = rustix::fs::fcntl_setfl(&*from, flags | rustix::fs::OFlags::NONBLOCK);
    }
    let mut buf = vec![0u8; 64 * 1024];
    let out = std::io::stdout();
    let mut status: Option<Option<std::process::ExitStatus>> = None;
    let mut ended_at: Option<Instant> = None;
    let mut last = Instant::now();
    let mut held = false;
    loop {
        // Every pass, whatever arrives, asks whether the program ended, and counts from its end.
        let read = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut out = out.lock();
                let _ = out.write_all(&buf[..n]).and_then(|()| out.flush());
                drop(out);
                sink(&buf[..n]);
                last = Instant::now();
                true
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => false,
            // Once nothing holds a terminal, Linux answers EIO, and macOS the end.
            Err(_) => break,
        };
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(done)) => {
                    status = Some(Some(done));
                    ended_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(_) => {
                    status = Some(None);
                    ended_at = Some(Instant::now());
                }
            }
        }
        if let Some(at) = ended_at
            && (last.max(at).elapsed() >= QUIET || at.elapsed() >= MOST_AFTER)
        {
            held = true;
            break;
        }
        if !read {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // A reader of what is left (the forwarder) reads as any reader does: blocking again.
    if let Some(flags) = flags {
        let _ = rustix::fs::fcntl_setfl(&*from, flags);
    }
    let status = match status {
        Some(status) => status,
        None => wait(child),
    };
    (status, held)
}

/// [`copy_until_held`] where output can't be read without blocking: it is read to its end, and
/// nothing is held.
#[cfg(not(unix))]
pub(super) fn copy_until_held(
    from: &mut std::fs::File,
    child: &mut std::process::Child,
    sink: &mut dyn FnMut(&[u8]),
) -> (Option<std::process::ExitStatus>, bool) {
    use std::io::{Read as _, Write as _};
    let mut buf = vec![0u8; 64 * 1024];
    let out = std::io::stdout();
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut out = out.lock();
                let _ = out.write_all(&buf[..n]).and_then(|()| out.flush());
                drop(out);
                sink(&buf[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    (wait(child), false)
}

/// How `child` ended, when that can be told.
fn wait(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    loop {
        match child.wait() {
            Ok(done) => return Some(done),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Hands `from` to a forwarder this process doesn't wait for, which copies it to this process's
/// standard output until its end; when none can be started, copies it here, to its end.
#[cfg(unix)]
pub(super) fn hand_over(from: std::fs::File) {
    // A unit test's own executable is its test harness, not hushrun: there `cat` forwards.
    #[cfg(test)]
    let exe: std::io::Result<PathBuf> = Ok(PathBuf::from("cat"));
    #[cfg(not(test))]
    let exe = std::env::current_exe();
    let started = exe.and_then(|exe| {
        let mut command = Command::new(&exe);
        if !cfg!(test) {
            command.arg(FORWARD);
        }
        command
            .stdin(from.try_clone()?)
            .stderr(std::process::Stdio::null())
            .spawn()
    });
    if started.is_err() {
        forward(from);
    }
}

/// Copies `from` to this process's standard output until its end: what the forwarder does.
fn forward(mut from: impl std::io::Read) {
    use std::io::Write as _;
    let mut buf = vec![0u8; 64 * 1024];
    let out = std::io::stdout();
    loop {
        match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let mut out = out.lock();
                if out.write_all(&buf[..n]).and_then(|()| out.flush()).is_err() {
                    return;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Where nothing is held, nothing is handed over: what is left is copied here.
#[cfg(not(unix))]
pub(super) fn hand_over(from: std::fs::File) {
    forward(from);
}

/// A pipe's read end as a file, for [`copy_until_held`] and [`hand_over`].
pub(super) fn pipe_file(reader: std::io::PipeReader) -> std::fs::File {
    #[cfg(unix)]
    {
        std::fs::File::from(std::os::fd::OwnedFd::from(reader))
    }
    #[cfg(windows)]
    {
        std::fs::File::from(std::os::windows::io::OwnedHandle::from(reader))
    }
}

/// The end this process reads and the end the child writes its standard output to: a
/// pseudo-terminal when this process's standard output is a terminal, else a pipe.
#[cfg(unix)]
fn output_channel() -> std::io::Result<(std::fs::File, std::process::Stdio)> {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::{
        OptionalActions, isatty, tcgetattr, tcgetwinsize, tcsetattr, tcsetwinsize,
    };
    if !isatty(std::io::stdout()) {
        let (from, to) = std::io::pipe()?;
        return Ok((
            std::fs::File::from(std::os::fd::OwnedFd::from(from)),
            to.into(),
        ));
    }
    let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    // Never the child's: only this process reads it.
    rustix::io::fcntl_setfd(&controller, rustix::io::FdFlags::CLOEXEC)?;
    grantpt(&controller)?;
    unlockpt(&controller)?;
    let name = ptsname(&controller, Vec::new())?;
    let terminal = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    // Raw: the bytes arrive as the child wrote them, no `\n` made `\r\n`.
    let mut modes = tcgetattr(&terminal)?;
    modes.make_raw();
    tcsetattr(&terminal, OptionalActions::Now, &modes)?;
    if let Ok(size) = tcgetwinsize(std::io::stdout()) {
        let _ = tcsetwinsize(&terminal, size);
    }
    Ok((
        std::fs::File::from(controller),
        std::fs::File::from(terminal).into(),
    ))
}

#[cfg(not(unix))]
fn output_channel() -> std::io::Result<(std::fs::File, std::process::Stdio)> {
    let (from, to) = std::io::pipe()?;
    Ok((pipe_file(from), to.into()))
}

/// One start of a program on `PATH`.
pub struct Call<'a> {
    /// Its name.
    pub name: &'a str,
    /// The arguments it was started with, its name aside.
    pub args: &'a [OsString],
    /// The real program.
    pub real: &'a Path,
    /// The run's `HUSHRUN_UNITS` directory, when there is one.
    pub units: Option<&'a Path>,
}

/// Makes the directory of programs for a run in its own directory `run_dir`: one program named
/// each of `names`, that starts `exe`, hushrun, in its hidden mode. The directory's path, which
/// [`variables`] puts first on `PATH`.
pub fn make(run_dir: &Path, exe: &Path, names: &[&str]) -> std::io::Result<PathBuf> {
    let dir = run_dir.join("programs");
    std::fs::create_dir_all(&dir)?;
    super::facts::write_whole(&dir.join(MARKER), b"")?;
    for name in names {
        write_program(&dir, exe, name)?;
    }
    Ok(dir)
}

#[cfg(unix)]
fn write_program(dir: &Path, exe: &Path, name: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let quote = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
    let exe = exe
        .to_str()
        .ok_or_else(|| std::io::Error::other("hushrun's path isn't UTF-8"))?;
    let script = format!(
        "#!/bin/sh\nexec {} {SUBCOMMAND} {} \"$@\"\n",
        quote(exe),
        quote(name)
    );
    let path = dir.join(name);
    super::facts::write_whole(&path, script.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn write_program(dir: &Path, exe: &Path, name: &str) -> std::io::Result<()> {
    let path = dir.join(format!("{name}.exe"));
    let _ = std::fs::remove_file(&path);
    std::fs::hard_link(exe, &path).or_else(|_| std::fs::copy(exe, &path).map(|_| ()))
}

/// The variables COMMAND gets for the programs in `dir`: `PATH` with `dir` first and the rest as
/// `path`, COMMAND's own, had it; [`DIR_VAR`]; and [`PATH_WAS`], from which the hidden mode gives
/// the real program `PATH` back. `Err` when `dir` can't be on `PATH`, as with a `:` in it.
pub fn variables(dir: &Path, path: Option<OsString>) -> Result<Vec<(String, OsString)>, String> {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(path) = &path {
        parts.extend(std::env::split_paths(path));
    }
    let joined =
        std::env::join_paths(parts).map_err(|_| format!("{} can't be on PATH", dir.display()))?;
    let mut was = OsString::new();
    if let Some(path) = path {
        was.push("=");
        was.push(path);
    }
    Ok(vec![
        ("PATH".to_string(), joined),
        (DIR_VAR.to_string(), dir.as_os_str().to_os_string()),
        (PATH_WAS.to_string(), was),
    ])
}

/// `PATH` as COMMAND's environment had it, from [`PATH_WAS`]: `Some(None)` when it had none,
/// `None` when the variable isn't there.
pub(super) fn path_was(value: Option<OsString>) -> Option<Option<OsString>> {
    let value = value?;
    if value.is_empty() {
        return Some(None);
    }
    let text = value.to_string_lossy();
    match text.strip_prefix('=') {
        Some(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
                Some(Some(OsString::from_vec(value.as_bytes()[1..].to_vec())))
            }
            #[cfg(not(unix))]
            {
                Some(Some(OsString::from(text[1..].to_string())))
            }
        }
        None => Some(None),
    }
}

/// The real program named `name`: the first file of that name, executable, in a directory of
/// `path` that isn't a directory of programs (it holds no [`MARKER`]), and that isn't a link into
/// one; on Windows with one of `PATHEXT`'s extensions, `.exe` and `.com` without it. As a shell
/// reads `PATH`, an empty entry is the current directory and a relative one is from it: the real
/// program is the one COMMAND would have started.
pub fn real(name: &str, path: &OsStr) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    real_in(name, path, &cwd)
}

/// [`real`], from the current directory `cwd`.
fn real_in(name: &str, path: &OsStr, cwd: &Path) -> Option<PathBuf> {
    let programs = |dir: &Path| dir.join(MARKER).exists();
    for dir in std::env::split_paths(path) {
        let dir = if dir.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else {
            cwd.join(dir)
        };
        if programs(&dir) {
            continue;
        }
        for candidate in candidates(&dir, name) {
            // A link to one of hushrun's programs would start hushrun again, for ever.
            let into_programs = std::fs::canonicalize(&candidate)
                .ok()
                .and_then(|real| real.parent().map(programs))
                .unwrap_or(false);
            if runnable(&candidate) && !into_programs {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

#[cfg(not(unix))]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE".to_string());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|ext| dir.join(format!("{name}{}", ext.to_ascii_lowercase())))
        .collect()
}

#[cfg(unix)]
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// Runs as a program on `PATH` when the process says so: on Unix its arguments start with
/// [`SUBCOMMAND`] and the name; on Windows its own file is `<name>.exe` in the directory
/// [`DIR_VAR`] names. `None` for any other start, which hushrun parses as usual.
pub fn from_args(args: &[OsString]) -> Option<ExitCode> {
    if args.get(1).map(OsString::as_os_str) == Some(FORWARD.as_ref()) {
        forward(std::io::stdin().lock());
        return Some(ExitCode::SUCCESS);
    }
    if args.get(1).map(OsString::as_os_str) == Some(SUBCOMMAND.as_ref()) {
        let Some(name) = args.get(2).and_then(|n| n.to_str()) else {
            eprintln!("error: {SUBCOMMAND} is hushrun's own program on PATH, started by a script");
            return Some(ExitCode::from(2));
        };
        return Some(run(name, &args[3..]));
    }
    let dir = std::env::var_os(DIR_VAR)?;
    let exe = std::env::current_exe().ok()?;
    let same = |a: &Path, b: &Path| {
        std::fs::canonicalize(a)
            .ok()
            .is_some_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
    };
    if !exe
        .parent()
        .is_some_and(|parent| same(parent, Path::new(&dir)))
    {
        return None;
    }
    let name = exe.file_stem()?.to_str()?.to_string();
    Some(run(&name, args.get(1..).unwrap_or_default()))
}

/// One start of the program `name` with `args`, from the process's environment.
fn run(name: &str, args: &[OsString]) -> ExitCode {
    let program = PROGRAMS.iter().copied().find(|p| p.name() == name);
    let was = path_was(std::env::var_os(PATH_WAS));
    let units = std::env::var_os(super::runner::UNITS_VAR).map(PathBuf::from);
    run_with(name, args, program, was, units.as_deref())
}

/// [`run`], with what it reads of the environment given: `was` [`PATH_WAS`] read (`None` when it
/// isn't set, and then `PATH` now is searched), `units` the run's directory.
fn run_with(
    name: &str,
    args: &[OsString],
    program: Option<&dyn Program>,
    was: Option<Option<OsString>>,
    units: Option<&Path>,
) -> ExitCode {
    // With no PATH at all nothing is searched (an empty PATH is the current directory).
    let search = match &was {
        Some(Some(path)) => Some(path.clone()),
        Some(None) => None,
        None => std::env::var_os("PATH"),
    };
    let Some(real) = search.and_then(|search| real(name, &search)) else {
        eprintln!("error: {name}: command not found on PATH");
        return ExitCode::from(127);
    };
    let call = Call {
        name,
        args,
        real: &real,
        units,
    };
    // \[ADR 0086\] A program that runs the real one itself.
    if let (Some(program), Some(_)) = (program, units)
        && let Some(code) = program.run(&call)
    {
        return code;
    }
    // A run with no directory of its own changes nothing: hushrun knows of no plan.
    let start: Vec<OsString> = match program {
        Some(program) if units.is_some() => program.args(&call),
        _ => args.to_vec(),
    };
    let mut command = Command::new(&real);
    command.args(&start);
    // What the program would have seen without hushrun: `PATH` as it was, and no variable of
    // this module's.
    match &was {
        Some(Some(path)) => {
            command.env("PATH", path);
        }
        Some(None) => {
            command.env_remove("PATH");
        }
        None => {}
    }
    command.env_remove(DIR_VAR).env_remove(PATH_WAS);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("error: can't start {}: {err}", real.display());
            return ExitCode::from(if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    let forwarding = super::runner::forward_signals(child.id());
    let status = loop {
        match child.wait() {
            Ok(status) => break Some(status),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break None,
        }
    };
    drop(forwarding);
    let (exit, signal) = match &status {
        Some(status) => (status.code(), super::runner::signal_of(status)),
        None => (None, None),
    };
    if let (Some(program), Some(_)) = (program, units) {
        program.ended(&call, &start, exit);
    }
    super::runner::end_with(exit, signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake tool on a directory of `PATH`: it prints its arguments, one a line, and `PATH` and
    /// whether hushrun's variables reached it, into `out`, and exits 3.
    #[cfg(unix)]
    fn fake_tool(dir: &Path, name: &str, out: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done > '{out}'\necho \"path:$PATH\" >> '{out}'\necho \"dir:${{{DIR_VAR}-unset}}\" >> '{out}'\nexit 3\n",
            out = out.display()
        );
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    struct AddsSkip;

    #[cfg(unix)]
    impl Program for AddsSkip {
        fn name(&self) -> &'static str {
            "faketool"
        }
        fn args(&self, call: &Call<'_>) -> Vec<OsString> {
            assert_eq!(call.name, "faketool");
            assert!(
                call.real.ends_with("real/faketool"),
                "{}",
                call.real.display()
            );
            assert!(call.units.is_some());
            let mut args = call.args.to_vec();
            args.push("--skip".into());
            args.push("Cart/testTotal".into());
            args
        }
    }

    // covers: C-39
    #[cfg(unix)]
    #[test]
    fn a_program_on_path_finds_the_real_one_and_starts_it_with_changed_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let real_dir = tmp.path().join("real");
        fake_tool(&real_dir, "faketool", &out);
        let programs = make(
            &tmp.path().join("run"),
            Path::new("/opt/it's/hushrun"),
            &["faketool"],
        )
        .unwrap();
        // The program is a script that starts hushrun in its hidden mode, quoted.
        let script = std::fs::read_to_string(programs.join("faketool")).unwrap();
        assert_eq!(
            script,
            "#!/bin/sh\nexec '/opt/it'\\''s/hushrun' __hushrun-program 'faketool' \"$@\"\n"
        );
        // COMMAND's PATH: the programs first, then what it had.
        let had = std::env::join_paths([real_dir.clone(), PathBuf::from("/usr/bin")]).unwrap();
        let vars = variables(&programs, Some(had.clone())).unwrap();
        let path = &vars[0].1;
        assert_eq!(
            std::env::split_paths(path).next().unwrap(),
            programs,
            "the programs come first"
        );
        // The programs' own directory is never the real one, even first on the PATH searched.
        assert_eq!(real("faketool", path), Some(real_dir.join("faketool")));
        assert_eq!(real("faketool", OsStr::new("")), None);
        let was = path_was(Some(vars[2].1.clone()));
        assert_eq!(was, Some(Some(had.clone())));
        let units = tmp.path().join("run");
        let code = run_with(
            "faketool",
            &["test".into(), "--parallel".into()],
            Some(&AddsSkip),
            was.clone(),
            Some(&units),
        );
        assert_eq!(code, ExitCode::from(3));
        let said = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            said,
            format!(
                "arg:test\narg:--parallel\narg:--skip\narg:Cart/testTotal\npath:{}\ndir:unset\n",
                had.to_string_lossy()
            )
        );
        // With no run's directory, or no program for the name, the arguments pass unchanged.
        run_with(
            "faketool",
            &["test".into()],
            Some(&AddsSkip),
            was.clone(),
            None,
        );
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .starts_with("arg:test\npath:")
        );
        run_with("faketool", &["list".into()], None, was, Some(&units));
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .starts_with("arg:list\npath:")
        );
        // A program no PATH holds: 127, as a shell says.
        let gone = run_with("nothere", &[], None, Some(Some(had)), Some(&units));
        assert_eq!(gone, ExitCode::from(127));
    }

    // covers: C-39
    #[cfg(unix)]
    #[test]
    fn the_real_program_is_the_one_a_shell_would_start() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let out = root.join("out.txt");
        // An empty entry of PATH is the current directory, and a relative one is from it.
        fake_tool(&root.join("here"), "faketool", &out);
        fake_tool(&root.join("here/rel"), "faketool", &out);
        let here = root.join("here");
        assert_eq!(
            real_in("faketool", OsStr::new(":/nowhere"), &here),
            Some(here.join("faketool"))
        );
        assert_eq!(
            real_in("faketool", OsStr::new("/nowhere::/usr/bin"), &here),
            Some(here.join("faketool"))
        );
        assert_eq!(
            real_in("faketool", OsStr::new("rel"), &here),
            Some(here.join("rel/faketool"))
        );
        // hushrun's own directory of programs, however PATH names it, is never the real one;
        // nor a program that is a link into it.
        let programs = make(&root.join("run"), Path::new("/opt/hushrun"), &["faketool"]).unwrap();
        assert_eq!(real_in("faketool", OsStr::new(""), &programs), None);
        assert_eq!(real_in("faketool", OsStr::new("."), &programs), None);
        let linked = root.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(programs.join("faketool"), linked.join("faketool")).unwrap();
        let path = std::env::join_paths([linked.clone(), root.join("here/rel")]).unwrap();
        assert_eq!(
            real_in("faketool", &path, &root),
            Some(root.join("here/rel/faketool"))
        );
    }

    // covers: C-48
    #[cfg(unix)]
    #[test]
    fn a_program_that_keeps_output_open_past_its_end_is_waited_for_a_while_only() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        // The program exits, and leaves one it started holding its standard output.
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo before; sleep 40 & echo $! > '{}'; exit 3",
            pid.display()
        ));
        let started = std::time::Instant::now();
        let copied = run_copying_stdout(command, |_| true).unwrap();
        let took = started.elapsed();
        if let Ok(text) = std::fs::read_to_string(&pid) {
            let _ = Command::new("kill").arg(text.trim()).status();
        }
        assert_eq!(copied.exit, Some(3));
        // What came before it ended is passed on and kept; the rest is a forwarder's to pass on,
        // and hushrun doesn't wait for it.
        let text = String::from_utf8_lossy(&copied.stdout).into_owned();
        assert!(text.contains("before"), "{text}");
        assert!(took < MOST_AFTER * 2, "{took:?}");
        assert!(copied.held);
        // A program whose output ends with it isn't held.
        let mut quick = Command::new("sh");
        quick.arg("-c").arg("echo done");
        assert!(!run_copying_stdout(quick, |_| true).unwrap().held);
    }

    // covers: C-39
    #[cfg(windows)]
    #[test]
    fn a_program_on_path_ends_with_a_windows_exit_code_whole() {
        let mut command = Command::new("cmd");
        command.args(["/C", "exit 300"]);
        let copied = run_copying_stdout(command, |_| true).unwrap();
        assert_eq!(copied.exit, Some(300));
        assert_eq!(
            super::super::runner::ending(copied.exit, copied.signal, cfg!(windows)),
            super::super::runner::Ending::Whole(300)
        );
    }

    // covers: C-48
    #[cfg(unix)]
    #[test]
    fn output_that_never_stops_is_handed_over_and_kept_only_so_far() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        // The program exits, and leaves one printing as fast as it can.
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "yes '{}' & echo $! > '{}'; exit 4",
            "a long line printed over and over ".repeat(30),
            pid.display()
        ));
        // Whatever happens, `yes` stops after 10 s.
        let watched = pid.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            if let Ok(text) = std::fs::read_to_string(&watched) {
                let _ = Command::new("kill").arg(text.trim()).status();
            }
        });
        let started = std::time::Instant::now();
        let copied = run_copying_stdout(command, |_| true).unwrap();
        let took = started.elapsed();
        if let Ok(text) = std::fs::read_to_string(&pid) {
            let _ = Command::new("kill").arg(text.trim()).status();
        }
        assert_eq!(copied.exit, Some(4));
        assert!(took < MOST_AFTER * 4, "{took:?}");
        assert!(copied.held);
        assert!(copied.stdout.len() <= KEEP_MOST, "{}", copied.stdout.len());
        assert!(copied.cut);
    }

    // covers: C-39
    #[test]
    fn path_as_it_was_comes_back_whole() {
        assert_eq!(path_was(None), None);
        assert_eq!(path_was(Some(OsString::new())), Some(None));
        assert_eq!(
            path_was(Some(OsString::from("=/a:/b"))),
            Some(Some(OsString::from("/a:/b")))
        );
        assert_eq!(
            path_was(Some(OsString::from("="))),
            Some(Some(OsString::new()))
        );
        let vars = variables(Path::new("/run/programs"), None).unwrap();
        assert_eq!(vars[0].1, OsString::from("/run/programs"));
        assert_eq!(vars[2].1, OsString::new());
    }
}

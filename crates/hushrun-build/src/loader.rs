//! Whether a program that came from another machine can start on this one (B-38).
//!
//! A downloaded build-script binary is run by cargo, not by rustc: one that can't start fails
//! the build with "failed to run custom build command", no rule that watches rustc's errors
//! sees it (B-15), and cargo takes the binary for up to date, so every later build fails the
//! same way until the target folder is cleaned. A program that can't start is a test that
//! fails for no reason in the code. With a link environment that holds everything (B-37) this
//! can't happen; the check is for what it missed.
//!
//! **Nothing is run**: not the program, not the loader it names, not a tool. The file's bytes
//! came from another machine, so every path in them is data: [`can_start`] reads the file's
//! headers and this machine's files, and decides by the file's own format.
//!
//! - **ELF** (where this system runs ELF): the class, byte order and CPU are this machine's;
//!   the program-header and section-header tables and every segment and section lie inside the
//!   file; the loader (`PT_INTERP`), when there is one, is an absolute path in one of the
//!   system's own folders (as written and as it resolves), a regular file that can be run,
//!   never a file of the workspace, the store or any other place; each `DT_NEEDED` library is
//!   a regular ELF file for this CPU in the program's own run paths (`DT_RPATH` when there is
//!   no `DT_RUNPATH`, else `DT_RUNPATH`, `$ORIGIN` being the folder the program will be in) or
//!   in the folders the system's loader searches (`/etc/ld.so.conf`, then `/lib64`,
//!   `/usr/lib64`, `/lib`, `/usr/lib`); and each symbol version the program requires of a
//!   library (`DT_VERNEED`, the weak ones excepted) is one that library defines (`DT_VERDEF`).
//!   A static program has nothing to find.
//! - **Mach-O** (where this system runs Mach-O): the file, or its slice of a universal file,
//!   is for this machine's CPU; its load commands, every segment and each table in
//!   `__LINKEDIT` lie inside the file; its dynamic loader is `/usr/lib/dyld`; each library a
//!   load command names (the weak ones excepted) is a Mach-O file for this CPU where dyld
//!   looks (`@rpath` through each `LC_RPATH`, `@loader_path`, `@executable_path`, all from the
//!   folder the program will be in), or an image of this system's dyld shared cache, whose
//!   list of paths is read from the cache file. Where no cache can be read, a library under
//!   `/usr/lib/` or `/System/` counts when the SDK has its stub (`.tbd`).
//! - A file of the other format, or of neither, is not a program this system starts.
//!
//! `LD_LIBRARY_PATH` and the `DYLD_` variables are not consulted: what cargo will set when it
//! runs the program is not known here, and a miss costs one compile.

use std::fs::File;
use std::path::{Component, Path, PathBuf};

/// Where the program will run, and what the caller knows of this machine.
pub struct Here<'a> {
    /// The folder the program is put in before it runs: `$ORIGIN`, `@loader_path` and
    /// `@executable_path`.
    pub folder: &'a Path,
    /// The macOS SDK, when one is known: consulted only where no shared cache can be read.
    pub sdk: Option<&'a Path>,
}

/// `Ok` when `program` can start here as far as its headers and this machine's files tell,
/// with nothing run; else why not, in words for the run's log.
pub fn can_start(program: &Path, here: &Here<'_>) -> Result<(), String> {
    check(program, here, &System::host())
}

/// The format of the programs a system starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Elf,
    MachO,
    /// Neither (Windows): no linked output is stored there, and nothing is checked.
    Other,
}

/// What a machine starts and where it looks: this machine's, or a test's.
struct System {
    format: Format,
    /// ELF's `e_machine` for this CPU.
    elf_machine: Option<u16>,
    /// Whether this machine's ELF class is 64-bit.
    wide: bool,
    /// Mach-O's `cputype` for this CPU.
    macho_cpu: Option<u32>,
    /// The folders a program's loader may lie in.
    loader_dirs: Vec<PathBuf>,
    /// The folders the system's loader searches after a program's own run paths.
    library_dirs: Vec<PathBuf>,
    /// The dyld shared caches this machine may map.
    caches: Vec<PathBuf>,
}

/// Mach-O's `cputype` for arm64 and x86-64.
const CPU_ARM64: u32 = 0x0100_000c;
const CPU_X86_64: u32 = 0x0100_0007;

impl System {
    fn host() -> System {
        let format = if cfg!(target_vendor = "apple") {
            Format::MachO
        } else if cfg!(unix) {
            Format::Elf
        } else {
            Format::Other
        };
        let elf_machine = if cfg!(target_arch = "x86_64") {
            Some(62)
        } else if cfg!(target_arch = "aarch64") {
            Some(183)
        } else if cfg!(target_arch = "x86") {
            Some(3)
        } else if cfg!(target_arch = "arm") {
            Some(40)
        } else if cfg!(target_arch = "riscv64") {
            Some(243)
        } else {
            None
        };
        let macho_cpu = if cfg!(target_arch = "aarch64") {
            Some(CPU_ARM64)
        } else if cfg!(target_arch = "x86_64") {
            Some(CPU_X86_64)
        } else {
            None
        };
        let cache_names: &[&str] = if cfg!(target_arch = "aarch64") {
            &["dyld_shared_cache_arm64e"]
        } else {
            &["dyld_shared_cache_x86_64h", "dyld_shared_cache_x86_64"]
        };
        let caches = [
            "/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld",
            "/System/Library/dyld",
        ]
        .iter()
        .flat_map(|dir| {
            cache_names
                .iter()
                .map(move |name| Path::new(dir).join(name))
        })
        .collect();
        System {
            format,
            elf_machine,
            wide: cfg!(target_pointer_width = "64"),
            macho_cpu,
            loader_dirs: [
                "/lib",
                "/lib64",
                "/usr/lib",
                "/usr/lib64",
                "/libexec",
                "/usr/libexec",
                "/system/bin",
                "/apex",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
            library_dirs: if format == Format::Elf {
                crate::linkenv::system_library_dirs()
            } else {
                Vec::new()
            },
            caches,
        }
    }
}

fn check(program: &Path, here: &Here<'_>, system: &System) -> Result<(), String> {
    if system.format == Format::Other {
        return Ok(());
    }
    let image = Image::open(program).ok_or("it can't be read")?;
    let mut magic = [0u8; 4];
    image
        .read(0, &mut magic)
        .ok_or("it is too short to be a program")?;
    match (system.format, magic) {
        (Format::Elf, [0x7f, b'E', b'L', b'F']) => elf(&image, here, system),
        (Format::MachO, [0xcf, 0xfa, 0xed, 0xfe]) => macho(&image, here, system),
        (Format::MachO, [0xca, 0xfe, 0xba, 0xbe | 0xbf]) => {
            let slice = slice_for(&image, system, magic[3] == 0xbf)?
                .ok_or("it has no part for this machine's CPU")?;
            macho(&slice, here, system)
        }
        _ => Err("it is not a program this system starts".into()),
    }
}

/// A file, or one slice of it, read at offsets: every read is checked against its length.
struct Image {
    file: File,
    /// Where the image starts in the file, and how long it is.
    base: u64,
    len: u64,
}

/// The most one table may take in memory: headers and string tables, never a whole program.
const MOST_TABLE: u64 = 64 << 20;

impl Image {
    fn open(path: &Path) -> Option<Image> {
        let file = File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        meta.is_file().then(|| Image {
            file,
            base: 0,
            len: meta.len(),
        })
    }

    /// Whether `len` bytes at `offset` lie inside the image.
    fn holds(&self, offset: u64, len: u64) -> bool {
        offset.checked_add(len).is_some_and(|end| end <= self.len)
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Option<()> {
        if !self.holds(offset, buf.len() as u64) {
            return None;
        }
        let at = self.base.checked_add(offset)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_exact_at(buf, at).ok()
        }
        #[cfg(not(unix))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = self.file.try_clone().ok()?;
            file.seek(SeekFrom::Start(at)).ok()?;
            file.read_exact(buf).ok()
        }
    }

    /// `len` bytes at `offset`; `what` names them when they are not all there.
    fn bytes(&self, offset: u64, len: u64, what: &str) -> Result<Vec<u8>, String> {
        if len > MOST_TABLE {
            return Err(format!("its {what} is too large to read"));
        }
        let mut buf = vec![0u8; usize::try_from(len).map_err(|_| "it is too large")?];
        self.read(offset, &mut buf)
            .ok_or_else(|| format!("the file ends before its {what} does"))?;
        Ok(buf)
    }
}

/// Numbers in a file's byte order.
#[derive(Clone, Copy)]
struct Order {
    big: bool,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> Option<u64> {
        let x: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(u64::from(if self.big {
            u16::from_be_bytes(x)
        } else {
            u16::from_le_bytes(x)
        }))
    }
    fn u32(self, b: &[u8], at: usize) -> Option<u64> {
        let x: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(u64::from(if self.big {
            u32::from_be_bytes(x)
        } else {
            u32::from_le_bytes(x)
        }))
    }
    fn u64(self, b: &[u8], at: usize) -> Option<u64> {
        let x: [u8; 8] = b.get(at..at.checked_add(8)?)?.try_into().ok()?;
        Some(if self.big {
            u64::from_be_bytes(x)
        } else {
            u64::from_le_bytes(x)
        })
    }
    /// A word of the file's class.
    fn word(self, b: &[u8], at: usize, wide: bool) -> Option<u64> {
        if wide {
            self.u64(b, at)
        } else {
            self.u32(b, at)
        }
    }
}

fn c_string(b: &[u8], at: usize) -> Option<String> {
    let rest = b.get(at..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

const MALFORMED: &str = "its headers can't be read";

/// What an ELF file says of itself.
#[derive(Debug, Default)]
struct Elf {
    interpreter: Option<String>,
    needed: Vec<String>,
    rpath: Option<String>,
    runpath: Option<String>,
    /// Each library a version is required of, with the versions that are not weak.
    requires: Vec<(String, Vec<String>)>,
    /// The versions it defines.
    defines: Vec<String>,
}

/// Reads an ELF file for this machine. `whole`: every segment and section must lie inside it.
fn read_elf(image: &Image, system: &System, whole: bool) -> Result<Elf, String> {
    let ident = image.bytes(0, 20, "header")?;
    let (wide, big) = (ident[4] == 2, ident[5] == 2);
    if wide != system.wide || big != cfg!(target_endian = "big") || !matches!(ident[4], 1 | 2) {
        return Err("it is built for another kind of machine".into());
    }
    let o = Order { big };
    let machine = o.u16(&ident, 18).ok_or(MALFORMED)?;
    if system.elf_machine.map(u64::from) != Some(machine) {
        return Err(format!(
            "it is built for another CPU (ELF machine {machine})"
        ));
    }
    let head = image.bytes(0, if wide { 64 } else { 52 }, "header")?;
    let field = |at_wide: usize, at_narrow: usize, word: bool| -> Result<u64, String> {
        let at = if wide { at_wide } else { at_narrow };
        match word {
            true => o.word(&head, at, wide),
            false => o.u16(&head, at),
        }
        .ok_or_else(|| MALFORMED.to_owned())
    };
    let (phoff, shoff) = (field(0x20, 0x1c, true)?, field(0x28, 0x20, true)?);
    let (phentsize, phnum) = (field(0x36, 0x2a, false)?, field(0x38, 0x2c, false)?);
    let (shentsize, shnum) = (field(0x3a, 0x2e, false)?, field(0x3c, 0x30, false)?);
    if phnum > 0 && phentsize < if wide { 56 } else { 32 } {
        return Err(MALFORMED.into());
    }

    // The segments: each inside the file; the loader's name, the dynamic table, and where
    // each address lies in the file.
    let table = image.bytes(phoff, phentsize * phnum, "program headers")?;
    let mut loads: Vec<(u64, u64, u64)> = Vec::new();
    let mut dynamic = None;
    let mut out = Elf::default();
    for i in 0..phnum {
        let base = usize::try_from(i * phentsize).map_err(|_| MALFORMED)?;
        let kind = o.u32(&table, base).ok_or(MALFORMED)?;
        let (offset, address, size) = if wide {
            (
                o.u64(&table, base + 8),
                o.u64(&table, base + 16),
                o.u64(&table, base + 32),
            )
        } else {
            (
                o.u32(&table, base + 4),
                o.u32(&table, base + 8),
                o.u32(&table, base + 16),
            )
        };
        let (Some(offset), Some(address), Some(size)) = (offset, address, size) else {
            return Err(MALFORMED.into());
        };
        if kind == 0 {
            continue;
        }
        if !image.holds(offset, size) {
            if whole || matches!(kind, 2 | 3) {
                return Err("the file ends before one of its segments does".into());
            }
            continue;
        }
        match kind {
            1 => loads.push((address, offset, size)),
            2 => dynamic = Some((offset, size)),
            3 => {
                let name = image.bytes(offset, size, "loader's name")?;
                out.interpreter = Some(c_string(&name, 0).ok_or(MALFORMED)?);
            }
            _ => {}
        }
    }
    if whole && shoff != 0 {
        if shnum > 0 && shentsize < if wide { 64 } else { 40 } {
            return Err(MALFORMED.into());
        }
        let table = image.bytes(shoff, shentsize * shnum, "section headers")?;
        for i in 0..shnum {
            let base = usize::try_from(i * shentsize).map_err(|_| MALFORMED)?;
            let kind = o.u32(&table, base + 4).ok_or(MALFORMED)?;
            // An empty entry, or one that takes no bytes of the file (`.bss`).
            if matches!(kind, 0 | 8) {
                continue;
            }
            let (offset, size) = if wide {
                (o.u64(&table, base + 0x18), o.u64(&table, base + 0x20))
            } else {
                (o.u32(&table, base + 0x10), o.u32(&table, base + 0x14))
            };
            let (Some(offset), Some(size)) = (offset, size) else {
                return Err(MALFORMED.into());
            };
            if !image.holds(offset, size) {
                return Err("the file ends before one of its sections does".into());
            }
        }
    }
    let Some((offset, size)) = dynamic else {
        return Ok(out);
    };

    // The dynamic table: names are offsets into its string table, which it names by address.
    let table = image.bytes(offset, size, "dynamic table")?;
    let entry = if wide { 16 } else { 8 };
    let mut names: Vec<(u64, u64)> = Vec::new();
    let mut found = std::collections::BTreeMap::new();
    for at in (0..table.len().saturating_sub(entry - 1)).step_by(entry) {
        let tag = o.word(&table, at, wide).ok_or(MALFORMED)?;
        let value = o.word(&table, at + entry / 2, wide).ok_or(MALFORMED)?;
        match tag {
            0 => break,
            // DT_NEEDED, DT_RPATH, DT_RUNPATH.
            1 | 15 | 29 => names.push((tag, value)),
            // DT_STRTAB, DT_STRSZ, DT_VERDEF, DT_VERDEFNUM, DT_VERNEED, DT_VERNEEDNUM.
            5 | 10 | 0x6fff_fffc..=0x6fff_ffff => {
                found.insert(tag, value);
            }
            _ => {}
        }
    }
    let in_file = |address: u64| -> Result<u64, String> {
        loads
            .iter()
            .find(|(start, _, size)| address >= *start && address - start < *size)
            .map(|(start, offset, _)| offset + (address - start))
            .ok_or_else(|| MALFORMED.to_owned())
    };
    let strings = match (found.get(&5), found.get(&10)) {
        (Some(address), Some(size)) => image.bytes(in_file(*address)?, *size, "string table")?,
        _ if names.is_empty() => Vec::new(),
        _ => return Err(MALFORMED.into()),
    };
    let name = |at: u64| -> Result<String, String> {
        usize::try_from(at)
            .ok()
            .and_then(|at| c_string(&strings, at))
            .ok_or_else(|| MALFORMED.to_owned())
    };
    for (tag, value) in names {
        let text = name(value)?;
        match tag {
            1 => out.needed.push(text),
            15 => out.rpath = Some(text),
            _ => out.runpath = Some(text),
        }
    }
    // Versions required (`Elf_Verneed`, each with its `Elf_Vernaux` list) and defined
    // (`Elf_Verdef`, whose first `Elf_Verdaux` is its name). The two layouts are the same in
    // both classes.
    if let (Some(address), Some(count)) = (found.get(&0x6fff_fffe), found.get(&0x6fff_ffff)) {
        let mut at = in_file(*address)?;
        for _ in 0..(*count).min(4096) {
            let need = image.bytes(at, 16, "version table")?;
            let get = |b: &[u8], at: usize| o.u32(b, at).ok_or(MALFORMED);
            let aux_count = o.u16(&need, 2).ok_or(MALFORMED)?;
            let file = name(get(&need, 4)?)?;
            let mut aux_at = at + get(&need, 8)?;
            let mut versions = Vec::new();
            for _ in 0..aux_count {
                let aux = image.bytes(aux_at, 16, "version table")?;
                let weak = o.u16(&aux, 4).ok_or(MALFORMED)? & 2 != 0;
                if !weak {
                    versions.push(name(get(&aux, 8)?)?);
                }
                let next = get(&aux, 12)?;
                if next == 0 {
                    break;
                }
                aux_at += next;
            }
            out.requires.push((file, versions));
            let next = get(&need, 12)?;
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    if let (Some(address), Some(count)) = (found.get(&0x6fff_fffc), found.get(&0x6fff_fffd)) {
        let mut at = in_file(*address)?;
        for _ in 0..(*count).min(65536) {
            let def = image.bytes(at, 20, "version table")?;
            let get = |b: &[u8], at: usize| o.u32(b, at).ok_or(MALFORMED);
            let aux = image.bytes(at + get(&def, 12)?, 8, "version table")?;
            out.defines.push(name(get(&aux, 0)?)?);
            let next = get(&def, 16)?;
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    Ok(out)
}

/// `path` without `.` parts, when it is absolute and has no `..` part.
fn plain_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir | Component::Prefix(_) => return None,
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

/// Whether `loader` is one of the system's own: an absolute path in a system folder, as it is
/// written and as it resolves, that is a regular file with a run bit. Read, never run.
fn system_loader(loader: &str, system: &System) -> Result<(), String> {
    let no = |why: &str| Err(format!("its loader, {loader}, {why}"));
    let Some(written) = plain_absolute(Path::new(loader)) else {
        return no("is not a path from the root");
    };
    let Ok(resolved) = std::fs::canonicalize(&written) else {
        return no("is not on this machine");
    };
    // A system folder may itself be a link (`/lib` to `/usr/lib`): both forms count.
    let system_dirs: Vec<PathBuf> = system
        .loader_dirs
        .iter()
        .flat_map(|dir| [Some(dir.clone()), std::fs::canonicalize(dir).ok()])
        .flatten()
        .collect();
    let in_system = |path: &Path| system_dirs.iter().any(|dir| path.starts_with(dir));
    if !in_system(&written) || !in_system(&resolved) {
        return no("is not in one of the system's folders");
    }
    let runs = std::fs::metadata(&resolved).is_ok_and(|meta| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    });
    if !runs {
        return no("is not a program");
    }
    Ok(())
}

/// The folders of a run path, `$ORIGIN` being `origin`; one with any other `$` word (`$LIB`,
/// `$PLATFORM`) is left out, since only the loader knows what it stands for.
fn run_path(list: &str, origin: &Path) -> Vec<PathBuf> {
    let origin = origin.to_string_lossy();
    list.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| {
            dir.replace("${ORIGIN}", &origin)
                .replace("$ORIGIN", &origin)
        })
        .filter(|dir| !dir.contains('$'))
        .map(PathBuf::from)
        .collect()
}

fn elf(image: &Image, here: &Here<'_>, system: &System) -> Result<(), String> {
    let program = read_elf(image, system, true)?;
    if let Some(loader) = &program.interpreter {
        system_loader(loader, system)?;
    }
    let mut dirs = Vec::new();
    match (&program.runpath, &program.rpath) {
        (Some(runpath), _) => dirs.extend(run_path(runpath, here.folder)),
        (None, Some(rpath)) => dirs.extend(run_path(rpath, here.folder)),
        (None, None) => {}
    }
    dirs.extend(system.library_dirs.iter().cloned());
    for needed in &program.needed {
        let candidates: Vec<PathBuf> = if needed.contains('/') {
            // The loader takes a name with a `/` as a path, from the working directory when it
            // is relative, which nothing here knows.
            plain_absolute(Path::new(needed)).into_iter().collect()
        } else {
            dirs.iter()
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(needed))
                .collect()
        };
        // As the loader does: the first file of that name that is a library for this machine.
        let library = candidates.iter().find_map(|path| {
            let image = Image::open(path)?;
            read_elf(&image, system, false).ok()
        });
        let Some(library) = library else {
            return Err(format!("{needed}, which it loads, is not on this machine"));
        };
        for (file, versions) in &program.requires {
            if file != needed {
                continue;
            }
            if let Some(missing) = versions.iter().find(|v| !library.defines.contains(v)) {
                return Err(format!(
                    "{needed} on this machine has no version {missing}, which it needs"
                ));
            }
        }
    }
    Ok(())
}

/// The slice of a universal file for this machine's CPU, when it has one.
fn slice_for(image: &Image, system: &System, wide: bool) -> Result<Option<Image>, String> {
    let o = Order { big: true };
    let head = image.bytes(0, 8, "header")?;
    let count = o.u32(&head, 4).ok_or(MALFORMED)?;
    if count > 64 {
        return Err(MALFORMED.into());
    }
    let size = if wide { 32 } else { 20 };
    let table = image.bytes(8, count * size, "list of parts")?;
    for i in 0..count {
        let base = usize::try_from(i * size).map_err(|_| MALFORMED)?;
        let cpu = o.u32(&table, base).ok_or(MALFORMED)?;
        let (offset, len) = if wide {
            (o.u64(&table, base + 8), o.u64(&table, base + 16))
        } else {
            (o.u32(&table, base + 8), o.u32(&table, base + 12))
        };
        let (Some(offset), Some(len)) = (offset, len) else {
            return Err(MALFORMED.into());
        };
        if system.macho_cpu.map(u64::from) != Some(cpu) {
            continue;
        }
        if !image.holds(offset, len) {
            return Err("the file ends before its part for this machine does".into());
        }
        return Ok(Some(Image {
            file: image.file.try_clone().map_err(|_| "it can't be read")?,
            base: image.base + offset,
            len,
        }));
    }
    Ok(None)
}

/// What a Mach-O image says of itself.
#[derive(Debug, Default)]
struct MachO {
    /// Each library a load command names, and whether it may be absent (`LC_LOAD_WEAK_DYLIB`).
    dylibs: Vec<(String, bool)>,
    rpaths: Vec<String>,
    dylinker: Option<String>,
}

/// Reads a thin Mach-O image for this machine's CPU. `whole`: every segment and table must lie
/// inside it.
fn read_macho(image: &Image, system: &System, whole: bool) -> Result<MachO, String> {
    let o = Order { big: false };
    let head = image.bytes(0, 32, "header")?;
    if head[..4] != [0xcf, 0xfa, 0xed, 0xfe] {
        return Err("it is not a 64-bit Mach-O file".into());
    }
    let cpu = o.u32(&head, 4).ok_or(MALFORMED)?;
    if system.macho_cpu.map(u64::from) != Some(cpu) {
        return Err(format!(
            "it is built for another CPU (Mach-O type {cpu:#x})"
        ));
    }
    let count = o.u32(&head, 16).ok_or(MALFORMED)?;
    let size = o.u32(&head, 20).ok_or(MALFORMED)?;
    let commands = image.bytes(32, size, "load commands")?;
    let mut out = MachO::default();
    let mut at = 0usize;
    for _ in 0..count {
        let cmd = o.u32(&commands, at).ok_or(MALFORMED)?;
        let cmdsize =
            usize::try_from(o.u32(&commands, at + 4).ok_or(MALFORMED)?).map_err(|_| MALFORMED)?;
        let body = at
            .checked_add(cmdsize)
            .filter(|_| cmdsize >= 8)
            .and_then(|end| commands.get(at..end))
            .ok_or(MALFORMED)?;
        let text = |body: &[u8]| -> Result<String, String> {
            let at = usize::try_from(o.u32(body, 8).ok_or(MALFORMED)?).map_err(|_| MALFORMED)?;
            c_string(body, at).ok_or_else(|| MALFORMED.to_owned())
        };
        // Where a command says bytes of the file lie: (offset, size).
        let mut inside: Vec<(Option<u64>, Option<u64>)> = Vec::new();
        match cmd {
            // LC_SEGMENT_64: fileoff, filesize.
            0x19 => inside.push((o.u64(body, 40), o.u64(body, 48))),
            // LC_SYMTAB: the symbols (16 bytes each) and their strings.
            0x2 => {
                let symbols = o.u32(body, 12).map(|n| n * 16);
                inside.push((o.u32(body, 8), symbols));
                inside.push((o.u32(body, 16), o.u32(body, 20)));
            }
            // LC_CODE_SIGNATURE, LC_FUNCTION_STARTS, LC_DATA_IN_CODE, LC_DYLD_EXPORTS_TRIE,
            // LC_DYLD_CHAINED_FIXUPS: dataoff, datasize.
            0x1d | 0x26 | 0x29 | 0x8000_0033 | 0x8000_0034 => {
                inside.push((o.u32(body, 8), o.u32(body, 12)));
            }
            // LC_LOAD_DYLIB, LC_REEXPORT_DYLIB, LC_LAZY_LOAD_DYLIB, LC_LOAD_UPWARD_DYLIB.
            0xc | 0x8000_001f | 0x20 | 0x8000_0023 => out.dylibs.push((text(body)?, false)),
            // LC_LOAD_WEAK_DYLIB.
            0x8000_0018 => out.dylibs.push((text(body)?, true)),
            // LC_RPATH.
            0x8000_001c => out.rpaths.push(text(body)?),
            // LC_LOAD_DYLINKER.
            0xe => out.dylinker = Some(text(body)?),
            _ => {}
        }
        for (offset, len) in inside {
            let (Some(offset), Some(len)) = (offset, len) else {
                return Err(MALFORMED.into());
            };
            if whole && !image.holds(offset, len) {
                return Err("the file ends before one of its segments does".into());
            }
        }
        at += cmdsize;
    }
    Ok(out)
}

/// The paths of the images in a dyld shared cache, read from the cache file's own list.
fn cache_images(cache: &Path) -> Option<Vec<String>> {
    let image = Image::open(cache)?;
    let o = Order { big: false };
    let head = image.bytes(0, 0x1c8, "header").ok()?;
    if !head.starts_with(b"dyld_v1") {
        return None;
    }
    // `imagesOffset` and `imagesCount`; a cache from before they moved has them at 0x18.
    let (mut offset, mut count) = (o.u32(&head, 0x1c0)?, o.u32(&head, 0x1c4)?);
    let mapping = o.u32(&head, 0x10)?;
    if mapping < 0x1c8 || offset == 0 {
        (offset, count) = (o.u32(&head, 0x18)?, o.u32(&head, 0x1c)?);
    }
    if count == 0 || count > 1 << 20 {
        return None;
    }
    // Each entry: address, date, inode (8 bytes each), then where its path lies, and padding.
    let infos = image.bytes(offset, count * 32, "list of images").ok()?;
    let mut places = Vec::with_capacity(usize::try_from(count).ok()?);
    for i in 0..usize::try_from(count).ok()? {
        places.push(o.u32(&infos, i * 32 + 24)?);
    }
    let (first, last) = (*places.iter().min()?, *places.iter().max()?);
    let len = (last - first + 1024).min(image.len.checked_sub(first)?);
    let text = image.bytes(first, len, "list of images").ok()?;
    places
        .iter()
        .map(|place| c_string(&text, usize::try_from(place - first).ok()?))
        .collect()
}

fn macho(image: &Image, here: &Here<'_>, system: &System) -> Result<(), String> {
    let program = read_macho(image, system, true)?;
    if let Some(dylinker) = &program.dylinker
        && dylinker != "/usr/lib/dyld"
    {
        return Err(format!(
            "its loader, {dylinker}, is not the system's (/usr/lib/dyld)"
        ));
    }
    let folder = here.folder.to_string_lossy();
    let expand = |text: &str| -> Option<PathBuf> {
        for prefix in ["@loader_path", "@executable_path"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                return Some(PathBuf::from(format!("{folder}{rest}")));
            }
        }
        // Any other `@` word is not one dyld gives a meaning here.
        (!text.starts_with('@')).then(|| PathBuf::from(text))
    };
    // Read once, and only when a library is not a file.
    let mut cached: Option<Option<Vec<String>>> = None;
    for (dylib, weak) in &program.dylibs {
        if *weak {
            continue;
        }
        let candidates: Vec<PathBuf> = match dylib.strip_prefix("@rpath/") {
            Some(rest) => program
                .rpaths
                .iter()
                .filter_map(|rpath| expand(rpath))
                .map(|dir| dir.join(rest))
                .collect(),
            None => expand(dylib).into_iter().collect(),
        };
        let on_disk = candidates.iter().any(|path| {
            path.is_absolute()
                && Image::open(path).is_some_and(|image| {
                    let mut magic = [0u8; 4];
                    if image.read(0, &mut magic).is_none() {
                        return false;
                    }
                    match magic {
                        [0xcf, 0xfa, 0xed, 0xfe] => read_macho(&image, system, false).is_ok(),
                        [0xca, 0xfe, 0xba, 0xbe | 0xbf] => {
                            matches!(slice_for(&image, system, magic[3] == 0xbf), Ok(Some(_)))
                        }
                        _ => false,
                    }
                })
        });
        if on_disk {
            continue;
        }
        // The system's own libraries are images of the shared cache, not files.
        let images = cached.get_or_insert_with(|| {
            let lists: Vec<Vec<String>> = system
                .caches
                .iter()
                .filter_map(|cache| cache_images(cache))
                .collect();
            (!lists.is_empty()).then(|| lists.concat())
        });
        let in_system = match images {
            Some(images) => images.iter().any(|image| image == dylib),
            // No cache can be read: what the SDK says this system has.
            None => {
                (dylib.starts_with("/usr/lib/") || dylib.starts_with("/System/"))
                    && plain_absolute(Path::new(dylib)).is_some()
                    && here.sdk.is_some_and(|sdk| {
                        let stub = Path::new(dylib).with_extension("tbd");
                        stub.strip_prefix("/")
                            .is_ok_and(|rest| sdk.join(rest).is_file())
                    })
            }
        };
        if !in_system {
            return Err(format!("{dylib}, which it loads, is not on this machine"));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;

    /// A 64-bit Mach-O program for `cpu` that loads `dylibs` (each with whether it is weak),
    /// with the run paths `rpaths`, a `__LINKEDIT` segment of 64 bytes at its end, and
    /// `dylinker` as its loader.
    pub(crate) fn macho_with(
        cpu: u32,
        dylibs: &[(&str, bool)],
        rpaths: &[&str],
        dylinker: &str,
    ) -> Vec<u8> {
        fn named(cmd: u32, header: usize, name: &str, more: &[u32]) -> Vec<u8> {
            let mut text = name.as_bytes().to_vec();
            text.push(0);
            while !(header + text.len()).is_multiple_of(8) {
                text.push(0);
            }
            let mut out = Vec::new();
            for word in [cmd, (header + text.len()) as u32, header as u32] {
                out.extend(word.to_le_bytes());
            }
            for word in more {
                out.extend(word.to_le_bytes());
            }
            out.extend(text);
            out
        }
        let mut commands = Vec::new();
        let mut count = 0u32;
        // LC_LOAD_DYLINKER.
        commands.extend(named(0xe, 12, dylinker, &[]));
        count += 1;
        for (name, weak) in dylibs {
            // cmd, cmdsize, the name's offset, a date, two versions, the name.
            let cmd = if *weak { 0x8000_0018 } else { 0xc };
            commands.extend(named(cmd, 24, name, &[0, 0, 0]));
            count += 1;
        }
        for rpath in rpaths {
            commands.extend(named(0x8000_001c, 12, rpath, &[]));
            count += 1;
        }
        // LC_SEGMENT_64 `__LINKEDIT`: 72 bytes, no sections; its bytes are the file's last 64.
        let segment_at = commands.len();
        let mut segment = Vec::new();
        segment.extend(0x19u32.to_le_bytes());
        segment.extend(72u32.to_le_bytes());
        segment.extend(*b"__LINKEDIT\0\0\0\0\0\0");
        segment.extend([0u8; 16]); // vmaddr, vmsize
        segment.extend([0u8; 16]); // fileoff, filesize: filled in below
        segment.extend([0u8; 16]); // maxprot, initprot, nsects, flags
        commands.extend(segment);
        count += 1;
        let data_at = (32 + commands.len()) as u64;
        commands[segment_at + 40..segment_at + 48].copy_from_slice(&data_at.to_le_bytes());
        commands[segment_at + 48..segment_at + 56].copy_from_slice(&64u64.to_le_bytes());
        let mut out = Vec::new();
        // magic, cputype, cpusubtype, filetype (MH_EXECUTE), ncmds, sizeofcmds, flags, reserved.
        for word in [
            0xfeed_facf_u32,
            cpu,
            0,
            2,
            count,
            commands.len() as u32,
            0,
            0,
        ] {
            out.extend(word.to_le_bytes());
        }
        out.extend(commands);
        out.extend([0u8; 64]);
        out
    }

    /// A Mach-O program for arm64 that loads `dylibs` and nothing more.
    pub(crate) fn macho(dylibs: &[&str]) -> Vec<u8> {
        let dylibs: Vec<(&str, bool)> = dylibs.iter().map(|name| (*name, false)).collect();
        macho_with(CPU_ARM64, &dylibs, &[], "/usr/lib/dyld")
    }

    /// What [`elf_with`] writes.
    #[derive(Default, Clone)]
    pub(crate) struct ElfSpec<'a> {
        pub machine: u16,
        pub loader: Option<&'a str>,
        pub needed: Vec<&'a str>,
        pub runpath: Option<&'a str>,
        /// A version each needed library must define.
        pub requires: Option<&'a str>,
        /// The versions this file defines.
        pub defines: Vec<&'a str>,
    }

    /// A 64-bit little-endian ELF file: one loaded segment that is the whole file, the loader's
    /// name, a dynamic table with its strings and version tables, and no sections.
    pub(crate) fn elf_with(spec: &ElfSpec<'_>) -> Vec<u8> {
        // The strings first, so every name has its offset.
        let mut strings = vec![0u8];
        let mut add = |text: &str| -> u64 {
            let at = strings.len() as u64;
            strings.extend(text.as_bytes());
            strings.push(0);
            at
        };
        let needed: Vec<u64> = spec.needed.iter().map(|name| add(name)).collect();
        let runpath = spec.runpath.map(&mut add);
        let requires = spec.requires.map(&mut add);
        let defines: Vec<u64> = spec.defines.iter().map(|name| add(name)).collect();
        let loader: Vec<u8> = spec
            .loader
            .map(|name| [name.as_bytes(), &[0]].concat())
            .unwrap_or_default();

        // Layout: header (64), three program headers (56 each), then each part in turn.
        let mut at = 64 + 3 * 56;
        let mut place = |len: usize| {
            let start = at as u64;
            at += len;
            start
        };
        let loader_at = place(loader.len());
        let strings_at = place(strings.len());
        // Verneed: one entry of 16 bytes and one aux of 16 per needed library.
        let need_at = place(match requires {
            Some(_) => needed.len() * 32,
            None => 0,
        });
        // Verdef: one entry of 20 bytes and one aux of 8 per version.
        let def_at = place(defines.len() * 28);
        let mut dynamic: Vec<(u64, u64)> = Vec::new();
        dynamic.extend(needed.iter().map(|name| (1, *name)));
        if let Some(runpath) = runpath {
            dynamic.push((29, runpath));
        }
        dynamic.push((5, strings_at));
        dynamic.push((10, strings.len() as u64));
        if requires.is_some() && !needed.is_empty() {
            dynamic.push((0x6fff_fffe, need_at));
            dynamic.push((0x6fff_ffff, needed.len() as u64));
        }
        if !defines.is_empty() {
            dynamic.push((0x6fff_fffc, def_at));
            dynamic.push((0x6fff_fffd, defines.len() as u64));
        }
        dynamic.push((0, 0));
        let dynamic_at = place(dynamic.len() * 16);
        let total = at as u64;

        let mut out = vec![0u8; 64];
        out[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        out[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        out[18..20].copy_from_slice(&spec.machine.to_le_bytes());
        out[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        out[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        out[0x38..0x3a].copy_from_slice(&3u16.to_le_bytes()); // e_phnum
        let mut header = |kind: u32, offset: u64, size: u64| {
            let mut ph = vec![0u8; 56];
            ph[..4].copy_from_slice(&kind.to_le_bytes());
            ph[8..16].copy_from_slice(&offset.to_le_bytes());
            ph[16..24].copy_from_slice(&offset.to_le_bytes()); // an address is its offset
            ph[32..40].copy_from_slice(&size.to_le_bytes());
            out.extend(ph);
        };
        header(1, 0, total);
        match spec.loader {
            Some(_) => header(3, loader_at, loader.len() as u64),
            None => header(0, 0, 0),
        }
        header(2, dynamic_at, (dynamic.len() * 16) as u64);
        out.extend(&loader);
        out.extend(&strings);
        if let Some(version) = requires {
            for (i, file) in needed.iter().enumerate() {
                let last = i + 1 == needed.len();
                // vn_version, vn_cnt, vn_file, vn_aux, vn_next.
                out.extend(1u16.to_le_bytes());
                out.extend(1u16.to_le_bytes());
                out.extend((*file as u32).to_le_bytes());
                out.extend(16u32.to_le_bytes());
                out.extend((if last { 0u32 } else { 32 }).to_le_bytes());
                // vna_hash, vna_flags, vna_other, vna_name, vna_next.
                out.extend(0u32.to_le_bytes());
                out.extend(0u16.to_le_bytes());
                out.extend(2u16.to_le_bytes());
                out.extend((version as u32).to_le_bytes());
                out.extend(0u32.to_le_bytes());
            }
        }
        for (i, name) in defines.iter().enumerate() {
            let last = i + 1 == defines.len();
            // vd_version, vd_flags, vd_ndx, vd_cnt, vd_hash, vd_aux, vd_next.
            out.extend(1u16.to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend((i as u16 + 1).to_le_bytes());
            out.extend(1u16.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            out.extend(20u32.to_le_bytes());
            out.extend((if last { 0u32 } else { 28 }).to_le_bytes());
            // vda_name, vda_next.
            out.extend((*name as u32).to_le_bytes());
            out.extend(0u32.to_le_bytes());
        }
        for (tag, value) in dynamic {
            out.extend(tag.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        assert_eq!(out.len() as u64, total);
        out
    }

    /// A program in this system's own format that starts here, or with `missing` one that
    /// loads a library of that name, which no machine has.
    pub(crate) fn host_program(missing: Option<&str>) -> Vec<u8> {
        let system = System::host();
        match system.format {
            Format::MachO => {
                let mut dylibs = vec![("/usr/lib/libSystem.B.dylib", false)];
                let path = missing.map(|name| format!("/opt/only-there/lib/{name}"));
                if let Some(path) = &path {
                    dylibs.push((path, false));
                }
                macho_with(system.macho_cpu.unwrap(), &dylibs, &[], "/usr/lib/dyld")
            }
            _ => elf_with(&ElfSpec {
                machine: system.elf_machine.unwrap(),
                needed: missing.into_iter().collect(),
                ..ElfSpec::default()
            }),
        }
    }

    const AARCH64: u16 = 183;
    const X86_64: u16 = 62;

    /// A Linux machine for arm64 whose system folders are `root`'s.
    fn linux(root: &Path) -> System {
        System {
            format: Format::Elf,
            elf_machine: Some(AARCH64),
            wide: true,
            macho_cpu: None,
            loader_dirs: vec![root.join("lib")],
            library_dirs: vec![root.join("lib")],
            caches: Vec::new(),
        }
    }

    /// A Mac for arm64 whose shared cache is the file `cache`.
    fn mac(cache: &Path) -> System {
        System {
            format: Format::MachO,
            elf_machine: None,
            wide: true,
            macho_cpu: Some(CPU_ARM64),
            loader_dirs: Vec::new(),
            library_dirs: Vec::new(),
            caches: vec![cache.to_path_buf()],
        }
    }

    /// A dyld shared cache file that lists `images`.
    fn cache(images: &[&str]) -> Vec<u8> {
        let mut out = vec![0u8; 0x1c8];
        out[..16].copy_from_slice(b"dyld_v1  arm64e\0");
        out[0x10..0x14].copy_from_slice(&0x1c8u32.to_le_bytes());
        out[0x1c0..0x1c4].copy_from_slice(&0x1c8u32.to_le_bytes());
        out[0x1c4..0x1c8].copy_from_slice(&(images.len() as u32).to_le_bytes());
        let mut at = 0x1c8 + images.len() * 32;
        let mut text = Vec::new();
        for image in images {
            let mut info = [0u8; 32];
            info[24..28].copy_from_slice(&(at as u32).to_le_bytes());
            out.extend(info);
            text.extend(image.as_bytes());
            text.push(0);
            at += image.len() + 1;
        }
        out.extend(text);
        out
    }

    fn executable(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct Fixture {
        dir: tempfile::TempDir,
        root: PathBuf,
        /// The folder the program will run from.
        folder: PathBuf,
        /// Where the downloaded program lies before it is put there.
        program: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let folder = root.join("ws/target/debug/build/app-1");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("store")).unwrap();
        Fixture {
            program: root.join("store/build-script-build"),
            folder,
            root,
            dir,
        }
    }

    impl Fixture {
        fn asked(&self, bytes: &[u8], system: &System) -> Result<(), String> {
            std::fs::write(&self.program, bytes).unwrap();
            let here = Here {
                folder: &self.folder,
                sdk: None,
            };
            check(&self.program, &here, system)
        }
    }

    // covers: B-38
    #[test]
    fn an_elf_programs_loader_is_one_of_the_systems_own_and_is_never_run() {
        let f = fixture();
        let system = linux(&f.root);
        let with = |loader: &Path| {
            let spec = ElfSpec {
                machine: AARCH64,
                loader: Some(loader.to_str().unwrap()),
                ..ElfSpec::default()
            };
            f.asked(&elf_with(&spec), &system)
        };
        // The system's own loader: a file in a system folder. It is `exit 1` here: were it
        // run, or asked anything, the answer would not be `Ok`.
        let loader = f.root.join("lib/ld-linux.so.1");
        executable(&loader, b"#!/bin/sh\nexit 1\n");
        assert_eq!(with(&loader), Ok(()));
        // What a downloaded file may name instead: a script beside it in the store, or in the
        // workspace. Each leaves a mark when it runs.
        let mark = f.root.join("ran");
        let script = format!("#!/bin/sh\necho ran > '{}'\n", mark.display());
        for place in [
            "store/payload",
            "ws/payload",
            "ws/target/debug/build/app-1/ld.so",
        ] {
            let payload = f.root.join(place);
            executable(&payload, script.as_bytes());
            let why = with(&payload).unwrap_err();
            assert!(why.contains("system's folders"), "{why}");
        }
        // A path that leaves a system folder by `..`, a link in a system folder that leads out
        // of it, a relative path, one that is not there, and one that can't be run.
        let payload = f.root.join("store/payload");
        let sneaky = f.root.join("lib/../store/payload");
        assert!(with(&sneaky).is_err());
        std::os::unix::fs::symlink(&payload, f.root.join("lib/ld-link.so")).unwrap();
        let why = with(&f.root.join("lib/ld-link.so")).unwrap_err();
        assert!(why.contains("system's folders"), "{why}");
        assert!(with(Path::new("lib/ld-linux.so.1")).is_err());
        let why = with(&f.root.join("lib/ld-gone.so")).unwrap_err();
        assert!(why.contains("not on this machine"), "{why}");
        std::fs::write(f.root.join("lib/ld-data.so"), b"data").unwrap();
        let why = with(&f.root.join("lib/ld-data.so")).unwrap_err();
        assert!(why.contains("not a program"), "{why}");
        assert!(!mark.exists(), "nothing the file names is run");
        // A link inside the system's folders is the system's own (`ld-linux` often is one).
        std::os::unix::fs::symlink(&loader, f.root.join("lib/ld-alias.so")).unwrap();
        assert_eq!(with(&f.root.join("lib/ld-alias.so")), Ok(()));
        drop(f.dir);
    }

    // covers: B-38
    #[test]
    fn an_elf_program_starts_when_each_library_it_needs_is_here_with_the_versions_it_needs() {
        let f = fixture();
        let system = linux(&f.root);
        let library = |versions: &[&str]| {
            elf_with(&ElfSpec {
                machine: AARCH64,
                defines: versions.to_vec(),
                ..ElfSpec::default()
            })
        };
        let needs = |name: &'static str| ElfSpec {
            machine: AARCH64,
            needed: vec![name],
            ..ElfSpec::default()
        };
        // A static program has nothing to find.
        let alone = ElfSpec {
            machine: AARCH64,
            ..ElfSpec::default()
        };
        assert_eq!(f.asked(&elf_with(&alone), &system), Ok(()));
        // A library in a system folder: not there, then there.
        let program = elf_with(&needs("libq.so.1"));
        let why = f.asked(&program, &system).unwrap_err();
        assert!(why.contains("libq.so.1"), "{why}");
        std::fs::write(f.root.join("lib/libq.so.1"), library(&[])).unwrap();
        assert_eq!(f.asked(&program, &system), Ok(()));
        // A file of that name that is no library for this CPU is not one the loader takes.
        let other_cpu = elf_with(&ElfSpec {
            machine: X86_64,
            ..ElfSpec::default()
        });
        for wrong in [b"not a library".to_vec(), other_cpu] {
            std::fs::write(f.root.join("lib/libq.so.1"), wrong).unwrap();
            assert!(f.asked(&program, &system).is_err());
        }
        // A version the program needs of it: one this machine's library lacks, then has.
        let versioned = elf_with(&ElfSpec {
            requires: Some("GLIBC_2.38"),
            ..needs("libq.so.1")
        });
        std::fs::write(f.root.join("lib/libq.so.1"), library(&["GLIBC_2.17"])).unwrap();
        let why = f.asked(&versioned, &system).unwrap_err();
        assert!(why.contains("GLIBC_2.38"), "{why}");
        let both = library(&["GLIBC_2.17", "GLIBC_2.38"]);
        std::fs::write(f.root.join("lib/libq.so.1"), both).unwrap();
        assert_eq!(f.asked(&versioned, &system), Ok(()));
        // A library beside the program by its own run path: `$ORIGIN` is the folder the
        // program will run from, not where the download lies.
        let beside = elf_with(&ElfSpec {
            runpath: Some("$ORIGIN/lib:$LIB/x"),
            ..needs("libbeside.so")
        });
        std::fs::write(f.root.join("store/libbeside.so"), library(&[])).unwrap();
        assert!(f.asked(&beside, &system).is_err());
        std::fs::create_dir_all(f.folder.join("lib")).unwrap();
        std::fs::write(f.folder.join("lib/libbeside.so"), library(&[])).unwrap();
        assert_eq!(f.asked(&beside, &system), Ok(()));
    }

    // covers: B-38
    #[test]
    fn an_elf_file_for_another_machine_or_cut_short_does_not_start() {
        let f = fixture();
        let system = linux(&f.root);
        let whole = elf_with(&ElfSpec {
            machine: AARCH64,
            ..ElfSpec::default()
        });
        assert_eq!(f.asked(&whole, &system), Ok(()));
        // Another CPU, another class, another byte order.
        let x86 = elf_with(&ElfSpec {
            machine: X86_64,
            ..ElfSpec::default()
        });
        let why = f.asked(&x86, &system).unwrap_err();
        assert!(why.contains("another CPU"), "{why}");
        for (at, value) in [(4, 1u8), (5, 2)] {
            let mut other = whole.clone();
            other[at] = value;
            let why = f.asked(&other, &system).unwrap_err();
            assert!(why.contains("another kind of machine"), "{why}");
        }
        // Cut anywhere: a segment, a table or the header itself ends after the file does.
        for len in [whole.len() - 1, whole.len() / 2, 70, 10, 2, 0] {
            assert!(f.asked(&whole[..len], &system).is_err(), "{len} bytes");
        }
        // A section that lies past the end.
        let mut sections = whole.clone();
        let at = sections.len() as u64;
        sections[0x28..0x30].copy_from_slice(&at.to_le_bytes());
        sections[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        sections[0x3c..0x3e].copy_from_slice(&1u16.to_le_bytes());
        let mut section = [0u8; 64];
        section[4..8].copy_from_slice(&1u32.to_le_bytes());
        section[0x18..0x20].copy_from_slice(&at.to_le_bytes());
        section[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        sections.extend(section);
        assert_eq!(f.asked(&sections, &system), Ok(()), "its own bytes");
        let last = sections.len() - 64 + 0x20;
        sections[last..last + 8].copy_from_slice(&65u64.to_le_bytes());
        let why = f.asked(&sections, &system).unwrap_err();
        assert!(why.contains("sections"), "{why}");
        // Not this system's format at all: a script, a Mach-O file.
        for other in [b"#!/bin/sh\nexit 0\n".to_vec(), macho(&[])] {
            let why = f.asked(&other, &system).unwrap_err();
            assert!(why.contains("not a program this system starts"), "{why}");
        }
    }

    // covers: B-38
    #[test]
    fn a_mach_o_program_starts_when_each_library_it_loads_is_here() {
        let f = fixture();
        let cache_file = f.root.join("dyld_shared_cache");
        let system = mac(&cache_file);
        std::fs::write(&cache_file, cache(&["/usr/lib/libSystem.B.dylib"])).unwrap();
        let system_only = macho(&["/usr/lib/libSystem.B.dylib"]);
        assert_eq!(f.asked(&system_only, &system), Ok(()));
        // A system library this system's cache doesn't have: its path alone is no proof.
        let why = f
            .asked(&macho(&["/usr/lib/libNOPE.B.dylib"]), &system)
            .unwrap_err();
        assert!(why.contains("libNOPE.B.dylib"), "{why}");
        // One that may be absent (a weak load) is not asked for.
        let weak = macho_with(
            CPU_ARM64,
            &[("/usr/lib/libNOPE.B.dylib", true)],
            &[],
            "/usr/lib/dyld",
        );
        assert_eq!(f.asked(&weak, &system), Ok(()));
        // One beside the program, by `@loader_path` and by a run path: where the program will
        // run, not where the download lies; and it must be a library for this CPU.
        let library = macho(&[]);
        for (name, rpaths) in [
            ("@loader_path/libq.dylib", vec![]),
            (
                "@rpath/libq.dylib",
                vec!["/nonexistent", "@executable_path"],
            ),
        ] {
            let program = macho_with(CPU_ARM64, &[(name, false)], &rpaths, "/usr/lib/dyld");
            std::fs::write(f.root.join("store/libq.dylib"), &library).unwrap();
            let _ = std::fs::remove_file(f.folder.join("libq.dylib"));
            let why = f.asked(&program, &system).unwrap_err();
            assert!(why.contains("libq.dylib"), "{why}");
            std::fs::write(f.folder.join("libq.dylib"), b"q").unwrap();
            assert!(f.asked(&program, &system).is_err(), "not a library");
            let x86 = macho_with(CPU_X86_64, &[], &[], "/usr/lib/dyld");
            std::fs::write(f.folder.join("libq.dylib"), x86).unwrap();
            assert!(f.asked(&program, &system).is_err(), "another CPU's");
            std::fs::write(f.folder.join("libq.dylib"), &library).unwrap();
            assert_eq!(f.asked(&program, &system), Ok(()));
        }
        // An absolute path outside the system's own.
        assert!(
            f.asked(&macho(&["/opt/gone/lib/libz.9.dylib"]), &system)
                .is_err()
        );
        // Another dynamic loader than the system's.
        let payload = f.root.join("store/payload");
        let other = macho_with(CPU_ARM64, &[], &[], payload.to_str().unwrap());
        let why = f.asked(&other, &system).unwrap_err();
        assert!(why.contains("/usr/lib/dyld"), "{why}");
        // No cache that can be read: the SDK's stub says what the system has.
        let no_cache = mac(&f.root.join("no-cache"));
        assert!(f.asked(&system_only, &no_cache).is_err());
        let sdk = f.root.join("sdk");
        std::fs::create_dir_all(sdk.join("usr/lib")).unwrap();
        std::fs::write(sdk.join("usr/lib/libSystem.B.tbd"), b"--- !tapi-tbd\n").unwrap();
        let here = Here {
            folder: &f.folder,
            sdk: Some(&sdk),
        };
        assert_eq!(check(&f.program, &here, &no_cache), Ok(()));
        std::fs::write(&f.program, macho(&["/usr/lib/libNOPE.B.dylib"])).unwrap();
        assert!(check(&f.program, &here, &no_cache).is_err());
    }

    // covers: B-38
    #[test]
    fn a_mach_o_file_for_another_cpu_or_cut_short_does_not_start() {
        let f = fixture();
        let cache_file = f.root.join("dyld_shared_cache");
        std::fs::write(&cache_file, cache(&["/usr/lib/libSystem.B.dylib"])).unwrap();
        let system = mac(&cache_file);
        let dylibs = [("/usr/lib/libSystem.B.dylib", false)];
        let whole = macho_with(CPU_ARM64, &dylibs, &[], "/usr/lib/dyld");
        assert_eq!(f.asked(&whole, &system), Ok(()));
        let x86 = macho_with(CPU_X86_64, &dylibs, &[], "/usr/lib/dyld");
        let why = f.asked(&x86, &system).unwrap_err();
        assert!(why.contains("another CPU"), "{why}");
        for len in [whole.len() - 1, whole.len() / 2, 40, 3, 0] {
            assert!(f.asked(&whole[..len], &system).is_err(), "{len} bytes");
        }
        // A universal file: the part for this CPU is the one that counts, and must be whole.
        let fat = |parts: &[(u32, &[u8])]| {
            let mut out = Vec::new();
            out.extend(0xcafe_babe_u32.to_be_bytes());
            out.extend((parts.len() as u32).to_be_bytes());
            let mut at = 8 + parts.len() * 20;
            let mut body: Vec<u8> = Vec::new();
            for (cpu, bytes) in parts {
                for word in [*cpu, 0, at as u32, bytes.len() as u32, 0] {
                    out.extend(word.to_be_bytes());
                }
                body.extend(*bytes);
                at += bytes.len();
            }
            out.extend(body);
            out
        };
        let both = fat(&[(CPU_X86_64, &x86), (CPU_ARM64, &whole)]);
        assert_eq!(f.asked(&both, &system), Ok(()));
        let why = f.asked(&fat(&[(CPU_X86_64, &x86)]), &system).unwrap_err();
        assert!(why.contains("no part for this machine's CPU"), "{why}");
        assert!(f.asked(&both[..both.len() - 1], &system).is_err());
        // Not this system's format: a script, an ELF file.
        let elf = elf_with(&ElfSpec {
            machine: AARCH64,
            ..ElfSpec::default()
        });
        for other in [b"#!/bin/sh\nexit 0\n".to_vec(), elf] {
            let why = f.asked(&other, &system).unwrap_err();
            assert!(why.contains("not a program this system starts"), "{why}");
        }
    }

    // covers: B-38
    #[test]
    fn this_test_program_can_start_here_and_a_made_up_one_as_the_format_says() {
        // The real thing on whatever this runs on: the test's own program, which did start,
        // read where it lies.
        let me = std::env::current_exe().unwrap();
        let folder = me.parent().unwrap().to_path_buf();
        let here = Here {
            folder: &folder,
            sdk: None,
        };
        assert_eq!(can_start(&me, &here), Ok(()));
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("p");
        std::fs::write(&program, host_program(None)).unwrap();
        assert_eq!(can_start(&program, &here), Ok(()));
        std::fs::write(&program, host_program(Some("libhrq-nowhere.so.9"))).unwrap();
        let why = can_start(&program, &here).unwrap_err();
        assert!(why.contains("libhrq-nowhere.so.9"), "{why}");
    }
}

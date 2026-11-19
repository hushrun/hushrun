//! What a C or C++ block's bytes must be before they are served (B-59): an object of the
//! compile's kind, and messages that can't drive a terminal.
//!
//! Ninja, Make and the linker take any file with the right name, so these checks are the only
//! ones there are before a downloaded object is linked into a program. They catch garbage, not
//! malice: an object of the right format with other code in it passes, and only the audit (B-59)
//! finds it.

/// Why an object's bytes aren't the compile's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrong {
    /// Neither ELF, Mach-O nor (for clang's `-flto`) LLVM bitcode.
    Format,
    /// An object file, but not a relocatable one (`ET_REL`, `MH_OBJECT`): a program, a library.
    Type,
    /// An object of another machine or width than the target's.
    Machine,
}

impl Wrong {
    /// The launcher's word for it, in the run's log.
    pub fn word(self) -> &'static str {
        match self {
            Wrong::Format => "wrong-format",
            Wrong::Type => "wrong-type",
            Wrong::Machine => "wrong-machine",
        }
    }
}

/// A target's machine as ELF and Mach-O name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Machine {
    /// ELF's `e_machine`.
    elf: u16,
    /// 64-bit (ELFCLASS64, `MH_MAGIC_64`).
    wide: bool,
    /// Little-endian.
    little: bool,
    /// Mach-O's `cputype`, where Mach-O has one.
    macho: Option<u32>,
}

const CPU_ARCH_ABI64: u32 = 0x0100_0000;
const CPU_ARCH_ABI64_32: u32 = 0x0200_0000;

/// The machine of a target triple's first part, or `None` for one this version doesn't know.
fn machine(target: &str) -> Option<Machine> {
    let arch = target.split('-').next().unwrap_or_default();
    let m = |elf: u16, wide: bool, little: bool, macho: Option<u32>| {
        Some(Machine {
            elf,
            wide,
            little,
            macho,
        })
    };
    match arch {
        "x86_64" | "amd64" | "x86_64h" => m(62, true, true, Some(7 | CPU_ARCH_ABI64)),
        "i386" | "i486" | "i586" | "i686" => m(3, false, true, Some(7)),
        "aarch64" | "arm64" | "arm64e" => m(183, true, true, Some(12 | CPU_ARCH_ABI64)),
        "aarch64_be" => m(183, true, false, None),
        "arm64_32" => m(183, false, true, Some(12 | CPU_ARCH_ABI64_32)),
        a if a.starts_with("armeb") || a.starts_with("thumbeb") => m(40, false, false, None),
        a if a.starts_with("arm") || a.starts_with("thumb") => m(40, false, true, Some(12)),
        "riscv64" => m(243, true, true, None),
        "riscv32" => m(243, false, true, None),
        "powerpc64le" | "ppc64le" => m(21, true, true, None),
        "powerpc64" | "ppc64" => m(21, true, false, None),
        "powerpc" | "ppc" => m(20, false, false, None),
        "s390x" => m(22, true, false, None),
        "loongarch64" => m(258, true, true, None),
        _ => None,
    }
}

/// Whether the launcher can check an object of `target` (B-59): a target this version doesn't
/// know is never served from a download, since nothing could tell its objects from garbage.
pub fn knows(target: &str) -> bool {
    machine(target).is_some()
}

/// Whether `bytes` are an object a compile for `target` writes (B-59): ELF `ET_REL` or Mach-O
/// `MH_OBJECT` of the target's machine, width and byte order; or, when the compile asked for
/// clang's `-flto` (`lto`), LLVM bitcode, raw or in its wrapper, whose target the check doesn't
/// read. A target [`knows`] doesn't know passes nothing.
pub fn object(bytes: &[u8], target: &str, lto: bool) -> Result<(), Wrong> {
    let Some(machine) = machine(target) else {
        return Err(Wrong::Machine);
    };
    if lto && is_bitcode(bytes) {
        return Ok(());
    }
    if bytes.starts_with(b"\x7fELF") {
        return elf(bytes, machine);
    }
    if bytes.len() >= 4 {
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic == 0xfeed_face || magic == 0xfeed_facf {
            return macho(bytes, magic == 0xfeed_facf, machine);
        }
    }
    Err(Wrong::Format)
}

/// LLVM bitcode: `BC\xC0\xDE`, or the wrapper Apple's tools write (`0x0B17C0DE`, little-endian).
fn is_bitcode(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BC\xc0\xde") || bytes.starts_with(&[0xde, 0xc0, 0x17, 0x0b])
}

fn elf(bytes: &[u8], machine: Machine) -> Result<(), Wrong> {
    if bytes.len() < 20 {
        return Err(Wrong::Format);
    }
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        _ => return Err(Wrong::Format),
    };
    let little = match bytes[5] {
        1 => true,
        2 => false,
        _ => return Err(Wrong::Format),
    };
    let half = |at: usize| {
        let pair = [bytes[at], bytes[at + 1]];
        if little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    };
    if half(16) != 1 {
        return Err(Wrong::Type);
    }
    if wide != machine.wide || little != machine.little || half(18) != machine.elf {
        return Err(Wrong::Machine);
    }
    Ok(())
}

fn macho(bytes: &[u8], wide: bool, machine: Machine) -> Result<(), Wrong> {
    if bytes.len() < 16 {
        return Err(Wrong::Format);
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    // MH_OBJECT.
    if word(12) != 1 {
        return Err(Wrong::Type);
    }
    if Some(word(4)) != machine.macho || wide != machine.wide {
        return Err(Wrong::Machine);
    }
    Ok(())
}

/// `bytes` with every control character taken out but newlines, tabs, carriage returns and SGR
/// sequences (`ESC [ <digits and ;> m`, colour and weight), which is all a compiler's
/// diagnostics use: what a downloaded block's messages are written as (B-59), so that a forged
/// block can't move the cursor, set a window's title, write a link or ring a bell on the
/// person's terminal. Bytes of 0x80 and above, UTF-8 among them, pass as they are.
pub fn sgr_only(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            if let Some(len) = sgr_len(&bytes[i..]) {
                out.extend_from_slice(&bytes[i..i + len]);
                i += len;
            } else {
                i += 1;
            }
            continue;
        }
        if matches!(b, b'\n' | b'\t' | b'\r') || !(b < 0x20 || b == 0x7f) {
            out.push(b);
        }
        i += 1;
    }
    out
}

/// The length of an SGR sequence at the start of `bytes`: `ESC [`, digits and `;`, `m`.
fn sgr_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) != Some(&b'[') {
        return None;
    }
    for (at, b) in bytes.iter().enumerate().skip(2) {
        match b {
            b'0'..=b'9' | b';' => {}
            b'm' => return Some(at + 1),
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf_header(class: u8, data: u8, kind: u16, machine: u16) -> Vec<u8> {
        let mut h = vec![0u8; 64];
        h[..4].copy_from_slice(b"\x7fELF");
        h[4] = class;
        h[5] = data;
        let (k, m) = if data == 1 {
            (kind.to_le_bytes(), machine.to_le_bytes())
        } else {
            (kind.to_be_bytes(), machine.to_be_bytes())
        };
        h[16..18].copy_from_slice(&k);
        h[18..20].copy_from_slice(&m);
        h
    }

    fn macho_header(wide: bool, cpu: u32, filetype: u32) -> Vec<u8> {
        let mut h = vec![0u8; 32];
        let magic: u32 = if wide { 0xfeed_facf } else { 0xfeed_face };
        h[..4].copy_from_slice(&magic.to_le_bytes());
        h[4..8].copy_from_slice(&cpu.to_le_bytes());
        h[12..16].copy_from_slice(&filetype.to_le_bytes());
        h
    }

    // covers: B-59
    #[test]
    fn an_object_is_served_only_as_the_relocatable_object_of_the_targets_machine() {
        let x86 = "x86_64-linux-gnu";
        let arm = "aarch64-unknown-linux-gnu";
        assert_eq!(object(&elf_header(2, 1, 1, 62), x86, false), Ok(()));
        assert_eq!(object(&elf_header(2, 1, 1, 183), arm, false), Ok(()));
        // Another machine, another width, another byte order.
        assert_eq!(
            object(&elf_header(2, 1, 1, 183), x86, false),
            Err(Wrong::Machine)
        );
        assert_eq!(
            object(&elf_header(1, 1, 1, 62), x86, false),
            Err(Wrong::Machine)
        );
        assert_eq!(
            object(&elf_header(2, 2, 1, 62), x86, false),
            Err(Wrong::Machine)
        );
        // A program or a shared library is no object.
        assert_eq!(
            object(&elf_header(2, 1, 2, 62), x86, false),
            Err(Wrong::Type)
        );
        assert_eq!(
            object(&elf_header(2, 1, 3, 62), x86, false),
            Err(Wrong::Type)
        );
        // Mach-O.
        let mac = "arm64-apple-darwin25.2.0";
        assert_eq!(
            object(&macho_header(true, 0x0100_000c, 1), mac, false),
            Ok(())
        );
        assert_eq!(
            object(&macho_header(true, 0x0100_0007, 1), mac, false),
            Err(Wrong::Machine)
        );
        assert_eq!(
            object(&macho_header(true, 0x0100_000c, 2), mac, false),
            Err(Wrong::Type)
        );
        assert_eq!(
            object(
                &macho_header(true, 0x0100_0007, 1),
                "x86_64-apple-darwin",
                false
            ),
            Ok(())
        );
        // Garbage, and what is too short to tell.
        assert_eq!(object(b"#!/bin/sh\n", x86, false), Err(Wrong::Format));
        assert_eq!(object(b"\x7fELF\x02\x01", x86, false), Err(Wrong::Format));
        assert_eq!(object(b"", x86, false), Err(Wrong::Format));
        // Bitcode only for `-flto`, raw or wrapped.
        assert_eq!(object(b"BC\xc0\xde\x35\x14", x86, true), Ok(()));
        assert_eq!(object(&[0xde, 0xc0, 0x17, 0x0b, 0, 0], mac, true), Ok(()));
        assert_eq!(
            object(b"BC\xc0\xde\x35\x14", x86, false),
            Err(Wrong::Format)
        );
        // GCC's `-flto` writes ELF.
        assert_eq!(object(&elf_header(2, 1, 1, 62), x86, true), Ok(()));
        // A target this version doesn't know passes nothing.
        assert!(!knows("vax-dec-ultrix"));
        assert!(knows(x86) && knows(mac) && knows("armv7-unknown-linux-gnueabihf"));
        assert_eq!(
            object(&elf_header(2, 1, 1, 62), "vax-dec-ultrix", false),
            Err(Wrong::Machine)
        );
    }

    // covers: B-59
    #[test]
    fn downloaded_messages_keep_only_colour() {
        let warning =
            b"\x1b[1m\x1b[0;1;35mwarning: \x1b[0mx\tunused\r\n\xe2\x80\x98y\xe2\x80\x99\n";
        assert_eq!(sgr_only(warning), warning.to_vec());
        // A title, a link, the cursor moved, the screen cleared, a bell, a backspace, a DEL.
        let forged =
            b"a\x1b]0;owned\x07b\x1b]8;;http://x\x1b\\c\x1b[2J\x1b[10;10Hd\x08\x7fe\x1b[1;31";
        assert_eq!(
            sgr_only(forged),
            b"a]0;ownedb]8;;http://x\\c[2J[10;10Hde[1;31".to_vec()
        );
        assert_eq!(sgr_only(b"\x1b"), Vec::<u8>::new());
    }
}

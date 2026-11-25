//! \[C-147\] What the reach walk reads of a SCIP symbol rust-analyzer writes: `rust-analyzer cargo
//! <package> <version> <descriptors>`, or `local <n>`.

/// A symbol's parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol<'a> {
    pub package: &'a str,
    pub version: &'a str,
    /// The descriptors, as written: `names/impl#[CheckName]new().`.
    pub descriptors: &'a str,
}

/// What a symbol's last descriptor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    /// `name/`: a module or a crate.
    Namespace,
    /// `name#`: a type, a trait, an `impl`.
    Type,
    /// `name.`: a value, a field, a const.
    Term,
    /// `name().` or `name(disambiguator).`.
    Method,
    /// `name!`.
    Macro,
    /// `name:`: a derive or an attribute macro.
    Meta,
    /// Anything else (a type parameter `[T]`, a parameter `(x)`).
    Other,
}

impl<'a> Symbol<'a> {
    /// The parts of a global symbol; `None` for a local or one this reader doesn't know.
    pub fn parse(symbol: &'a str) -> Option<Symbol<'a>> {
        let mut parts = symbol.splitn(5, ' ');
        let scheme = parts.next()?;
        if scheme == "local" {
            return None;
        }
        let _manager = parts.next()?;
        let package = parts.next()?;
        let version = parts.next()?;
        let descriptors = parts.next()?;
        Some(Symbol {
            package,
            version,
            descriptors,
        })
    }

    /// What its last descriptor is.
    pub fn last(&self) -> Last {
        let d = self.descriptors;
        if d.ends_with(").") {
            Last::Method
        } else if d.ends_with('/') {
            Last::Namespace
        } else if d.ends_with('#') {
            Last::Type
        } else if d.ends_with('.') {
            Last::Term
        } else if d.ends_with('!') {
            Last::Macro
        } else if d.ends_with(':') {
            Last::Meta
        } else {
            Last::Other
        }
    }

    /// Its last descriptor's name, unescaped: `new` of `names/impl#[CheckName]new().`.
    pub fn name(&self) -> &'a str {
        let names = names(self.descriptors);
        names.last().copied().unwrap_or("")
    }

    /// Whether it is the standard library's (`std`, `core`, `alloc`, `proc_macro`, `test`).
    pub fn is_std(&self) -> bool {
        matches!(
            self.package,
            "std" | "core" | "alloc" | "proc_macro" | "test"
        )
    }

    /// The symbol of what holds it, its last descriptor dropped (with what bracketed it):
    /// `names/Tr#` of `names/Tr#m().`. `None` past the first descriptor.
    pub fn owner(symbol: &str) -> Option<String> {
        let parsed = Symbol::parse(symbol)?;
        let d = parsed.descriptors;
        let cut = last_descriptor_start(d)?;
        if cut == 0 {
            return None;
        }
        let prefix_len = symbol.len() - d.len();
        Some(symbol[..prefix_len + cut].to_string())
    }
}

/// Where the last descriptor of `d` starts, past backquoted names and bracketed ones.
fn last_descriptor_start(d: &str) -> Option<usize> {
    let bytes = d.as_bytes();
    // The end of the descriptor before the last: a `/`, `#`, `.` or `!` or `:` that ends one,
    // outside backquotes and brackets, before the last one.
    let mut ends: Vec<usize> = Vec::new();
    let mut quoted = false;
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            quoted = !quoted;
        } else if !quoted {
            match c {
                b'[' | b'(' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        ends.push(i + 1);
                    }
                }
                b')' => {
                    depth -= 1;
                    // `name().` ends with the `.` after it.
                    if depth == 0 && bytes.get(i + 1) == Some(&b'.') {
                        i += 1;
                        ends.push(i + 1);
                    }
                }
                b'/' | b'#' | b'.' | b'!' | b':' if depth == 0 => ends.push(i + 1),
                _ => {}
            }
        }
        i += 1;
    }
    ends.pop()?;
    Some(ends.pop().unwrap_or(0))
}

/// Every name in descriptors, in order, unescaped: `names`, `impl`, `CheckName`, `new`.
pub fn names(d: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let end = d[i + 1..].find('`').map_or(d.len(), |e| i + 1 + e);
            out.push(&d[i + 1..end]);
            i = end + 1;
            continue;
        }
        if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80 {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
            {
                i += 1;
            }
            out.push(&d[start..i]);
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-147
    #[test]
    fn symbols_are_read_as_rust_analyzer_writes_them() {
        let s = "rust-analyzer cargo hushrun-core 0.4.0 names/impl#[CheckName]new().";
        let p = Symbol::parse(s).unwrap();
        assert_eq!((p.package, p.version), ("hushrun-core", "0.4.0"));
        assert_eq!(p.last(), Last::Method);
        assert_eq!(p.name(), "new");
        assert_eq!(
            Symbol::owner(s).as_deref(),
            Some("rust-analyzer cargo hushrun-core 0.4.0 names/impl#[CheckName]")
        );
        let m = "rust-analyzer cargo a 0.1.0 x/Tr#m().";
        assert_eq!(
            Symbol::owner(m).as_deref(),
            Some("rust-analyzer cargo a 0.1.0 x/Tr#")
        );
        assert_eq!(
            Symbol::owner("rust-analyzer cargo a 0.1.0 x/Tr#").as_deref(),
            Some("rust-analyzer cargo a 0.1.0 x/")
        );
        assert_eq!(Symbol::owner("rust-analyzer cargo a 0.1.0 x/"), None);
        let std = "rust-analyzer cargo core https://github.com/rust-lang/rust/library/core slice/impl#[`[T]`]first().";
        let p = Symbol::parse(std).unwrap();
        assert!(p.is_std());
        assert_eq!(p.name(), "first");
        assert_eq!(Symbol::parse("local 3"), None);
        assert_eq!(
            Symbol::parse("rust-analyzer cargo a 0.1.0 m!")
                .unwrap()
                .last(),
            Last::Macro
        );
        assert_eq!(
            Symbol::parse("rust-analyzer cargo a 0.1.0 D:")
                .unwrap()
                .last(),
            Last::Meta
        );
        assert_eq!(
            Symbol::parse("rust-analyzer cargo a 0.1.0 S#f.")
                .unwrap()
                .last(),
            Last::Term
        );
        assert_eq!(
            names("impl#[`Iter<'a, T>`][Iterator]all()."),
            ["impl", "Iter<'a, T>", "Iterator", "all"]
        );
    }
}

//! \[ADR 0096\] The environment a unit's key holds what its tests read of (C-31's `env`): Go's
//! today ([`super::adapter::KeysOutside::Getenv`]). The code every adapter shares keys a list by
//! it (`shared::lists`), so it is a file of its own, not Go's adapter's.

use std::collections::BTreeSet;
use std::path::Path;

use hushrun_core::units;

/// \[ADR 0096\] The variable every package's key holds, read or not: Go's runtime reads it
/// without telling the test log, and Go's own test cache keys it always (`computeTestInputsID`).
pub const ALWAYS_READ: &str = "GODEBUG";

/// \[ADR 0096\] A variable's value in an environment, `None` when unset.
type Lookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// \[ADR 0096\] The environment a Go package's tests ran in, or a plan's would, as its key holds
/// what they read of it (C-31's `env`): COMMAND's own `env NAME=value` assignments over hushrun's
/// environment, with the checkout's folders (the repository's root and the home folder, B-8's
/// replacement) replaced in each value. The plan, the record after COMMAND and `hushrun why` all
/// key by [`Getenv::outside`].
pub struct Getenv<'a> {
    lookup: Box<Lookup<'a>>,
    roots: Option<hushrun_build::roots::Roots>,
}

impl<'a> Getenv<'a> {
    /// The environment of COMMAND, whose assignments are `assigned`, over hushrun's, in the
    /// repository at `root` ([`super::command::variable`]): what a run, a plan and `why` key by.
    pub fn of_command(root: &Path, assigned: &'a [(String, String)]) -> Getenv<'a> {
        Getenv::of(root, move |name| super::command::variable(assigned, name))
    }

    /// The environment whose variables `lookup` gives (`None`: unset), in the repository at
    /// `root`.
    pub fn of(root: &Path, lookup: impl Fn(&str) -> Option<String> + 'a) -> Getenv<'a> {
        let home = lookup("HOME")
            .filter(|home| home.starts_with('/') && !home.trim_matches('/').is_empty());
        let roots = root.to_str().and_then(|checkout| {
            hushrun_build::roots::Roots::of_checkout(
                &hushrun_build::roots::CheckoutFolders {
                    checkout: checkout.to_string(),
                    home,
                    ..Default::default()
                },
                hushrun_build::roots::resolve_on_disk,
            )
            .ok()
        });
        Getenv {
            lookup: Box::new(lookup),
            roots,
        }
    }

    /// A variable's value; `None` when unset.
    fn value(&self, name: &str) -> Option<String> {
        (self.lookup)(name)
    }

    /// What a key holds of a package whose tests read `names`: their `env`, each name with its
    /// value now, roots replaced, or alone when unset (set to nothing is a value), and
    /// [`ALWAYS_READ`]'s, as [`units::env()`] digests them.
    pub fn outside(&self, names: &BTreeSet<String>) -> units::Outside {
        let entries: Vec<(String, Option<String>)> = names
            .iter()
            .map(String::as_str)
            .chain([ALWAYS_READ])
            .map(|name| {
                let value = self.value(name).map(|value| match &self.roots {
                    Some(roots) => roots.to_placeholders(&value),
                    None => value,
                });
                (name.to_string(), value)
            })
            .collect();
        units::Outside {
            env: Some(units::env(
                entries.iter().map(|(n, v)| (n.as_str(), v.as_deref())),
            )),
            scripts: None,
        }
    }
}

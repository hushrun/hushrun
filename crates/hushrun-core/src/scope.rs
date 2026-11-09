//! Scopes: the deployable parts of a repository, chosen by include and exclude patterns
//! (D-7, C-4).

use crate::glob::Pattern;
use crate::names::ScopeName;

/// A scope with compiled patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: ScopeName,
    pub include: Vec<Pattern>,
    pub exclude: Vec<Pattern>,
    /// \[ADR 0037\] What its checks build against outside the repository (C-4).
    pub external: Vec<External>,
}

/// \[ADR 0037\] Another checkout a scope's checks build against, which the repository's own files
/// don't pin: a run's key covers it (C-9), so a change there runs the check again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct External {
    /// Where it is, relative to the repository's root, as `hushrun.yaml` says it: `../cli`.
    pub path: String,
    pub include: Vec<Pattern>,
    pub exclude: Vec<Pattern>,
}

/// Every scope of a policy, and the global `digest.exclude` patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scopes {
    scopes: Vec<Scope>,
    global_exclude: Vec<Pattern>,
}

/// Which patterns decide whether a path belongs to a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership<'a> {
    /// The first `include` pattern that matches.
    pub included_by: Option<&'a Pattern>,
    /// The first scope `exclude` pattern that matches.
    pub excluded_by: Option<&'a Pattern>,
    /// The first `digest.exclude` pattern that matches.
    pub globally_excluded_by: Option<&'a Pattern>,
}

impl Membership<'_> {
    /// D-7: included, and excluded by neither the scope nor `digest.exclude`.
    pub fn is_member(&self) -> bool {
        self.included_by.is_some()
            && self.excluded_by.is_none()
            && self.globally_excluded_by.is_none()
    }
}

impl Scopes {
    /// Scopes sorted by name.
    pub fn new(mut scopes: Vec<Scope>, global_exclude: Vec<Pattern>) -> Self {
        scopes.sort_by(|a, b| a.name.cmp(&b.name));
        Scopes {
            scopes,
            global_exclude,
        }
    }

    /// The single scope `default`, covering every entry, of a policy with no `scopes:`.
    pub fn default_scope(global_exclude: Vec<Pattern>) -> Self {
        let all = Pattern::new("**").expect("`**` is a valid pattern");
        Scopes::new(
            vec![Scope {
                name: ScopeName::default_scope(),
                include: vec![all],
                exclude: Vec::new(),
                external: Vec::new(),
            }],
            global_exclude,
        )
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.scopes.iter()
    }

    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Scope> {
        self.scopes.iter().find(|s| s.name.as_str() == name)
    }

    pub fn global_exclude(&self) -> &[Pattern] {
        &self.global_exclude
    }

    /// Which patterns decide whether `path` belongs to `scope`.
    pub fn membership<'a>(&'a self, scope: &'a Scope, path: &str) -> Membership<'a> {
        Membership {
            included_by: scope.include.iter().find(|p| p.is_match(path)),
            excluded_by: scope.exclude.iter().find(|p| p.is_match(path)),
            globally_excluded_by: self.global_exclude.iter().find(|p| p.is_match(path)),
        }
    }

    /// D-7: whether `path`, a normalized path, belongs to `scope`.
    pub fn contains(&self, scope: &Scope, path: &str) -> bool {
        scope.include.iter().any(|p| p.is_match(path))
            && !scope.exclude.iter().any(|p| p.is_match(path))
            && !self.global_exclude.iter().any(|p| p.is_match(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Vec<Pattern> {
        list.iter().map(|p| Pattern::new(p).unwrap()).collect()
    }

    // covers: D-7
    #[test]
    fn membership_needs_an_include_and_no_exclude() {
        let api = Scope {
            name: ScopeName::new("api").unwrap(),
            include: patterns(&["services/api/**", "go.mod"]),
            exclude: patterns(&["**/*_test.go"]),
            external: Vec::new(),
        };
        let scopes = Scopes::new(vec![api], patterns(&["**/*.md"]));
        let api = scopes.get("api").unwrap();
        assert!(scopes.contains(api, "services/api/main.go"));
        assert!(scopes.contains(api, "go.mod"));
        assert!(!scopes.contains(api, "services/api/main_test.go"));
        assert!(!scopes.contains(api, "services/api/README.md"));
        assert!(!scopes.contains(api, "services/web/main.go"));
        let m = scopes.membership(api, "services/api/README.md");
        assert_eq!(m.included_by.unwrap().as_str(), "services/api/**");
        assert_eq!(m.globally_excluded_by.unwrap().as_str(), "**/*.md");
        assert!(!m.is_member());
    }

    #[test]
    fn default_scope_covers_everything() {
        let scopes = Scopes::default_scope(Vec::new());
        let default = scopes.get("default").unwrap();
        assert!(scopes.contains(default, "a"));
        assert!(scopes.contains(default, "a/b/c"));
    }
}

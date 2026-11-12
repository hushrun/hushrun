//! Macros known to read files and variables the compiler does not list (B-6 `declared`).
//!
//! A proc macro can open a file or read a variable without telling rustc, so the dep-info doesn't
//! list it and neither cargo nor the wrapper sees it. For the well-known ones the wrapper adds
//! their inputs by name: given the crates a compile names with `--extern`, [`inputs`] says which
//! variables, files and folders to add. A place that doesn't exist is still an input (absent);
//! the wrapper records that. This module only names places.
//!
//! Checked and left out, because they list what they read or bake a path the output search
//! (B-10) catches:
//!
//! - `askama`: emits `include_bytes!` for every template and for `askama.toml`
//!   (`askama_derive/src/generator.rs`, `impl_template`).
//! - `pest_derive`: emits `include_str!` for each `#[grammar]` file.
//! - `include_dir`: emits `include_bytes!` for each file it embeds.
//! - `rust-embed`: `include_bytes!` for each file when it embeds; otherwise the folder's absolute
//!   path is baked into the output and the files are read when the program runs.
//! - `git-version`: emits `include_bytes!` for `<git dir>/logs/HEAD` and `<git dir>/index`
//!   (`git-version-macro/src/utils.rs`, `git_dependencies`).
//! - `refinery`'s `.sql` and `.rs` migrations are each in an `include_str!`, but the folder listing
//!   isn't: it is in the table for that.

/// A place, relative to a folder the wrapper knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Where {
    /// This path under the package folder (`CARGO_MANIFEST_DIR`).
    Package(&'static str),
    /// This path under the workspace folder (rustc's working directory under cargo).
    Workspace(&'static str),
    /// This name in the working directory and in each folder above it, up to the file system's
    /// root. What `dotenvy::dotenv()` searches. For a file, the places under a root are each an
    /// input where they lie, and above every root the input is the first file of that name
    /// found, by its bytes, wherever it is (B-6 `declared`).
    Upward(&'static str),
    /// This name in the package folder and in each folder above it, up to the file system's
    /// root. Covers a search that starts at the package and stops at the workspace or at the first
    /// one found. Keyed as [`Where::Upward`] is.
    UpwardFromPackage(&'static str),
}

/// What to add to a compile's key for the macros it can call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Known {
    /// Variables the macros read without an `env-dep` line, sorted, each once.
    pub env: Vec<&'static str>,
    /// Files to add by content, or as absent, in table order, each once.
    pub files: Vec<Where>,
    /// Folders to add in full (every file under them), or as absent, in table order, each once.
    pub folders: Vec<Where>,
}

/// One row of the table: a crate a compile can name with `--extern`, and what its macros read.
struct Entry {
    externs: &'static [&'static str],
    env: &'static [&'static str],
    files: &'static [Where],
    folders: &'static [Where],
}

/// The macros known to read inputs the compiler doesn't list, by the crate names a compile passes
/// to `--extern` (`-` written `_`, as cargo does).
const TABLE: &[Entry] = &[
    // sqlx's `query!` family (`sqlx-macros-core`, `query/mod.rs` in 0.8.6, `query/metadata.rs` on
    // main). It reads `DATABASE_URL`, `SQLX_OFFLINE` and `SQLX_OFFLINE_DIR` with `std::env::var`
    // (only a nightly cfg uses `tracked_env`). 0.8.6 reads `<package>/.env`, or else the first
    // `.env` from the working directory up (`dotenvy::dotenv_iter`). Main reads `.env` in the
    // package and each folder above it up to the workspace, and `<package>/sqlx.toml`. Cached
    // queries are `query-<hash>.json` in `SQLX_OFFLINE_DIR`, `<package>/.sqlx` or
    // `<workspace>/.sqlx`, read with `std::fs` (the `tracked_path` call is nightly only).
    // `migrate!` puts each migration in an `include_str!` but lists the folder with `read_dir`,
    // so a migration added to an empty `migrations` folder would go unseen.
    Entry {
        externs: &["sqlx", "sqlx_macros"],
        env: &["DATABASE_URL", "SQLX_OFFLINE", "SQLX_OFFLINE_DIR"],
        files: &[
            Where::Package(".env"),
            Where::Workspace(".env"),
            Where::Upward(".env"),
            Where::UpwardFromPackage(".env"),
            Where::Package("sqlx.toml"),
            Where::Workspace("sqlx.toml"),
        ],
        folders: &[
            Where::Package(".sqlx"),
            Where::Workspace(".sqlx"),
            Where::Package("migrations"),
        ],
    },
    // `dotenvy_macro::dotenv!` (0.15.7, `src/lib.rs`) calls `dotenvy::dotenv()`, which reads the
    // first `.env` from the working directory up, and emits no `include_str!` for it.
    Entry {
        externs: &["dotenvy_macro"],
        env: &[],
        files: &[Where::Upward(".env")],
        folders: &[],
    },
    // `dotenv_codegen::dotenv!` (`dotenv_codegen_implementation` 0.15.0, `src/lib.rs`) calls
    // `dotenv::dotenv()`: the same search, the same lack of tracking.
    Entry {
        externs: &["dotenv_codegen"],
        env: &[],
        files: &[Where::Upward(".env")],
        folders: &[],
    },
    // `diesel_migrations::embed_migrations!` (`migrations_macros/src/migrations.rs` and
    // `embed_migrations.rs`, `migrations_internals/src/lib.rs`). With no path it searches
    // `<package>/src/migrations`, then `migrations` in the package and each folder above it. It
    // lists the folder with `read_dir`, checks whether each `down.sql` exists and reads
    // `metadata.toml` with `std::fs`; only `up.sql` and `down.sql` are in an `include_str!`.
    Entry {
        externs: &["diesel_migrations"],
        env: &[],
        files: &[],
        folders: &[
            Where::Package("src/migrations"),
            Where::UpwardFromPackage("migrations"),
        ],
    },
    // `refinery::embed_migrations!` (`refinery_macros/src/lib.rs`): `<package>/migrations` by
    // default, walked by `refinery_core::find_migration_files`. Each file is in an
    // `include_str!`, the listing is not.
    Entry {
        externs: &["refinery", "refinery_macros"],
        env: &[],
        files: &[],
        folders: &[Where::Package("migrations")],
    },
    // `rust_i18n::i18n!` (`crates/macro/src/lib.rs`, `crates/support/src/lib.rs` and
    // `config.rs`): reads `[package.metadata.i18n]` from `<package>/Cargo.toml`, then every
    // `**/*.{yml,yaml,json,toml}` under the load path (`locales` by default) with `std::fs`, and
    // bakes the strings into the code. Nothing is tracked.
    Entry {
        externs: &["rust_i18n", "rust_i18n_macro"],
        env: &[],
        files: &[Where::Package("Cargo.toml")],
        folders: &[Where::Package("locales")],
    },
];

/// What to add for a compile whose direct `--extern` crates are `externs` (their names, not
/// paths). Places may overlap (the package can be the workspace); the wrapper takes each
/// resolved path once.
pub fn inputs(externs: &[&str]) -> Known {
    let mut known = Known::default();
    for entry in TABLE {
        if !entry.externs.iter().any(|name| externs.contains(name)) {
            continue;
        }
        known.env.extend_from_slice(entry.env);
        for place in entry.files {
            if !known.files.contains(place) {
                known.files.push(*place);
            }
        }
        for place in entry.folders {
            if !known.folders.contains(place) {
                known.folders.push(*place);
            }
        }
    }
    known.env.sort_unstable();
    known.env.dedup();
    known
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // covers: B-6
    fn sqlx_adds_its_variables_env_files_config_and_query_cache() {
        let known = inputs(&["serde", "sqlx", "tokio"]);
        assert_eq!(
            known.env,
            ["DATABASE_URL", "SQLX_OFFLINE", "SQLX_OFFLINE_DIR"]
        );
        for place in [
            Where::Package(".env"),
            Where::Workspace(".env"),
            Where::Upward(".env"),
            Where::Package("sqlx.toml"),
            Where::Workspace("sqlx.toml"),
        ] {
            assert!(known.files.contains(&place), "{place:?}");
        }
        for place in [
            Where::Package(".sqlx"),
            Where::Workspace(".sqlx"),
            Where::Package("migrations"),
        ] {
            assert!(known.folders.contains(&place), "{place:?}");
        }
    }

    #[test]
    // covers: B-6
    fn dotenv_macros_search_upward_from_the_working_directory() {
        for name in ["dotenvy_macro", "dotenv_codegen"] {
            let known = inputs(&[name]);
            assert_eq!(known.files, [Where::Upward(".env")]);
            assert!(known.env.is_empty());
            assert!(known.folders.is_empty());
        }
    }

    #[test]
    // covers: B-6
    fn migration_embedders_add_their_folder() {
        assert_eq!(
            inputs(&["refinery"]).folders,
            [Where::Package("migrations")]
        );
        let diesel = inputs(&["diesel", "diesel_migrations"]);
        assert!(
            diesel
                .folders
                .contains(&Where::UpwardFromPackage("migrations"))
        );
        assert!(diesel.folders.contains(&Where::Package("src/migrations")));
    }

    #[test]
    fn nothing_for_crates_not_in_the_table_or_spelled_differently() {
        assert_eq!(inputs(&[]), Known::default());
        assert_eq!(inputs(&["serde", "askama", "sqlx_core"]), Known::default());
        // Cargo passes `-` as `_`; the dashed spelling never reaches `--extern`.
        assert_eq!(inputs(&["dotenvy-macro"]), Known::default());
    }

    #[test]
    fn several_macros_merge_without_repeats() {
        let known = inputs(&["sqlx", "sqlx_macros", "dotenvy_macro", "refinery"]);
        let upward = known
            .files
            .iter()
            .filter(|w| **w == Where::Upward(".env"))
            .count();
        assert_eq!(upward, 1);
        let migrations = known
            .folders
            .iter()
            .filter(|w| **w == Where::Package("migrations"))
            .count();
        assert_eq!(migrations, 1);
        assert_eq!(known.env.len(), 3);
    }
}

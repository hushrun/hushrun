# 0155. Composer in the package store

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-4, K-5, K-6 and K-10 extended (`docs/spec/11-baked-packages.md`); K-7 read

## Context

ADR 0149 made the package store, and ADR 0150 its first seeds (pnpm and npm). A PHP sandbox's
setup script runs `composer install` before any check: every package downloaded again, from
Packagist's metadata and GitHub's archives. The plan for this stage said the `composer` part would
be a seed into Composer's files cache, read in place instead if a read-only cache could be set from
the environment, and verified by a signature where a lock's `shasum` is empty.

## Decision

1. **The part is Composer's archives**, `files/<vendor>/<name>/<sha1 of the URL>.<type>`, taken
   from `composer install --download-only --no-scripts --no-plugins` with `COMPOSER_CACHE_DIR` at a
   folder of the bake's, or harvested from a cache a job filled. The rest of the cache (`repo/`,
   the registry's metadata with its times; `vcs/`, clones; `.htaccess`) is left out.
2. **A seed, not in place.** Composer reads one cache folder. `COMPOSER_CACHE_READ_ONLY=1` with
   `COMPOSER_CACHE_DIR` at the part does install from it, but in place of the person's cache:
   their metadata, their other archives and anything Composer would cache in the run are gone. The
   plan's mechanism A needs a folder read beside the tool's own, and Composer has none.
3. **A signature verifies it.** A lock's `dist.shasum` is empty for every Packagist package we
   looked at (`psr/log` 3.0.2 and `symfony/polyfill-ctype` 1.37 among them), and Composer checks a
   cached archive against nothing then. Where a package has a `shasum`, the seed also checks the
   archive's SHA-1 against it, as Composer does, and leaves out one that differs; the part's class
   stays `signature`, since a lock with one `shasum` says nothing of the others.
4. **The seed copies what the lock names now**, by the package's name and the SHA-1 of its
   `dist.url`, the name Composer gives the archive. The SHA-1 is gix's collision-detecting one,
   already in the binary; an archive made to collide is never copied.
5. **The files cache is where Composer says**, `composer config cache-files-dir` in the checkout:
   it follows `COMPOSER_CACHE_DIR`, `COMPOSER_HOME`, the XDG folders and the person's
   configuration, which reading variables alone would get wrong.
6. **Nothing in a run.** As for the JavaScript parts, only `hushrun packages exec` and `restore`
   seed; a check runs after the install. The PHPUnit adapter's `PHP_INI_SCAN_DIR` and its reads of
   `vendor/` are untouched.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with Composer 2.8.12 and PHP 8.3.6, against a local server,
since this container's network refuses GitHub's archives:

- An archive downloaded from a URL is cached as `files/<name>/<sha1(url)>.<type>`, the same as
  `FileDownloader`'s key in Composer's source.
- `install --download-only` fills the cache and writes no `vendor/`.
- A cache holding only the copied `files/` installs, with the server stopped, and the `vendor/` it
  writes is byte for byte the network install's (`composer/installed.json` and the autoloader
  included). An empty cache with the server stopped fails.
- A lock's `shasum` that matches the cached archive loads it from the cache; one that doesn't
  makes Composer download again.

Not confirmed: Composer 1, other platforms, and a dist mirror (its rewritten URL gives another
name: that archive is left out, and downloaded).

## Considered and dropped

- **Read in place through `COMPOSER_CACHE_READ_ONLY`** (decision 2).
- **Keeping `repo/`, Packagist's metadata.** `composer install` from a lock doesn't read it, and it
  holds the registry's times.
- **Seeding `vendor/`.** `composer install` writes it, runs the project's scripts and its plugins,
  and writes the autoloader; a copied `vendor/` would skip what the person's command asks for.

## Consequences

- `hushrun packages exec -- composer install` in a sandbox downloads only what the store lacks.
- The plan's "lockfile where it isn't" doesn't apply: the class is the whole part's.

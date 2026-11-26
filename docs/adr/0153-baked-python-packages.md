# 0153. Baked Python packages: pip and Poetry, and no part for uv

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-10's `pip`, `poetry` and `uv` paragraphs and K-30 and K-31 new
  (`docs/spec/11-baked-packages.md`); K-3 to K-8 read

## Context

ADR 0149 gave a package store parts for Gradle and Maven, two tools that read a second,
read-only folder beside their own cache. A Python sandbox still downloads every wheel its
lockfile names. The three package managers asked for here, uv, pip and Poetry, read no second
folder, and each writes its lockfile's hashes, so a part of theirs can be checked against the
lockfile rather than trusted on a signature.

## Decision

1. **A `pip` part, read as a registry (K-10, B).** For requirements files with hashes only: the
   part is a flat folder of the files those hashes name, and pip reads it through
   `PIP_FIND_LINKS`. pip checks each file against the requirements file's hashes, so the part is
   `verified_by: lockfile` and any run reads it, signed or not. A bake fetches it with
   `python3 -m pip download --no-deps --require-hashes`, once per requirements file with hashes,
   with or without COMMAND (K-30): pip has a download-only command, and the check's own build
   would fill pip's HTTP cache, which holds no file in a form pip reads back.
2. **A `poetry` part, read by a seed (K-10, C; K-31).** The part is `artifacts/` of a Poetry
   cache, each file unchanged in the folder Poetry names by its URL and hash. Before COMMAND, a
   run copies each file `poetry.lock` still names, after checking the store's bytes against the
   lockfile's hash, into Poetry's own cache at the same path, and leaves a file already there as
   it is. Poetry checks every cached file against `poetry.lock` before it installs it. A bake
   fetches the part with COMMAND, `POETRY_CACHE_DIR` at a folder of its own; a wheel Poetry built
   from a source archive sits in the same folder, isn't named by the lockfile, and is left out
   (built wheels are a later part).
3. **No `uv` part.** Nothing uv reads can come from a store without rewriting `uv.lock` or
   writing uv's private cache format (below). The part is dropped, not deferred.
4. **A part keeps only what its lockfile hashes.** For a part `verified_by: lockfile`, a bake
   keeps a file only when the lockfile in the checkout names its SHA-256 (and its name, where the
   lockfile gives one), and writes that hash as the row's `locked`. A cache's other files are
   left out and counted.
5. **Where a reader would undo the person's own setting, it leaves the tool alone**, with a line:
   `PIP_FIND_LINKS` set, or `find-links` in a pip configuration file pip reads (the variable would
   replace it); a relative `POETRY_CACHE_DIR`, or `cache-dir` in `poetry.toml` or Poetry's
   `config.toml`, which hushrun doesn't read.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with uv 0.13.10, pip 26.2.2 and Poetry 2.5.2, a project of
three pure-Python wheels (`six`, `idna`, `attrs`):

- **uv.** There is no download-only command (`uv pip` has no `download`). A folder of the locked
  wheels named by `UV_FIND_LINKS` is not read by `uv sync`, offline, from an empty cache: uv
  fetches each package from the URL `uv.lock` names. The same folder as a PEP 503 index named by
  `UV_DEFAULT_INDEX` or `UV_INDEX_URL` installs offline but rewrites `uv.lock`; with
  `UV_FROZEN=1` it is ignored, and with `UV_LOCKED=1` the sync fails. uv's cache
  (`wheels-v8`, `archive-v0`) holds unpacked wheels and HTTP metadata in uv's own versioned
  format, not the files uv downloaded, so it is no download K-5 allows, and seeding it would be
  writing uv's private format.
- **pip.** A folder named by `PIP_FIND_LINKS`, read-only, installs a hashed requirements file
  offline from an empty cache. With the index reachable, pip downloads every file from the index
  anyway: two candidates with allowed hashes sort equal, and pip's resolver takes the last, which
  is the index's (pip's `factory.py` walks the sorted candidates in reverse). A find-links page
  whose links carry `#sha256=` changes nothing. So the part is what an install reads where the
  index can't be reached; online it saves nothing and costs nothing.
- **Poetry.** A cache holding only `artifacts/`, copied from a cache `poetry install` filled,
  installs with no file downloaded (control: seven download lines from an empty cache, none from
  the seeded one). A seeded file changed by one byte is refused (`Cannot install six.`), not
  downloaded again. Poetry still asks the index for each file's URL, so an install with no
  network fails as today.

Not confirmed here: other platforms; pip older than 26 and Poetry older than 2.5; source
archives (only wheels were installed).

## Considered and dropped

- **`uv` through `UV_DEFAULT_INDEX`:** it rewrites a tracked file, which changes every key that
  reads it.
- **Seeding uv's cache:** its layout is uv's own and versioned (`wheels-v8`); a uv of another
  version reads none of it, and its files aren't the downloads.
- **`PIP_NO_INDEX` or `--no-index` beside the pip part:** an offline switch, which a stale store
  turns into a failed install (K-10: hushrun sets none).
- **`POETRY_CACHE_DIR` at the store:** Poetry writes into its cache, and the variable also moves
  Poetry's virtualenvs (`{cache-dir}/virtualenvs`).
- **Seeding pip's HTTP cache:** a private format of pip's vendored CacheControl.

## Consequences

- An unsigned store's `pip` and `poetry` parts are read by any run, in CI too; a store with
  Gradle or Maven parts and no evidence is read for these parts alone, with one line (K-7).
- A run with a `poetry` part writes into Poetry's cache, once per file; `hushrun packages
  restore` does the same and says what it copied.
- A pip install with its index reachable downloads as before. The `pip` part serves sandboxes
  that can't reach the index, which today fail.

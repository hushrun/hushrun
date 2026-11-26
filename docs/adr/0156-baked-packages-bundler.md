# 0156. Baked packages for Bundler: a seed into the gem cache Bundler already uses

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-10 gains `bundler`'s paragraph; K-45 new (`docs/spec/11-baked-packages.md`); K-3,
  K-6 and K-7 read

## Context

ADR 0149 made a package store with parts for Gradle and Maven, both read in place (K-10's A).
A Ruby repository's sandbox still runs `bundle install`, which downloads every gem the
Gemfile.lock names. Bundler reads no second, read-only gem folder, and serving the store as a
registry (B) would need a process for every install. That leaves C: copy each gem into the cache
Bundler already looks in, before the tool runs.

The ADR for Ruby's test adapter (0114) owns `RUBYLIB` and `RUBYOPT` in a run. A reader must not
set them, and no reader sets `GEM_PATH`, which would change which gems a run loads.

## Decision

1. **`bundler` is C, a seed.** A part holds the `.gem` files directly in a gem cache, as
   `cache/<name>-<version>[-<platform>].gem`. Before COMMAND starts, a run copies each gem the
   lockfile names into `<Bundler.bundle_path>/cache`: the folder where Bundler's `fetch_gem`
   looks before it downloads, and where it would have saved the download. Gems therefore install
   to the same paths as they do today. hushrun sets no variable: not `GEM_HOME`, `GEM_PATH`,
   `BUNDLE_PATH`, `RUBYOPT` or `RUBYLIB`.
2. **Bundler says where its path is.** That path follows `BUNDLE_PATH`, `.bundle/config`,
   deployment, `path.system` and `GEM_HOME`. So the run asks the `ruby` on `PATH`, from the
   Gemfile's folder (`require "bundler"; Bundler.bundle_path`), with `RUBYOPT` unset so that a
   `-rbundler/setup` there doesn't need the gems the seed is for. The question is asked only when
   the store has a `bundler` part the run reads and a Gemfile.lock names a gem. When Bundler's
   global gem cache is on, the part is left alone with a line, since Bundler then looks only
   under `~/.bundle/cache/gems/<source>`.
3. **A lockfile verifies the part only where it has `CHECKSUMS`.** When every lockfile of the
   manifest set has a `CHECKSUMS` section, the part is verified by `lockfile`: it holds only the
   files those lockfiles hash, and each row's `locked` is the lockfile's `sha256=<hex>`. A run
   seeds a file only when the lockfile in the checkout gives it that same hash, and needs no
   signature. Otherwise the part is verified by `signature`, as Gradle's and Maven's are.
4. **A seed never writes over a file and never copies a changed one (K-45).** Each file is copied
   through a file of the seed's own in the cache folder and hashed as it goes. It is linked into
   place only when that hash is its row's, and never over a file already there. A file the
   lockfile hashes differently is never copied: Bundler would refuse it with a "mismatched
   checksums" error instead of downloading it again, so a stale seed would break an install
   that passes today.
5. **The bake's fetch runs with `BUNDLE_PATH` at a folder of its own** and
   `BUNDLE_GLOBAL_GEM_CACHE=false`, as it does Gradle's user home and Maven's repository.
   `--harvest bundler=FOLDER` takes a gem home (`cache/`) or a `BUNDLE_PATH`
   (`ruby/<version>/cache/`).
6. **The seed is generic**, one function (`hushrun_packages::seed::seed`) and one reader
   (`Reader::Seed`), so that any other part that is C uses the same checks.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with Ruby 3.3.6 and Bundler 4.0.18:

- **Bundler 4 writes `CHECKSUMS` in every new lockfile**, as `name (version) sha256=<hex>`, the
  SHA-256 of the `.gem` file as downloaded. It writes them for a `file://` source too. It doesn't
  add them to a lockfile that has none.
- **A seeded cache is used.** `.gem` files copied into `<BUNDLE_PATH>/ruby/3.3.0/cache` are
  installed by `bundle install` without a `Fetching` line. With one of two gems seeded, only the
  other is fetched. With the source's gems removed, the seeded install passes and the same
  install without the seed fails (`packages_bundler.rs`).
- **Bundler still fetches the index.** With all gems seeded and no network, `bundle install`
  fails at "Could not reach host index.rubygems.org". So the seed saves the downloads, not the
  index request; hushrun sets no offline switch, so this is expected (ADR 0149).
- **`bundle install --local` doesn't read the seed.** In Bundler 4 it builds its index from
  `vendor/cache` alone, so the person's `--local` doesn't make a seeded install offline.
- **Bundler checks a cached gem against `CHECKSUMS`.** A `.gem` under the right name with other
  bytes fails the install with "Bundler found mismatched checksums". Hence decision 4.
- **The global gem cache** (`global_gem_cache`) makes `fetch_gem` look only under
  `~/.bundle/cache/gems/<source>` (Bundler 4.0.18's `source/rubygems.rb`). Hence the line in
  decision 2.

Not confirmed here: Bundler 2.x (2.5 writes `CHECKSUMS` only with `bundle lock
--add-checksums`), macOS, and a gem with native extensions. The part holds only the `.gem`, so
an extension is still compiled at install; that is a `bundler-built` part's job.

## Considered and dropped

- **`GEM_PATH` naming the part:** Bundler resets the gem path to its own, and a run's `GEM_PATH`
  would change which gems Ruby loads.
- **Seeding `vendor/cache`** (Bundler's app cache, which `--local` reads): it is inside the
  checkout, and when it exists `bundle install` writes every gem into it, changing the
  checkout.
- **Seeding the global gem cache:** its folder name comes from the source's URL. A part that
  served one source would seed under another source's name.
- **The lockfile class for every lockfile:** without `CHECKSUMS`, the lockfile hashes nothing.

## Consequences

- A run whose store has a `bundler` part starts one `ruby` process, before COMMAND, when a
  Gemfile.lock names a gem.
- A seeded gem is a file in Bundler's cache, as a downloaded one would be. A run never removes
  it, and the next install finds it.
- A sandbox image that carries the store and runs `bundle install` downloads only the gems the
  store lacks.

# 0156. Baked packages for Bundler: a seed into the gem cache Bundler already uses

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-45 new, the `bundler` part (`docs/spec/11-baked-packages.md`); K-3, K-6, K-7 and
  K-10 read; the seed is ADR 0150's

## Context

ADR 0149 made a package store with parts for Gradle and Maven, both read in place (K-10's A).
A Ruby repository's sandbox still runs `bundle install`, which downloads every gem the
Gemfile.lock names. Bundler reads no second, read-only gem folder (see "Considered and dropped"
for `BUNDLE_CACHE_PATH`), and serving the store as a registry (B) would need a process for every
install. That leaves C: put each gem into the cache Bundler already looks in, before the tool
runs.

The ADR for Ruby's test adapter (0114) owns `RUBYLIB` and `RUBYOPT` in a run, and its reads
count what code outside the repository loaded, by path. A reader must not set those variables,
no reader sets `GEM_PATH`, and a gem must load from the same path with and without the store.

## Decision

1. **`bundler` is C, a seed.** A part holds the `.gem` files directly in a gem cache, as
   `cache/<name>-<version>[-<platform>].gem`. `hushrun packages exec` and `restore` copy each
   gem the lockfile names into `<Bundler.bundle_path>/cache`, before the install;
   `hushrun run` gives COMMAND nothing for it, since by then the install is done (ADR 0150). That
   folder is where Bundler's `fetch_gem` looks before it downloads, and where it would have saved
   the download. Gems therefore install
   to the same paths as they do today, and load from the same paths. hushrun sets no variable:
   not `GEM_HOME`, `GEM_PATH`, `BUNDLE_PATH`, `BUNDLE_CACHE_PATH`, `RUBYOPT`, `RUBYLIB` or
   Bootsnap's, whose cache therefore stays as the person has it.
2. **Bundler says where its path is.** That path follows `BUNDLE_PATH`, `.bundle/config`,
   deployment, `path.system` and `GEM_HOME`. So the seed asks the `ruby` on `PATH`, from the
   Gemfile's folder (`require "bundler"; Bundler.bundle_path`), with `RUBYOPT` unset so that a
   `-rbundler/setup` there doesn't need the gems the seed is for. When Bundler's global gem cache
   is on, the part is left alone with a line, since Bundler then looks only under
   `~/.bundle/cache/gems/<source>`.
3. **A lockfile verifies the part only where it has `CHECKSUMS`.** When every lockfile of the
   manifest set has a `CHECKSUMS` section, the part is verified by `lockfile`: it holds only the
   files those lockfiles hash, and each row's `locked` is the lockfile's `sha256=<hex>`. A run
   seeds a file only when the lockfile in the checkout gives it that same hash, and needs no
   signature. Otherwise the part is verified by `signature`, as Gradle's and Maven's are.
4. **The seed is ADR 0150's.** It copies each file, hashed as it is read and checked against its
   row, written beside its place and renamed, never over a file already there, and leaves a stamp
   (`.hushrun-packages-<part identity>`) when it left nothing out. The part says what it admits: a
   gem the `GEM` section of a lockfile in the checkout names now and, where its `CHECKSUMS` hashes
   it, only when the SHA-256 of its bytes is that hash. A gem named with no hash is admitted only
   for a part a signature verifies (`Part::admit_for`, given the part's `verified_by`). A file the
   lockfile hashes differently is never copied: Bundler would refuse it with a "mismatched
   checksums" error instead of downloading it again, so a stale seed would break an install that
   passes today.
5. **The part fetches itself with `bundle cache --no-install`**, Bundler's own download (K-32,
   ADR 0153's mechanism): `bundle cache --no-install --gemfile <Gemfile> --cache-path
   <part>/cache` for each lockfile of the manifest set, with or without COMMAND. It downloads each
   gem and installs none, so no gem's install script runs in a bake, and the absolute cache path
   keeps it out of `vendor/cache` in the checkout. The download and COMMAND get `BUNDLE_PATH` at a
   folder of the bake's own that the part never takes from, `BUNDLE_FROZEN=true` (the lockfile
   stays as the checkout has it; `--frozen` is gone from `bundle cache` in Bundler 4) and
   `BUNDLE_GLOBAL_GEM_CACHE=false`. `--harvest bundler=FOLDER` takes a gem home (`cache/`) or a
   `BUNDLE_PATH` (`ruby/<version>/cache/`) instead.
6. **Additive to the shared code**, each with a default that keeps every other part as it was:
   `Part::verified_by_for` (a class that follows the lockfiles' text), `Part::admit_for` (`admit`
   given the part's `verified_by`) and `Locked::add_as` (a hash in the lockfile's own words,
   Bundler's `sha256=<hex>`). The lockfile hashes go through ADR 0153's `Part::lockfile`, which
   also keeps a lockfile part to the files they hash.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with Ruby 3.3.6 and Bundler 4.0.18:

- **Bundler 4 writes `CHECKSUMS` in every new lockfile**, as `name (version) sha256=<hex>`, the
  SHA-256 of the `.gem` file as downloaded. It writes them for a `file://` source too. It doesn't
  add them to a lockfile that has none.
- **A seeded cache is used.** `.gem` files put into `<BUNDLE_PATH>/ruby/3.3.0/cache` are
  installed by `bundle install` without a `Fetching` line. With one of two gems seeded, only the
  other is fetched. With the source's gems removed, the seeded install passes, and the same
  install without the seed fails. A gem added to the lockfile after the bake is fetched while
  the others are seeded. A seeded gem loads from the same path as a downloaded one
  (`packages_bundler.rs`).
- **Bundler still fetches the index.** With all gems seeded and no network, `bundle install`
  fails at "Could not reach host index.rubygems.org". So the seed saves the downloads, not the
  index request. hushrun sets no offline switch, so this is expected (ADR 0149). The tests take
  the network away by removing the gems from a `file://` source and keeping its index.
- **`bundle install --local` doesn't read the seed.** In Bundler 4 it builds its index from the
  app cache alone, so the person's `--local` doesn't make a seeded install offline.
- **Bundler checks a cached gem against `CHECKSUMS`.** A `.gem` under the right name with other
  bytes fails the install with "Bundler found mismatched checksums". Hence decision 4.
- **`bundle cache --no-install` installs nothing.** With an absolute `BUNDLE_CACHE_PATH` it puts
  the `.gem` files there and in `<BUNDLE_PATH>/ruby/<version>/cache`, creates no `gems/` folder,
  and writes nothing into the checkout. A gem whose install script writes a file doesn't write it
  during the bake, and does during a plain `bundle install` (`no_gems_install_script_runs_in_a_bake`).
- **The global gem cache is off by default**, and when on, `fetch_gem` looks only under
  `~/.bundle/cache/gems/<source>` (Bundler 4.0.18's `source/rubygems.rb`).

Not confirmed here: Bundler 2.x (2.5 writes `CHECKSUMS` only with `bundle lock
--add-checksums`), macOS, and a gem with a compiled extension. The part holds only the `.gem`,
so an extension is still compiled at install; that is a `bundler-built` part's job (Phase 4).

## Where this differs from the plan

- **The plan's reader is "C, into the global gem cache".** That cache is off by default in
  Bundler 4.0.18, and Bundler looks only there when it is on. Seeding it would do nothing for
  most repositories, and its folder name comes from the source's URL. The seed goes into the
  cache Bundler uses (`<Bundler.bundle_path>/cache`), and the part is left alone, with a line,
  where the global cache is on.
- **The plan's "A if `BUNDLE_CACHE_PATH` may be absolute":** it may. Bundler reads `.gem` files
  from an absolute `BUNDLE_CACHE_PATH`, and an install from a read-only one passes. But with a
  stale store, `bundle install` downloads the missing gem and then writes it into that folder
  ("Updating files in …"). The write succeeded here only because the container runs as root; for
  any other user it fails the install, and either way it writes into the store, which K-10's
  "Breaks if" forbids. So A is dropped.
- **"Link, else copy":** the seed is ADR 0150's, which copies. A link would share one file
  between the store and Bundler's cache, so a tool or a person writing either in place would
  change both. Only one seed is kept in the repository, so this part doesn't add its own.
- **The keys test is in `packages_bundler.rs`, not `units_ruby.rs`.** It checks the path the
  adapter's reads are made of: Ruby loads `tiny.rb` from the same file whether the gem was
  downloaded or seeded. `units_ruby.rs` runs on the adapter's own gem folders, which have no
  Gemfile.

## Considered and dropped

- **`GEM_PATH` naming the part:** Bundler resets the gem path to its own, and a run's `GEM_PATH`
  would change which gems Ruby loads.
- **Seeding `vendor/cache`** (Bundler's app cache, which `--local` reads): it is inside the
  checkout, and when it exists `bundle install` writes every gem into it, changing the
  checkout.
- **A, `BUNDLE_CACHE_PATH` at the part:** see above.
- **Taking the part from COMMAND's own `bundle install`**, as Gradle's and Maven's are taken:
  it runs every gem's install script in the bake, and a script can write into the cache the part
  is taken from.
- **The lockfile class for every lockfile:** without `CHECKSUMS`, the lockfile hashes nothing.

## Consequences

- A seed of a `bundler` part starts one `ruby` process, to find Bundler's path.
- A seeded gem is a file in Bundler's cache, as a downloaded one would be. Nothing removes it, and
  the next install finds it.
- A bake of a Ruby repository needs `bundle` on `PATH`, and no COMMAND.
- A sandbox image that carries the store and runs `bundle install` downloads only the gems the
  store lacks.

# 0156. Baked packages for Bundler: a seed into the gem cache Bundler already uses

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-10 gains `bundler`'s paragraph; K-45 new (`docs/spec/11-baked-packages.md`); K-3,
  K-6 and K-7 read

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
   `cache/<name>-<version>[-<platform>].gem`. Before COMMAND starts, a run puts each gem the
   lockfile names into `<Bundler.bundle_path>/cache`: the folder where Bundler's `fetch_gem`
   looks before it downloads, and where it would have saved the download. Gems therefore install
   to the same paths as they do today, and load from the same paths. hushrun sets no variable:
   not `GEM_HOME`, `GEM_PATH`, `BUNDLE_PATH`, `BUNDLE_CACHE_PATH`, `RUBYOPT`, `RUBYLIB` or
   Bootsnap's, whose cache therefore stays as the person has it.
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
4. **A seed links, else copies, and checks every file (K-45).** Each file is hashed as it is
   read. It is hard-linked from the store where the file system allows; otherwise it is copied
   through a file of the seed's own and hashed again as it is written. It is linked into place
   only when the hash is its row's, and never over a file already there. A file the lockfile
   hashes differently is never put there: Bundler would refuse it with a "mismatched checksums"
   error instead of downloading it again, so a stale seed would break an install that passes
   today. A seed that left nothing out leaves a stamp, `.hushrun-packages-<store identity>`,
   holding a digest of the files it was asked for; the next seed of the same files reads it and
   stops there.
5. **The part fetches itself with `bundle cache --no-install`**, for each lockfile of the
   manifest set, run as a bake runs COMMAND, with or without COMMAND. It downloads each gem and
   installs none, so no gem's install script runs in a bake. `BUNDLE_PATH` and an absolute
   `BUNDLE_CACHE_PATH` are the bake's own folders, so nothing is written into the checkout;
   `BUNDLE_FROZEN=true` keeps the lockfile as it is, and the global gem cache is off. COMMAND gets
   no Bundler variable. `--harvest bundler=FOLDER` takes a gem home (`cache/`) or a `BUNDLE_PATH`
   (`ruby/<version>/cache/`) instead.
6. **One seed function** (`hushrun_packages::seed::seed`) and one reader (`Reader::Seed`) serve
   any part that is C. PR #76 (Cargo and Go) adds a seed too: whichever merges second folds its
   copy into the other's, so the repository keeps one.

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
- **The stamp holds a digest of the files asked for**, not only the store's name. With the
  store's name alone, a lockfile changed by one gem the store holds would never be seeded. A
  second seed costs one small read, not one `stat`. The `ruby` question of decision 2 comes
  before it and costs more than either.
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

- A run whose store has a `bundler` part starts one `ruby` process, before COMMAND, when a
  Gemfile.lock names a gem.
- A seeded gem is a file in Bundler's cache, as a downloaded one would be, or a hard link to the
  store's. A run never removes it, and the next install finds it. A tool that rewrote a linked
  file in place would change the store's too; K-1's check by sizes or K-8's hash finds that, and
  Bundler checks the file against `CHECKSUMS` where the part is verified by the lockfile.
- A bake of a Ruby repository needs `bundle` on `PATH`, and no COMMAND.
- A sandbox image that carries the store and runs `bundle install` downloads only the gems the
  store lacks.

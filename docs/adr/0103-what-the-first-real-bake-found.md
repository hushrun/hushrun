# 0103. What the first real bake found: a bake of unchanged code repeats, and one answer to "in CI"

- **Status:** proposed; built
- **Date:** 2026-11-22
- **Rules:** B-2, B-37, B-63, B-67 and B-72 changed; B-7, B-25, B-30, B-31, B-65 and B-66 read

## Context

The first bake outside the tests ran in GitHub Actions, in a small public repository of 33
third-party crates, publishing to GitHub's container registry and signing keylessly through
public-good Sigstore, with no backend (`HUSHRUN_BACKEND=""`). It worked, and it found four things
the tests didn't:

1. **A CI job with no backend had nothing to bake, and was told the wrong thing.**
   `HUSHRUN_BUILD=on hushrun run build -- cargo build` reused and kept nothing and said nothing;
   the `hushrun build bake --from store` that followed exited 2 with "run the build through
   `hushrun run NAME -- COMMAND` first, with build reuse on", which is what the job had just
   done. Keeping nothing is B-25's "CI otherwise" row and [ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md)'s
   first decision ("CI is unchanged unless the org shares"); the silence and the advice were not
   decided anywhere.
2. **Two places disagreed about being in CI.** In a job told it runs outside CI
   (`HUSHRUN_PROVIDER=local`), the bake said `-latest` moves only from CI and left it alone, and
   signed keylessly as the workflow all the same: the signer took any ID token at hand, while the
   line about publishing unsigned said that outside CI only a key signs.
3. **B-67's keyless cosign command failed after ten minutes.** Without
   `--use-signed-timestamps`, cosign checks the Fulcio certificate against the time now, and the
   certificate lasts ten minutes: `failed to verify leaf certificate`. With it, cosign checks the
   certificate at the bundle's RFC 3161 timestamp, in CI and on a laptop (cosign 3.1.4).
   hushrun's own check reads the timestamp and was right all along.
4. **A bake of unchanged code never repeated.** Across five bakes of the same commit, every pack
   was byte-identical, but 9 of the 36 dependency hints had new bytes each time (the seven build
   scripts' programs and the two proc macros), so the dependencies layer (24 MB) never repeated
   and every bake uploaded it again, against B-65's promise that it changes only with the
   lockfile, the toolchain or the flags. And the store's identity, with the fixed tag
   `hushrun-<platform>-<16 hex>` that B-66 says never moves, was new at every bake.

## Decision

1. **A linked output's hint names rustc's temporary folder by one name** (B-37). The hint keeps
   the link's question for its dry run (the line without inputs and output), and that line holds
   `<out-dir>/rustcXXXXXX`, the folder rustc links in under a new random name at every link: the
   symbol list `-exported_symbols_list` hands Apple's linker for a proc macro, and on Linux
   `-L <out-dir>/rustcXXXXXX/raw-dylibs`, which every link of a program or a proc macro gets. E
   already read the name fixed, so the keys repeated and the packs with them; the hint didn't.
   Now the hint writes `{hushrun:out-dir}/rustcXXXXXX` (`link::fixed_rustc_temp`), only for a
   folder directly in the output folder, as `link::under_rustc_temp` counts one. Nothing a lookup
   does changes: the folder is gone once rustc ends, so the dry run never found it under either
   name, and E normalizes the text it prints. A hint written before still serves.
2. **A manifest's identity is its set of pairs, not their order** (B-63). It hashed the pairs in
   the order compiles ended, which cargo's parallel build changes from one build to the next. It
   is now the SHA-256 of `hushrun.block-manifest/v2`, the kind and each distinct pair, sorted
   (`baked::manifest_id`, the one function the bake, `hushrun build manifest` and the check of a
   replaced store read). The order stays in the manifest, for the prefetch (B-30). `baked.json`'s
   rows are sorted by kind, then address, in the one place every baked folder is written
   (`build_relay::baked::write_whole`) and in the image's own copy, so the same store bakes the
   same bytes.
3. **"In CI" is one answer, read by both** (B-67). The bake reads it once (`gitlab_job::in_ci`)
   and hands it to the one function that decides what signs, `App::bake_signing`, which returns
   the signer or the reason there is none, and the bake prints that reason. Outside CI an ID token
   at hand signs only where `HUSHRUN_SIGNING=sigstore` asks for it, the variable that already
   chooses keyless signing for a run's events; the key file and the key variables sign anywhere,
   as before.
4. **A CI job with no backend says why it keeps nothing, and its bake says what works there**
   (B-2, B-63). B-25's row is not changed. `build_session::ci_without_backend` is the one test
   and the one text: `hushrun run` says it in one line when the job asked for reuse
   (`HUSHRUN_BUILD` `on` or `observe`; a job that asked nothing hears nothing, as B-2 wants of a
   job whose org doesn't share), `hushrun build status` on its `ci` line in place of "no backend
   is signed in to", and a bake or `hushrun build manifest` of the job's store refuses with it,
   exit 2, naming `--deps-only -- COMMAND` and, with a backend that shares, `--from store` and
   `--from backend`.
5. **The keyless cosign commands take `--use-signed-timestamps`** (B-67), for the image's
   attestation and for `verify-blob-attestation` of `baked.sigstore.json`. The key's command is
   unchanged: a key's bundle holds no timestamp unless `HUSHRUN_TSA` names an authority.
6. **A dependencies-only bake says nothing of a workspace layer** (B-72): its image has none, and
   it said the workspace layer was written again, as there was no previous image. What a bake
   says of the layer is decided from the image (`bake_image::rewritten`).

## Consequences

- Two builds of the same code bake the same dependencies layer, workspace layer, store identity
  and fixed tag; a bake over an image of the same lockfile uploads no dependencies layer. The
  recent layer holds `baked.json`, whose rows carry each compile's measured time, and changes with
  every bake as B-65 says.
- **The identity changes once.** Every store's identity, and so its image's fixed tag, is new
  after this version, once; a store baked by an older version compares as replaced the first time
  a backend's manifest is checked against it, and is baked again. A backend that computes a
  manifest's identity itself (for a webhook's payload, or to tell a changed manifest from an
  unchanged one) needs the same definition, or it reports a change at every build.
- A job told it runs outside CI no longer signs with the job's identity unasked. A workflow that
  relied on that sets `HUSHRUN_SIGNING=sigstore`, or bakes in CI.
- A CI job with no backend still keeps nothing, so a full store can't be baked there:
  `--deps-only` can, and a backend that shares build blocks gives `--from store` the job's runs.

## Alternatives considered

- **Keep a CI job's blocks in its own store with no backend**, so that a `--from store` bake later
  in the job works. The store would stay untrusted and go with the job. Not done: B-25 and ADR
  0079 decided that CI is unchanged unless the org shares, and turning the wrapper on for every
  CI job that names no backend changes every such job's builds. It is the owner's to decide, with
  the trade-off: one full bake per job with no backend, against compiles through the wrapper (and
  its cost) in every such job, most of which never bake.
- **Sort the saved manifest itself.** Rejected: the order is the prefetch's, libraries in the
  order a build needs them, and a backend keeps it as sent.
- **Leave the dry run's line out of the hint.** Rejected: a lookup computes E from it.
- **Never sign keylessly outside CI.** Rejected: `HUSHRUN_SIGNING=sigstore` already chooses keyless
  signing for events anywhere, and a test, and a person, may want a keylessly signed bake of a
  store built outside CI. Unasked, it never happens.

## Measurement

On a Mac (arm64, Rust 1.94.0), the same repository's sources (33 third-party crates, 38
compiles), each round in a fresh store, cache directory and target folder, `-j 8`, `cargo build
--release --locked` through `hushrun run` with reuse on, then `hushrun build bake --out`. Before:
two rounds gave two manifests and 2 of 37 hints with other bytes (the two proc macros, the
temporary folder in `-exported_symbols_list`'s argument their only difference). After: the same
manifest, and all 37 packs and 37 hints byte-identical.

The same rounds on Linux (`rust:1.94.0-bookworm`, arm64, a container, a git repository of the
same files), with this change's parent and with this change: before, two manifests and 10 of 36
first keys with other hint bytes, and in every one of those hints the only difference was the
folder's name in the dry run's line (`{hushrun:out-dir}/rustcXXXXXX/raw-dylibs` after `-L`, and
for the two proc macros `-Wl,--version-script={hushrun:out-dir}/rustcXXXXXX/list` too); after,
one manifest, and all 36 packs and the hints of all 36 first keys byte-identical.

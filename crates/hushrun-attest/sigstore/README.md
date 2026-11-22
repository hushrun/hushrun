# The public-good Sigstore trust root, vendored

hushrun verifies keyless signatures and RFC 3161 timestamps offline (ADR 0013). These files are
compiled into the binary:

- `trusted_root.json`: the public-good trusted root, as Sigstore's TUF repository published it
  (target `trusted_root.json` of `targets.json` version 14, sha256
  `6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66`).
- `targets.json`: that `targets.json`, signed by the TUF root's targets keys. It expires on
  2036-05-09.
- `root.json`: version 16 of the TUF root (`https://tuf-repo-cdn.sigstore.dev/16.root.json`),
  vendored on 2026-11-22 and expiring on 2027-04-22. It verifies `targets.json`, and
  `hushrun sigstore refresh` starts its updates from it. Version 15, vendored before it, is
  `testdata/sigstore/15.root.json`.

Tests check that `root.json` signs `targets.json`, that `targets.json` pins `trusted_root.json`,
that the root vendored before signed `root.json`, and that `root.json` hadn't expired on the day
it was vendored.

To update the snapshot, in one reviewed change, on a machine with the network:

1. `HUSHRUN_CACHE_DIR=$(mktemp -d) hushrun sigstore refresh --format json`, and note its
   `rootVersion`, `targetsVersion` and `trustedRoot` (the trusted root's SHA-256). Or fetch from
   `https://tuf-repo-cdn.sigstore.dev/`: `<N>.root.json` for each N after `root.json`'s version
   until one answers 404, then `timestamp.json`, the `<S>.snapshot.json` it names, the
   `<V>.targets.json` that names, and `targets/<sha256>.trusted_root.json` as that names it; and
   verify them with `tuf::Root::verify_next` and `verify_role` and `Target::check`, as the refresh
   does.
2. From the set the file `current` names in `$HUSHRUN_CACHE_DIR/sigstore/tuf-repo-cdn.sigstore.dev/`,
   copy `targets.json` and `trusted_root.json` over the files here, and the highest `root/<N>.json`
   over `root.json`. Move the `root.json` it replaces to `testdata/sigstore/<version>.root.json`,
   in place of the one there.
3. Write the new versions, dates and SHA-256 here, and the versions and the vendoring date into
   the tests in `src/tuf/tests.rs`.
4. `cargo test -p hushrun-attest`, then the workspace's tests: a run's keyless checks read
   `trusted_root.json`, and the cache in `hushrun` compares a cached `targets.json`'s version with
   the one here.

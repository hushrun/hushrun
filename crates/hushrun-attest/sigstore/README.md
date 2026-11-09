# The public-good Sigstore trust root, vendored

hushrun verifies keyless signatures and RFC 3161 timestamps offline (ADR 0013). These files are
compiled into the binary:

- `trusted_root.json`: the public-good trusted root, as Sigstore's TUF repository published it
  (target `trusted_root.json` of `targets.json` version 14, sha256
  `6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66`).
- `targets.json`: that `targets.json`, signed by the TUF root's targets keys.
- `root.json`: version 15 of the TUF root (`https://tuf-repo-cdn.sigstore.dev/15.root.json`). It
  verifies `targets.json`, and `hushrun sigstore refresh` starts its updates from it.

A test checks that `root.json` signs `targets.json` and that `targets.json` pins
`trusted_root.json`. To update the snapshot, run `hushrun sigstore refresh` and copy the files it
writes under `$HUSHRUN_CACHE_DIR/sigstore/`, or fetch them from the TUF repository, in one
reviewed change.

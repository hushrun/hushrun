# Sigstore fixtures

`fulcio-sigstore-js-2024.der` is a signing certificate that public-good Fulcio issued on
2024-10-14 to the release workflow of `sigstore/sigstore-js` (from the npm provenance bundle of
`sigstore@3.0.0`, at `https://registry.npmjs.org/-/npm/v1/attestations/sigstore@3.0.0`). It is
public, logged in Sigstore's CT log, and expired ten minutes after it was issued. Tests read its
identity and verify its chain and SCT against the public-good trust root, as hushrun reads
certificates it didn't make.

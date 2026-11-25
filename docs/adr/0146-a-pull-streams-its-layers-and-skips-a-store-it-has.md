# 0146. A pull streams its layers to disk and downloads nothing for a store it has

- **Status:** proposed; built
- **Date:** 2026-11-25
- **Rules:** B-240 changed; B-63, B-67 read

## Context

`hushrun build pull` (ADR 0143) read each layer into memory whole, up to 8 GiB, before it unpacked
anything, and downloaded every layer on every call. A sandbox whose setup pulls at each session
start, as the Claude Code recipe does, downloads the same store again each time. Its line gave the
digest cut to 12 hex digits, it had no way to say what a tag names without downloading, and it had
no JSON for a setup script to read. It had never read a real registry: only a fake one in-process.

## Decision

1. **Layers go to disk as they arrive.** Each layer is written to a file in a folder beside the
   destination while it is hashed, checked against its digest and refused past 8 GiB, then
   unpacked from the file an entry at a time, and the file removed. A layer is never held in
   memory; the disk holds one layer and the unpacked store at most.
2. **A store the pull put there is kept.** After a swap, the pull writes `.<name>.pulled` beside
   the destination: the image, the tag, the manifest's digest and the digest of the store's
   `baked.json`. A later pull reads the manifest only, and when the tag still names that digest,
   `baked.json` is the one it put there, the folder still checks (B-63, names and bytes) and its
   signature still verifies when `hushrun.yaml` names signers (B-67), it downloads nothing and says
   `hushrun: <folder> holds <image>:<tag> (<digest>) already, so nothing was downloaded: …`.
   Anything else pulls again, as before.
3. **The whole digest**, `sha256:` and 64 hex digits, in every line.
4. **`--resolve`** reads the manifest and prints `<image>@<digest>`, downloading and changing
   nothing.
5. **`--format json`** prints `hushrun.build-pull/v1`: `outcome` (`pulled`, `unchanged`,
   `resolved`), `image`, `tag`, `digest`, `reference`, `to` and `trust` (what the line says after
   its colon; `null` for `resolved`).
6. **The round trip is tested** against an image layout served over HTTP in every test run, and
   against Distribution and zot in CI's registries job: a bake pushes, a pull reads back, and a
   fresh machine reuses the store on its signature.

## Consequences

- A second pull in an unchanged sandbox costs one manifest request.
- The record is a convenience, not a trust: each run still reads `baked.json` and decides what it
  trusts as it begins (B-63, B-64). A record someone edits to match only skips a download of a store
  that checks out and, with signers, verifies.
- Pulling needs free disk for one layer beside the unpacked store, where it needed memory for all.
- `hushrun.build-pull/v1` is a contract: it only grows.

## Alternatives considered

- **Comparing the store's files with the image's layers** to skip a download: the layers must be
  downloaded to compare.
- **Keeping the record inside the store:** a baked store holds `baked.json`, packs and hints and
  nothing else (B-63).
- **Unpacking straight from the network stream:** a layer that fails its digest at its end would
  have been unpacked already; written first, nothing of it is unpacked until it checks.

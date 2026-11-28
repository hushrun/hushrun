# 0163. A package store published as an image and pulled into a sandbox

- **Status:** proposed; built (the CLI). The backend's registry doesn't take one yet: see
  "What the backend needs".
- **Date:** 2026-11-28
- **Rules:** K-14 and K-15 new; C-168 changed; relies on K-1, K-6, K-7, K-11, B-66, B-240 and C-13
- **Amends:** [0161](0161-a-session-start-that-warms-and-tells.md)

## Context

A package store (ADRs 0149 to 0159) reaches a sandbox only through a Dockerfile `COPY` from an
image holding `/hushrun-packages`, or by someone unpacking `hushrun packages bake --tar`'s archive
there. Claude Code on the web and the other hosted sandboxes with no Dockerfile have neither, so
every session downloads every package again. Compiled code had the same gap, and `hushrun build
pull` (ADR 0143, B-240) closed it: CI publishes an image, and a session's start pulls it. Nothing
publishes a package store to a registry, and nothing pulls one. `hushrun hook session-start` (ADR
0161) left a slot for this.

## Decision

1. **A separate image, in the same repository, under its own tags (K-14).** CI publishes the store
   as an image of the repository's own name (`<host>/<org>/<name>`, B-70's naming), tagged
   `hushrun-packages-<platform>-<first 16 hex of packages.json's identity>`, which never moves,
   and `hushrun-packages-<platform>-latest`, which moves only from CI on the default branch, by
   B-66's rule (`moves_latest`). No baked store's platform starts with `packages-`, so these tags
   never collide with `hushrun-<platform>-…`.
2. **One layer: the archive `packages bake --tar` writes, byte for byte.** That archive is already
   deterministic (K-6: sorted entries, fixed modes, owner and time 0), so the same store is the same
   layer digest, and a registry that holds it uploads nothing again. The config carries the label
   `dev.hushrun.baked-packages` (`hushrun.packages-image/v1`: identity, platform, the SHA-256 of
   `packages.json`, the parts), and the manifest the annotation `dev.hushrun.packages-identity`.
3. **The signature is the store's own `packages.sigstore.json`, inside the layer.** It covers
   `packages.json`, which covers every rows file, which covers every byte (K-7). An image referrer
   would sign the same thing again. Keyless signatures differ each time, so the same store signed
   again is another image: its fixed tag stays on the first one, and the moving tag goes to the new
   one.
4. **Publishing: `hushrun packages bake --oci REPOSITORY` or `--oci-dir DIR`,** next to `--out`
   and `--tar`, either of which it can go without. The registry's credential is settled before the
   fetch runs anything of the repository's, as `build bake --oci` does (ADR 0102): the CI job's own,
   else docker's. A store that is already baked (K-6's nothing to do) is published as it is.
   `--oci hushrun`, or a repository on the backend's registry host, is refused with a line that
   names the alternative (see "What the backend needs").
5. **`hushrun packages pull` (K-15) is `build pull` with a package store,** and shares its code
   (`build_pull::{image_and_credential, Source, Unpack, untar, swap}`), not a copy of it:
   - the image is `--image`, else `HUSHRUN_PACKAGES_IMAGE`, else the repository's on its backend's
     registry; the tag is `--tag`, else `hushrun-packages-<platform>-latest`;
   - the credential is the same: `HUSHRUN_REGISTRY_TOKEN`, the backend's own token only to its own
     registry, or none. The token goes to hushrun's registry, an image `--image` or
     `HUSHRUN_PACKAGES_IMAGE` names, or a backend the environment named, never a registry a
     repository's file chose (C-13). The variable counts as named because whoever set up the sandbox
     set it, as with `--image`;
   - the layers download beside the destination, are unpacked from there, are checked, and only then
     swap into `--to`, `HUSHRUN_PACKAGES_LOWER`, or `/hushrun-packages`. Nothing is left behind;
   - the unpack takes only entries under `hushrun-packages/`. Unlike a baked store, a package store
     has files that run and links (K-3): a file keeps its owner's run bit (`0755`, else `0644`),
     and a link is kept only under `parts/<part>/`, when its target stays in the part (K-3's
     `link_stays_inside`). Links are made after every file and folder, never through another link,
     and never where an entry is already. A hard link or a device stops it;
   - the check is K-1's by names and bytes, plus the label's identity and, for this machine's tag,
     the platform;
   - trust is K-7's. When `hushrun.yaml` names signers of baked stores, a signature there must verify
     and pass `not_before`, and an unsigned store is refused if any part needs a signature. A store
     whose every part a lockfile checks is taken unsigned, since any run reads it. `--require-signed`
     takes only a verified signature;
   - it is a no-op download when the destination's `packages.json` has the label's identity and
     passes K-1 by names. It also has `--resolve`, `--force`, `-q`, `--format json`
     (`hushrun.packages-pull/v1`) and the GitHub step outputs.
6. **Session start: `packages pull -q` between `build pull` and `fetch`** (C-168). It needs no
   credential for the backend, like `build pull`. It shares the steps' time: a step cut off goes on
   in the background and the later steps are skipped, as before. The block's `Setup:` line names it.

### What the backend needs

The backend's registry is read-only from outside. Its one way in is `POST v0/build/images` (B-70),
which carries a baked store. The backend builds each layer from packs it already holds, and takes at
most 8 MiB per other file and 64 MiB per request. A package store is hundreds of MB of files the
backend has never seen, so it can't use that endpoint, whatever the endpoint checks. The smallest
backend change that keeps the registry read-only to everyone else is a **push grant**:

- `POST v0/packages/images/grant`, with the job's credential (in CI, its ID token), body
  `{"platform", "identity", "commit"}`. The backend answers when the job is CI on the repository's
  default branch, as for B-70's publish: `{"registry", "repository", "username", "password",
  "expires_ms", "tags": {"fixed", "latest"}}`. The credential pushes blobs and manifests to that one
  repository, and tags matching `hushrun-packages-<platform>-*` only. `409 not_newer` comes back when
  a newer commit holds the moving tag.
- The CLI then publishes with the standard OCI push it already uses for `--oci REPOSITORY`, with
  that credential.

That contract belongs in hushrun/cloud's `docs/contracts`. This change could not reach that
repository, so neither the endpoint nor the CLI's call to it exists yet. Until both do:

- publishing goes to any other registry (`--oci ghcr.io/acme/shop`);
- sandboxes pull it with `HUSHRUN_PACKAGES_IMAGE=ghcr.io/acme/shop`. In Claude Code on the web that
  variable goes in the environment's settings, and session start's step then pulls it;
- with no variable set, the step looks at the backend's registry, finds no `hushrun-packages-…`
  tag, and the `Setup:` line says `packages pull none published yet` within a second: both pulls
  exit 12 when the registry holds nothing under the tag, and session start says that, not
  `failed`, since a repository that hasn't published a store has nothing wrong with it.

`hushrun packages pull` itself needs nothing new from the backend. Reading a tag, a config and a
layer from its registry is the pull API `build pull` already uses.

## Consequences

- A sandbox with no Dockerfile starts with the repository's packages, and its first install or
  build reads them in place (K-10) instead of downloading them.
- A store that changes uploads its whole layer again: one layer per store. A layer per part would
  upload only the part whose lockfile changed. That would need its own deterministic writer, and is
  left for when stores grow.
- A pull that the budget cuts off finishes in the background, and its swap is atomic. A first
  install that starts before it lands downloads as it would have without it.
- `/hushrun-packages` is in `/`. A sandbox that doesn't run as root sets `HUSHRUN_PACKAGES_LOWER`,
  and a refused swap says so.
- `Role` (the layer roles of `hushrun-artifact`) gains `packages`.

## Alternatives considered

- **A layer of the baked image.** Baked layers are defined byte for byte for the backend to stream
  from its object store (B-65: files `0644` and folders only). A package store has files that run
  and links. The two also change at different rates (every merge, against lockfile changes), are
  trusted differently (K-7's lockfile parts need no signature), and a sandbox may want one without
  the other. One image would tie each to the other's re-bake limits and size.
- **A separate repository** (`<name>-packages`). That needs a second name, a second pull token
  scope, and registry access rules on the backend. A tag prefix in the same repository needs none
  of that.
- **A referrer signature, as B-67 has for images.** It would sign what `packages.sigstore.json`
  signs, and a pull would read two signatures to trust one store.
- **A client for the push grant now.** Without the endpoint it can't be tested against anything
  real, and the contract may change in review. The refusal names the way that works today.

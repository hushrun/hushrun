# 0148. A pull that says what it pulled, and downloads only what is new

- **Status:** proposed; built
- **Date:** 2026-11-25
- **Rules:** B-240 amended; B-244 new; B-63, B-65, B-66, B-67, B-69 and B-73 read

## Context

`hushrun build pull` (ADR 0143) is merged and not released. It had never read a registry: its
tests covered the unpacking and the swap only. It read every layer into memory whole, up to 8 GiB
each, and held them all until it unpacked them. It downloaded the whole store on every call, so a
Claude Code hook that pulls at each session start downloads the same gigabyte each time. Its line
cut the digest to 12 hex digits, it could not say which image a tag names without downloading it,
and it had no JSON. A bake published an image but told no later job, by step output, which one,
so a template build had to read the moving tag itself or parse the bake's JSON.

## Decision

The sandbox providers' plan (Stage 5, Phase 1) is built as follows. Its decisions D3 (which of the
pull's additions) and D5 (the credential a template build pulls with) are taken as it recommends.

1. **Tested against registries.** A bake, a pull and a fresh machine that reuses the pulled store
   on its signature, against an image layout served over HTTP in every test run, and against
   Distribution and zot in CI's registries job (`build-pull-registries`).
2. **The whole digest** (`sha256:` and 64 hex) in every line.
3. **`--resolve`** reads the manifest and the config and prints `<image>@<digest>`: no layer, and
   nothing written.
4. **Nothing to download.** The image's config label names the store's manifest identity (B-65).
   When it is the `manifest` of the destination's `baked.json`, and the destination passes B-63's
   check by names (what a run checks as it begins), the pull downloads nothing and says so.
   `--force` pulls anyway. No record of past pulls is kept: the label and the store say it all.
5. **`--format json`** prints `hushrun.build-pull/v1` on stdout, and the line stays on stderr. On
   GitHub Actions a pull writes `hushrun-image-digest` and `hushrun-manifest` as step outputs.
6. **Docker's credentials for another registry.** An image on a host that isn't hushrun's registry,
   with no hushrun credential, is pulled with docker's own credentials for that host, as `bake
   --oci` publishes with them. `HUSHRUN_REGISTRY_TOKEN` still goes only where someone named (C-13).
7. **`--require-signed`** refuses a store that no signer `hushrun.yaml` names signed, also when it
   names none: for a template that many sandboxes start from.
8. **Layers on disk.** Each layer is written to a file beside the destination as it arrives,
   hashed and checked, then unpacked from the file an entry at a time, and removed. D3 asked for
   this only if a sandbox failed for want of memory; it was built anyway, because the in-memory
   pull needs as much memory as the whole store, and the change is small.
9. **A session is told it can't pull.** The registry refuses a session (`hrs_…`); when it does, the
   pull says so and names the pull token to set, in place of the registry's own words.
10. **The bake's step outputs** (B-244): `hushrun-image`, `hushrun-image-digest`,
    `hushrun-image-tag`, `hushrun-image-published` and `hushrun-manifest`, to `$GITHUB_OUTPUT`, and
    on GitLab to the file `HUSHRUN_BAKE_ENV` names, which `init --bake` sets with a dotenv report.

D5: a template build pulls with a pull token in `HUSHRUN_REGISTRY_TOKEN`, never a job's ID token,
which the pull doesn't read.

## Consequences

- A second pull in an unchanged sandbox costs a manifest and a config request.
- What the pull compared stands for nothing a moment later: each run still reads `baked.json` and
  decides what it trusts as it begins (B-63, B-64). A destination someone changed to claim another
  manifest is a store no run trusts by that claim.
- Pulling needs free disk for one layer beside the unpacked store, where it needed memory for all.
- `hushrun.build-pull/v1`, the pull's two step outputs, B-244's five and `HUSHRUN_BAKE_ENV` are
  contracts: they only grow.

## Alternatives considered

- **A record of the last pull beside the store** (built first, then dropped): it knows only what
  this machine pulled, while the label also recognises a store that arrived another way, such as a
  sandbox base's `COPY --from`.
- **Unpacking straight from the network stream:** a layer that fails its digest at its end would
  have been unpacked already; written first, nothing of it is unpacked until it checks.
- **Exit codes per failure, `--timeout`, a positional reference:** the plan's reasons hold (B-240's
  exits stay 1 and 2; only silence ends a wait, ADR 0144; `--tag sha256:…` pulls by digest).

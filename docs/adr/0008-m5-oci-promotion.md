# 0008. OCI promotion: choices made in M5

- **Status:** accepted
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-14
- **Rules:** A-1, A-2, A-3, A-4, P-13, P-15, §10.4, §11.5

## Context

M5 implements `artifact resolve`, `artifact copy`, `artifact digest`, tag resolution for
`attest --artifact`, and the `Registry` trait over `oci-client` with Docker credentials. The
plan names `oci-client` and lists two of its gaps. Working with it showed more, and the plan
leaves some output and failure details open. Following plan §0.1, each choice below runs more
checks or deploys less.

## Decision

1. **`oci-client` for reads and blob streaming; hushrun's own requests for the rest.** Two
   operations don't go through `oci-client`:
   - **Mounts.** `oci-client` treats `202 Accepted` as an error and asks for a token scoped to
     the destination only. hushrun sends the mount itself, with a token for
     `repository:<dst>:pull,push` and `repository:<src>:pull`. A `202` means "upload instead"
     (A-2), and so do `404`, `405` and `501`, which registries without mounts send.
   - **Manifest pushes.** `oci-client` doesn't return the registry's `Docker-Content-Digest`,
     which A-2 checks. hushrun PUTs the manifest bytes unchanged and compares the header,
     then HEADs the destination by digest.

   Both requests answer one authentication challenge (Bearer or Basic) with the same Docker
   credentials, and the HTTP client drops credentials when a redirect leaves the host.
2. **Blobs are uploaded in one streamed PUT** with their length (`oci-client`'s monolithic
   push), hashed as they pass through. Chunked uploads would send one request per incoming
   chunk, and ECR rejects parts smaller than 5 MiB. A 2 GB layer copies with a peak resident
   set of about 17 MB.
3. **Credential helpers time out after 20 s.** The helper is killed and the host is tried
   without credentials, with a warning. Docker Desktop's helper hangs when Desktop isn't
   running, and a keychain helper can wait for a prompt nobody answers in CI. Helper names
   must be `[A-Za-z0-9._-]+`, since they become part of a program name. Identity tokens (ACR's
   refresh tokens) aren't exchanged for access tokens: what a helper prints is passed as basic
   credentials, and `identitytoken` entries in `auths` are ignored.
4. **`resolve` re-verifies the justifying event as P-13 accepted it** (A-1): signature,
   acceptance for (`T`, `artifact`), the digest, the pin, and a location declared for `T` or
   an upstream stage and allowed by trust (T-6). Any failure is treated like a failed copy:
   fallback, or exit 7.
5. **The fallback rebuilds the whole scope** (A-3). If any artifact of the scope can't be
   reused, `build` is `true` and every artifact's reference becomes `<T's location>:<tag>`.
   The fallback is off with `--no-fallback`, or when `T` requires a deployment check; then
   `resolve` exits 7.
6. **`resolve` needs a signing key before it copies.** The copy's `artifact` event
   (`origin: copied`, `copiedFrom`) is what `mark deployed` names. A copy nobody records would
   leave the `deployed` event naming the source.
7. **Outputs.** `ref` and `digest` are set when the scope has exactly one artifact, and
   `digest` is that artifact's digest (absent when building). `ref_<name>` is always set.
   JSON: `hushrun.artifact.resolve/v1`, `hushrun.artifact.copy/v1`,
   `hushrun.artifact.digest/v1` (`docs/schemas/`).
8. **Tags are pinned where they enter.** `attest --artifact` and `mark --artifact` resolve a
   tag through the registry and record the digest-pinned reference (A-4). `artifact copy`
   accepts a tagged source, resolves it first, and copies by digest. It records nothing.
9. **Referrers aren't copied.** Signatures and SBOMs attached to an image through `subject`
   stay in the source repository. Copying them is a later addition.
10. **Registry conformance runs on CNCF Distribution and zot**, pinned by digest, on every pull
    request. A 2 GB copy checks the memory bound nightly. A cross-project Artifact Registry
    copy, and a denied one, run nightly once the repository variables are set.

## Consequences

- Failures map to exit 7 (transfer), except malformed references, which are usage errors.
- Denied copies name the missing permission for Artifact Registry, ECR, ACR and GHCR, in the
  `resolve` fallback warning and in `artifact copy`'s error.
- Replacing `oci-client` later means reimplementing manifest reads and blob streaming behind
  the same trait; the copy algorithm and the commands don't change.

## Alternatives considered

- **A registry client written from scratch.** One token cache instead of two, but about
  twice the code, and `oci-client` already handles registry quirks (ECR's missing `Location`,
  redirects to storage).
- **Chunked uploads.** They resume more cheaply after a failure, but cost one request per
  chunk and break on ECR's minimum part size.
- **Falling back per artifact.** Building one artifact while reusing another mixes an
  artifact the upstream validated with one it never saw; building the scope keeps them
  consistent.

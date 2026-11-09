# 9. Spec: artifacts

> Normative. Copied from the implementation plan (2026-10-13) in M0; this file is now the source of truth.
> MUST, SHOULD and MAY follow RFC 2119. Rules tagged `[Mn]` belong to milestone n.
>
> [ADR 0062] Promotion between stages is gone: an artifact has one `location` (C-6), and nothing
> copies it from one stage's location to another's. The file keeps its name, so links still work.

**A-1 Resolve.** `artifact resolve` turns a plan decision into the reference to deploy.

It takes the decision from the plan (`--plan` or `HUSHRUN_PLAN_JSON`, §10.4). In CI a plan is required; locally, without one, it plans now, recording nothing, and refuses a `hushrun.yaml` with retired keys as `plan` does (C-1). For `current` or `blocked` it exits 2, because a pipeline that reaches `resolve` in those modes is wired wrong.

- `full`: `ref` is `<location>:<tag>` (the tag defaults to the revision) and `build = true`.
- `promote` or `refresh`: for each artifact, re-fetch and re-verify the event that justified it (signed by a signer that counts, T-16; about this scope and digest; pinned by digest; at the artifact's `location`, P-13), then deploy the reference it recorded, `<location>@sha256:<digest>`, with `build = false`. Nothing is copied.

**A-2 OCI copy.** `artifact copy` copies an image or index to another repository by digest:

1. Fetch the source manifest by digest and verify its SHA-256. Accepted types: OCI index, OCI manifest, Docker manifest list v2, Docker manifest v2.
2. For an index, first copy every child manifest by digest, for all platforms, so the index digest is preserved.
3. Copy each blob (config and layers), skipping non-distributable layers that carry `urls`:
   - skip it if the destination already has it;
   - on the same registry host, try a cross-repository mount. The token request must include pull scope on the source repository. A `202 Accepted` reply means the registry declined to mount, so upload instead; it is not an error;
   - otherwise stream it, hashing while copying, and fail on a mismatch.

   Concurrency is bounded (default 4), and memory stays bounded whatever the layer size.
4. Push the manifest bytes unchanged, by digest and then by tag. Check the registry's `Docker-Content-Digest`, then HEAD the destination by digest.

[ADR 0062] `artifact resolve` no longer copies: an artifact P-13 reuses is already at its one location.

**A-3 Fallback.** If the event that justified an artifact no longer verifies, or no longer records the reference the plan chose, `resolve` returns `build = true` and `ref = <location>:<tag>`, with a warning that names the cause and ends "building instead". With `--no-fallback`, it exits 7 instead. [ADR 0062] There are no deployment checks, so nothing turns the fallback off by itself.

**A-4 Pinning.** What a deploy deploys, and what an `artifact` event records, MUST be pinned by digest. Tags are written for people only.

**A-5 File artifacts [M12].** Locations are object-store prefixes or directories. The copy streams and verifies SHA-256; the store's own checksums are never trusted for identity.

**A-6 Exec artifacts [M12].** A configured command receives `HUSHRUN_SOURCE_REF`, `HUSHRUN_DIGEST`, `HUSHRUN_DESTINATION` and `HUSHRUN_REVISION`, and prints the resulting digest-pinned ref. hushrun verifies what it can and records the result. This covers package channels (`npm dist-tag`), app store tracks and other custom moves.

**A-7 No baked configuration.** Reusing an artifact is only correct when it has no configuration baked in that should differ from one deploy to the next, such as Next.js `NEXT_PUBLIC_*` values: a reused build carries the values it was built with. `hushrun doctor` warns about known patterns.

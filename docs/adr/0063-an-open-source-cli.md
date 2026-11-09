# 0063. An open-source CLI

- **Status:** accepted
- **Date:** 2026-11-09
- **Rules:** none changed
- **Supersedes:** [0034](0034-closed-source-cli.md)

## Context

The CLI was free to use but closed source, released from `github.com/hushrun/hushrun` with no
source in it. A tool that signs evidence, gates pushes and decides which checks a repository
skips asks for a lot of trust, and people extend that trust more easily to code they can read.
Others could already check hushrun's evidence through standard formats (DSSE, in-toto and
Sigstore bundles; RFC 6962 proofs and C2SP signed notes), but not the code that writes it.

## Decision

1. **The CLI is open source under the MIT license.** That covers everything in this repository:
   the `hushrun` binary, its crates, the specs, the recipes, the GitHub Actions, the npm packages
   and the container image's build. `THIRD-PARTY-NOTICES.md` still lists the crates the binaries
   are built with.
2. **`github.com/hushrun/hushrun` holds the source,** beside the releases and the actions it held
   before. Every install URL, `uses:` reference and image name stays as it was.
3. **The hosted service stays closed.** The server that shares results across an org's machines
   and CI, its dashboard, and billing aren't part of this repository. The CLI talks to it over
   the documented HTTP API, so it works the same against any server that speaks it.
4. **The name and logo aren't covered by the license** ([TRADEMARKS.md](../../TRADEMARKS.md)).
5. **Contributions are signed off** under the Developer Certificate of Origin
   ([CONTRIBUTING.md](../../CONTRIBUTING.md)).

## Consequences

- Anyone can audit what the CLI records, signs and sends, and build it themselves.
- Release archives stay the supported way to install; building from source is for auditing and
  contributing.

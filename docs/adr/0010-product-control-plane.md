# 0010. Product direction: an optional control plane beside the CLI

- **Status:** accepted on 2026-10-14, together with [RFC 0001](../rfc/0001-product-architecture.md)
- **Date:** 2026-10-14
- **Rules:** none yet. RFC 0001's Appendix A lists the spec changes that would follow, each with
  its own ADR
- **Supersedes:** in plan §2.2, the non-goal "a hosted verification API or SaaS backend"; it also
  narrows "release orchestration: rollouts, canaries, approval UIs, environment dashboards" and
  "scorecards, analytics, flakiness detection, cost dashboards"

## Context

The plan scoped v1 to need no server (§2.2), and the specs follow from that: the CLI decides in CI,
events live in a ledger the customer owns, and storage carries no trust. RFC 0001 asks what hushrun
needs to serve many teams. It finds that the decision path must not change, and that what is
missing sits around it: onboarding without secrets or per-repository admin work, a view and search
across repositories, organization policy, and a place to change trust and approve deploys. Some of
that needs services, which the non-goals rule out.

## Decision

1. A service that decides deploys, or verifies events on a pipeline's behalf, stays a non-goal,
   permanently. The CLI decides in CI, from events it verifies itself (RFC 0001 §2.2).
2. After M7, hushrun may add an optional control plane: onboarding automation, an index and a UI,
   organization policy with an admin surface and SSO, and an optional hosted ledger behind the
   `Ledger` trait. It follows RFC 0001's rules CP-1…CP-6: no new planner inputs, off the deploy
   path, trusted only by name, rebuildable, people sign and services relay, and the CLI is enough.
3. The non-goals change as follows:

   | Plan §2.2 non-goal | Now |
   |---|---|
   | Test impact analysis or per-test selection | Unchanged |
   | Release orchestration: rollouts, canaries, approval UIs, environment dashboards | Rollouts and canaries unchanged. Approval UIs and environment dashboards may be part of the control plane, where they show state and collect people's signatures; they never decide |
   | Scorecards, analytics, flakiness detection, cost dashboards | Unchanged, except a reuse report computed from events |
   | A hosted verification API or SaaS backend | Split: deciding or verifying for pipelines stays a non-goal (item 1); the control plane and the hosted ledger become goals after M7 (item 2) |
   | Replacing CI, build systems or build caches | Unchanged |
   | Telemetry of any kind | Unchanged: the CLI reports nothing about its use. It talks to our services only when a customer makes the hosted ledger its ledger, and then sends what it would write to a bucket. Services read only what customers grant them |

4. Nothing changes before M7, and hushrun's 1.0 needs no hosted service.

## Consequences

- Each control-plane milestone's ADR shows how it keeps CP-1…CP-6.
- Once RFC 0001 is accepted, the milestones after M7 follow its §5.1, with M10 moved before M8.
- The `Ledger` trait and the event format are the seams: an `https://` ledger arrives as one more
  backend (M18), and events gain only optional fields.

## Alternatives considered

- **Keep the non-goal.** hushrun stays a tool each team sets up alone; RFC 0001 §3 lists what that
  costs at scale.
- **Drop it entirely**, allowing a hosted decision service. Rejected: every customer's deploys
  would depend on its uptime, and its operator could approve any deploy (RFC 0001 §2.2).

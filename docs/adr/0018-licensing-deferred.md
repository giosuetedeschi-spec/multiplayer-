# ADR-0018: Licensing deliberately deferred

**Status:** deferred

## Context

`tempo` is intended to become a platform with an eventual hosted offering
([ADR-0024](0024-phasing-and-sequencing.md)). Licensing therefore has strategic consequences beyond
the usual open-source considerations: the choice affects adoption, contribution, competitive
exposure, and whether a hosted business is defensible.

Licensing is also close to irreversible in practice. Relicensing after a project has outside
contributors requires the agreement of every copyright holder, or a contributor licence agreement
established from the beginning. Choosing early is cheap; changing later is not.

The project owner has decided not to select a licence at this stage, to avoid making a strategic
commitment before the project's direction is settled.

## Decision

**No `LICENSE` file is added, and no licence is asserted, at this time.**

The consequence must be stated explicitly rather than left implicit, because it is frequently
misunderstood:

> Absent an explicit licence, the work is under **default copyright — all rights reserved**. Third
> parties have no legal permission to use, copy, modify, or distribute it. "No licence" is not
> permissive; it is the most restrictive state possible.

While the repository is private this is inert — nobody has access anyway. It becomes a **hard
blocker** at three specific moments, and the point of recording it here is that these arrive without
warning:

1. **Making the repository public.** Readers may look but may not legally use it. Publishing without
   a licence typically produces confusion rather than adoption.
2. **Accepting an outside contribution.** Without licence terms, the inbound contribution's status is
   unclear, and the pool of people whose agreement is needed for a future relicence begins to grow.
3. **Publishing to any package registry.** crates.io requires a licence field. PyPI, npm, and Go
   module proxies all effectively require one for real use.

**Interim rules while deferred.**

- No `LICENSE` file, and no licence field in any package manifest that would be published.
- Manifests are marked `publish = false` so a registry push cannot happen by accident.
- Every third-party dependency's licence is tracked from the start, since a future choice is
  constrained by what we depend on. A copyleft dependency would foreclose options that are currently
  open.
- No outside contributions are merged while this ADR is `deferred`.

**Revisiting.** This ADR is superseded by a new one when a licence is chosen. The decision should be
made before the first of the three moments above, not after. For context, the shape most commonly
used by projects with this structure is a permissive licence on the protocol, core, and bindings —
maximising adoption, with a patent grant, which matters for a protocol — paired with a source-
available licence on the control plane, protecting the hosted business. That is context, not a
recommendation being adopted here.

## Consequences

- No legal exposure is created by an early choice that later proves wrong.
- The project cannot be publicly adopted, contributed to, or published until this is revisited. It is
  a build-time-only state.
- Dependency licence tracking starts now, so the eventual decision is not constrained by an
  accidental dependency.
- Anyone reading the repository sees this ADR and understands the state is deliberate rather than an
  oversight — which is the main reason to record a non-decision at all.

## Alternatives considered

**Apache-2.0 now.** Maximises adoption, includes an explicit patent grant that is genuinely valuable
for a protocol implementation, and is the pragmatic default. Not chosen: the owner wishes to avoid
any licensing commitment today.

**MIT/Apache-2.0 dual.** The Rust ecosystem convention, with the least friction for contributors. Not
chosen for the same reason.

**A source-available licence such as BSL.** Would protect a future hosted business. Not chosen now,
and worth noting that it is easier to move from permissive to source-available for *new* components
than to relicence existing ones.

**Adding a `LICENSE` file saying "all rights reserved".** Makes the state explicit to readers. Not
chosen because it is legally identical to the current state and reads as a deliberate restriction
rather than a pending decision — this ADR communicates the situation more accurately.

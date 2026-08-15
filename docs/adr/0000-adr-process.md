# ADR-0000: How we record decisions

**Status:** accepted

## Context

`tempo` makes a large number of decisions that are individually defensible and collectively
non-obvious. Fixed-point math instead of floats. A custom wire format instead of protobuf. A core
that owns your game state instead of a transport that carries it. Six months from now, someone —
possibly the person who wrote it — will look at one of these and try to "fix" it.

Undocumented decisions get relitigated. Worse, they get quietly reversed by someone who did not
know what the original constraint was, and the reversal only shows up later as a desync bug in
production.

## Decision

Every architectural decision gets a numbered, immutable file in `docs/adr/`, using this structure:

- **Status** — `proposed`, `accepted`, `superseded by ADR-NNNN`, or `deferred`
- **Context** — the forces at play, including the ones that make this hard
- **Decision** — what we are doing, stated plainly
- **Consequences** — what this costs us, stated honestly, including what it makes impossible
- **Alternatives considered** — what lost, and specifically why

Rules:

1. **ADRs are append-only.** To change a decision, write a new ADR that supersedes the old one and
   update the old one's status line. Never edit the substance of an accepted ADR.
2. **Consequences must include the bad ones.** An ADR listing only benefits is marketing, not a
   decision record, and is worthless to the person who later hits the downside.
3. **Alternatives must be real.** "We could have done nothing" is not an alternative. Name the
   library, the technique, or the design that a competent engineer would actually have proposed.
4. **Link from code.** Where an implementation is surprising because of an ADR, the code comment
   cites the ADR number rather than re-explaining it.

## Consequences

- Writing an ADR is friction on every significant decision. That is the point; it is cheap
  relative to discovering the rationale by archaeology.
- The ADR set is the honest history, including decisions we later reverse. Superseded ADRs stay in
  the tree — a reader needs to see the wrong turn to understand the right one.
- Numbers are permanent and never reused, so a gap in the sequence means an ADR was withdrawn
  before acceptance.

## Alternatives considered

**Rationale in code comments only.** Comments explain local mechanism well and cross-cutting
architecture badly. A decision like "the core owns state" touches thirty files and belongs in none
of them.

**A design document per subsystem, revised in place.** Revised documents lose history exactly where
history matters most: the moment the decision changed. Git history technically preserves it, but
nobody reads `git log` on a design doc before proposing a change.

**A wiki.** Detached from the commit that implements the decision, and drifts silently. Docs in the
repository are reviewed in the same pull request as the code they justify.

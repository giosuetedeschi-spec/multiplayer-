# ADR-0025: The name `tempo`

**Status:** accepted

## Context

The name becomes an identifier in six package ecosystems simultaneously — crates.io, PyPI, npm, a Go
module path, a C header prefix and symbol namespace, and a CMake target. That is a stricter
constraint than naming a single-language library:

- It must be **typeable and pronounceable**, since it appears on every import line.
- It must work as a **C symbol prefix** (`tempo_session_create`), which rules out anything long or
  awkward.
- It should **suggest the domain** without over-narrowing it.
- It must be **findable** — a name colliding with a large existing project is a permanent
  discoverability tax that no amount of documentation fixes.

## Decision

**`tempo`.**

Rationale:

- **Semantically apt.** Tempo is the rate at which a piece proceeds — precisely what a fixed-timestep
  engine governs. Tick rate, send rate, clock dilation and the confirmed frame are all tempo. The
  metaphor holds at the level of the actual mechanism, not just vibes.
- **Short and clean across ecosystems.** `use tempo::`, `import tempo`, `tempo.NewServer()`,
  `tempo_session_create()`. Five letters, no ambiguous spelling, no awkward consonant clusters, no
  case convention problems.
- **Correctly scoped.** It suggests timing and rhythm without committing to one netcode model. This
  matters because `tempo` implements several: rollback, snapshot replication, and eventual sync
  ([ADR-0023](0023-eventual-sync-mode.md)).

**Conventions.**

- Crates are `tempo-*` with `tempo` as the facade.
- C symbols are prefixed `tempo_`; types are `TempoSession`, `TempoWorld`.
- Where an unscoped registry name is unavailable, the fallbacks are `@tempo/core` on npm and
  `tempo-rs` on crates.io, keeping `tempo` as the import identifier regardless of the package name —
  the identifier is what users type, and it stays stable.

Registry availability is verified before first publish, which cannot happen while
[ADR-0018](0018-licensing-deferred.md) stands.

## Consequences

- One identifier across six ecosystems, so documentation and examples read consistently.
- Some collision risk in JavaScript tooling, where the word is common. Scoped-package fallbacks
  mitigate it without changing what users write.
- The musical metaphor does not extend to subsystem naming. Subsystems are named literally —
  `tempo-rollback`, `tempo-interest`, `tempo-wire` — because cute names in a technical vocabulary
  cost readers time. The metaphor stops at the front door, deliberately.

## Alternatives considered

**`lockstep`.** Immediate domain signal to anyone in the field, and unambiguous about what the
library does. Rejected because it names *one* netcode model — deterministic lockstep — while `tempo`
also does authoritative snapshot replication and eventual sync. The name would actively mislead about
two thirds of the product.

**`chorus`.** Many independent voices staying in sync is an apt metaphor for both rollback peers and
replicated clients, and it reads pleasantly. Rejected on availability: high squatting risk on npm and
PyPI, and it says less about timing than `tempo` does about the actual mechanism.

**`quorum`.** Fits authority and agreement well. Rejected because of JPMorgan's Quorum blockchain,
which dominates search results — a permanent discoverability cost.

**A coined, unique word.** Guaranteed availability everywhere and perfect searchability. Rejected
because coined names carry no meaning and are harder to remember and spell, which matters for
something typed on every import line.

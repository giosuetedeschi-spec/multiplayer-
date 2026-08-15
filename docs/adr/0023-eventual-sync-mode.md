# ADR-0023: A CRDT mode for non-game realtime sync

**Status:** accepted

## Context

Collaborative applications — shared cursors, whiteboards, documents, dashboards — are a target
workload, and they resemble games enough to share infrastructure: many participants, low latency,
partial state per client, presence, and reconnection.

But their consistency model is the opposite. Games are **tick-locked**: a global timeline, an
authority, and a well-defined state at every tick. Collaborative apps are **eventually consistent**:
no global clock, edits originate anywhere, everything must converge, and offline participants must be
able to merge on reconnect.

Forcing collaborative apps into the tick model produces bad results — a shared document does not want
a 60 Hz simulation, an authority that discards concurrent edits, or a rollback that undoes someone's
typing. Building a separate product would duplicate transport, reliability, interest management,
connection handling, security, and observability, all of which apply unchanged.

## Decision

A second mode, `Mode::EventualSync`, riding the same replication pipeline with a different
convergence rule.

**Merge strategies per field**, declared in the schema alongside quantization:

| Strategy | Semantics | Typical use |
|---|---|---|
| `LwwRegister` | Last write wins by hybrid logical clock, ties broken by peer ID | Cursor position, selection, status |
| `Counter` | PN-counter; increments and decrements commute | Vote tallies, reaction counts |
| `OrSet` | Observed-remove set; concurrent add and remove resolve to add | Tags, participants, layers |
| `RgaSequence` | Replicated growable array for ordered text and lists | Document text, ordered lists |

Each is a genuine CRDT: commutative, associative and idempotent, so any delivery order converges.

**What changes from tick mode.**

- No global tick. Updates are timestamped with **hybrid logical clocks**, which give causal ordering
  without synchronised wall clocks.
- No authority. Any participant may write any field; conflicts resolve by merge rule rather than by
  a server deciding.
- No prediction or rollback. Local writes apply immediately and are correct by construction, because
  merge is commutative.
- Delivery is `ReliableUnordered` ([ADR-0010](0010-reliability-channels.md)); ordering is
  unnecessary when operations commute.
- Offline participants buffer operations and merge on reconnect. This falls out of CRDT semantics
  rather than needing separate machinery.

**What is reused unchanged.** Transport, reliability, connect tokens, interest management (still
valuable — a large document does not send every paragraph to every viewer), persistence, metrics,
capture, and every language binding. The shared portion is the large majority of the system.

**Persistence** ([ADR-0007](0007-ephemeral-core-pluggable-durability.md)) fits naturally: the
operation log *is* the document history, so point-in-time restore becomes document history for free.

## Consequences

- Collaborative applications get a genuinely appropriate consistency model rather than a game engine
  wearing a disguise.
- The infrastructure investment amortises across two markets with one implementation.
- Two modes means two sets of semantics to document and test, and a real risk of users choosing the
  wrong one. The guide leads with the distinguishing question — *is there a globally correct answer
  at each instant?* — because that single question decides it.
- CRDTs carry metadata overhead. `RgaSequence` in particular retains tombstones for deleted elements;
  compaction is provided, and the cost is documented rather than glossed.
- Modes do not mix within a session. A game needing a collaborative sub-feature runs two sessions.
  Unifying them is not planned, because the consistency models genuinely conflict.

## Alternatives considered

**Don't support non-game sync at all.** Keeps the project focused, which is a real virtue. Rejected
because the overlap is large and the marginal cost — a merge layer over existing infrastructure — is
small relative to the addressable use.

**Force collaborative apps into the tick model with an authoritative server.** No second mode, no
CRDTs. Rejected because it produces a bad product: concurrent edits get discarded rather than merged,
and offline editing is impossible.

**Operational transformation instead of CRDTs.** Historically dominant for text, and more compact
than CRDTs for that specific case. Rejected because OT requires a central server to transform
operations, conflicting with the P2P topologies `tempo` supports, and it does not generalise cleanly
beyond sequences.

**Delegate to an existing CRDT library such as Yjs or Automerge.** Mature, well tested, and would
save real work. Rejected because they own their own transport, persistence and wire format — adopting
one means bypassing our pipeline entirely, forfeiting interest management, our bandwidth budgets, and
the binding layer. A `tempo` transport adapter *for* those libraries is attractive future work and is
a different thing from this decision.

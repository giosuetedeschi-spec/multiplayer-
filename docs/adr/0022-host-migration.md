# ADR-0022: Deterministic successor election

**Status:** accepted

## Context

In listen-server and mesh topologies there is no dedicated server, so authority lives on a player's
machine. That player will eventually leave — by quitting, crashing, or losing connectivity. Without
host migration, the session ends for everyone, which is the defining weakness of peer-hosted
multiplayer and the reason players resent it.

Migration is harder than it first appears:

- The departure may be **ungraceful**, with no warning and no final state broadcast.
- Peers hold **differing amounts of state**; some are further ahead than others.
- The decision must be **unanimous**. Two peers each believing they are the new host splits the
  session permanently.
- It must be **fast**. Several seconds of freeze is worse than most alternatives.

This is a consensus problem under partial failure, which is a category with a long history of
subtly wrong implementations.

## Decision

**Deterministic election over shared state, not a distributed consensus protocol.**

**Detection.** Peers track liveness with the same ping/pong used for clock synchronisation
([ADR-0016](0016-clock-sync-and-time-dilation.md)). A host silent for the configured timeout
(default 2 s) is presumed gone. The timeout is deliberately not aggressive — migrating away from a
host having a brief hiccup is worse than a short stall.

**Election.** Every peer runs the *same deterministic function* over the *same shared state*: the
peer list from the last confirmed frame ([ADR-0013](0013-rollback-model.md)), which every peer has
by definition, is filtered to those still reachable and sorted by a stable key — a
`(connection_quality_bucket, peer_id)` tuple. Lowest wins.

Because the input is shared and the function is deterministic, every peer independently computes the
same answer with no votes, no rounds, and no protocol to get wrong. Determinism, adopted for
rollback, turns a consensus problem into a pure function.

Quality is bucketed rather than continuous so that small measurement differences between peers cannot
change the ordering — a continuous key would reintroduce exactly the disagreement the design
eliminates.

**State transfer.** In a mesh, every peer already holds full state and the new host simply asserts
authority from the confirmed frame. In a listen-server topology, clients hold only replicated state,
so the elected peer promotes its state to authoritative, ticks forward from the confirmed frame, and
other peers resynchronise from it.

**Reconnection.** Peers re-point their connections at the new host, re-running ICE where needed
([ADR-0021](0021-p2p-nat-traversal.md)). Because peers in a mesh are already connected to each other,
this is usually instant.

**Split-brain protection.** Each migration increments an epoch counter carried in every packet. A
returning old host arrives with a stale epoch, is recognised as such, and is demoted to a client
rather than fighting for authority.

**Announced departure.** A host leaving gracefully broadcasts its intent and its final confirmed
state, making migration seamless rather than merely correct. This is the common case and is worth
optimising for.

## Consequences

- Sessions survive host departure, which removes the main objection to peer-hosted play.
- Election needs no protocol, no voting rounds and no leader-election library, because determinism
  does the work. This is substantially simpler and less bug-prone than the alternatives.
- Migration is visible: a stall between detection and the new host ticking forward, typically
  detection timeout plus a few hundred milliseconds. Applications receive an event so they can show
  something honest to the player.
- A network partition can produce two viable sub-sessions, each electing a host. Epoch counters
  prevent state corruption on rejoin but cannot merge diverged timelines; the minority partition is
  ended. This limitation is documented rather than hidden.
- Bucketed quality can elect a host that is not strictly the best available. Accepted, because
  agreement matters more than optimality.

## Alternatives considered

**No host migration; the session ends.** Trivial. Rejected as the defining weakness of peer-hosted
multiplayer and a poor experience for players who did nothing wrong.

**Raft or Paxos for leader election.** Correct under adversarial conditions and well studied.
Rejected as substantial machinery for a problem determinism already solves; a full consensus protocol
would be more code and more failure modes than the entire election path as designed.

**A cloud service arbitrates migration.** Reliable and simple to reason about. Rejected because it
makes P2P sessions dependent on an always-available service, which undermines the reason to choose
P2P.

**Always promote the lowest-latency peer, measured continuously.** Optimal host selection. Rejected
because continuous measurements differ between peers, so the election is no longer deterministic —
reintroducing the disagreement the bucketed key exists to prevent.

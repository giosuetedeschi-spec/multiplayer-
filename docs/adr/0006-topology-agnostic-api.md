# ADR-0006: Topology is configuration, not a rewrite

**Status:** accepted

## Context

`tempo` supports three network topologies with genuinely different properties:

- **Dedicated** — an authoritative server process no player controls. Cheat-resistant, costs money
  to run, adds a hop of latency for every player.
- **Listen-server** — one player's machine is authoritative. Free, lower latency for the host,
  and the host can cheat. Needs host migration when they leave.
- **Mesh (peer-to-peer)** — every peer simulates and rolls back. Lowest possible latency, no server
  cost, and every peer can see and forge the entire simulation.

Games routinely need more than one. A game ships with P2P for friend lobbies and dedicated servers
for ranked. A game prototypes on a listen-server and moves to dedicated at scale. If topology is
baked into the API shape, each of those is a rewrite of all networking code.

There is a real tension with honesty here: making the topologies interchangeable in code risks
implying they are interchangeable in *security*, which they emphatically are not.

## Decision

**Topology is a configuration value.** The same application code runs under all three:

```rust
Session::new(Config { topology: Topology::Dedicated, .. })
Session::new(Config { topology: Topology::ListenServer, .. })
Session::new(Config { topology: Topology::Mesh, .. })
```

What varies underneath — who holds authority, whether inputs are broadcast or submitted, whether
rollback is active, whether host migration is armed — is handled by the core. What stays constant is
the API: declare components, read views, write through the command buffer, call `tick`.

Authority is expressed as a per-component property rather than a global one, so the same mechanism
covers "the server owns everything", "each client owns its own player", and mesh's "every peer owns
its own inputs and simulates the rest".

**Security is documented, not implied.** [`design/trust-model.md`](../design/trust-model.md) states
per topology exactly what a malicious participant can do, in plain terms:

- **Dedicated** — clients send inputs only; the server validates and is the sole authority. Cheat
  resistance is a property you can actually rely on.
- **Listen-server** — the host has full control of the simulation. Suitable for co-op and friend
  games. Not suitable for anything competitive or with persistent rewards.
- **Mesh** — every peer sees all state and can forge any of it. Trusted peers only. `tempo` provides
  desync detection, which catches *accidental* divergence and casual tampering, and is not
  anti-cheat.

Runtime reinforces the docs: constructing a `Mesh` or `ListenServer` session emits a one-time
informational log naming the trust assumption. Not a warning to be suppressed — a statement of what
was configured.

## Consequences

- Prototyping on a listen-server and shipping on dedicated servers is a config change, which is the
  single most valuable property of this design.
- Some code paths carry a topology branch. These are concentrated in authority resolution and
  session setup rather than smeared through replication.
- A uniform API across topologies with wildly different security properties is a genuine footgun.
  Explicit documentation and the startup log are the mitigation; there is no way to make P2P
  cheat-resistant by API design.
- Mesh sessions require rollback and therefore fixed-point determinism
  ([ADR-0002](0002-fixed-point-determinism.md)), while dedicated sessions do not. The core validates
  this at configuration time rather than failing mysteriously later.

## Alternatives considered

**Separate APIs per topology.** Each could be optimally shaped, and the security difference would be
structurally obvious. Rejected: it triples the API surface across six languages and makes changing
topology a rewrite — the exact cost this decision exists to remove.

**Authoritative only; peer links purely as a latency optimisation.** Safest and simplest to reason
about. Rejected because it rules out serverless P2P sessions entirely, which is a stated requirement.

**Peer-to-peer with a validating referee** — peers run rollback while a lightweight server checks
per-tick state hashes and input plausibility and evicts divergent peers. Genuinely attractive: most
of P2P's latency with real cheat detection. Rejected for v1 because it requires running a server
anyway, so it is a strictly more complex third system rather than a replacement for either. Recorded
as a strong candidate for a future ADR.

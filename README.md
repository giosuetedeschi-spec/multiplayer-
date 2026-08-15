# tempo

A plug-and-play multiplayer engine with one shared Rust core and first-class bindings for
**Rust, Go, Python, TypeScript, C and C++**.

Authoritative server, listen-server, and peer-to-peer mesh. Rollback. Fixed ticks. Four kinds of
delta compression. Interest management. Lag compensation. Client prediction and reconciliation.
NAT traversal. Matchmaking. One import, one API, six languages.

> **Status: P1 complete, pre-alpha.** The Rust core works end to end — a predicting client stays
> in agreement with an authoritative server at 300 ms round trip and 10% packet loss, with zero
> corrections. Language bindings are next. See [`docs/ROADMAP.md`](docs/ROADMAP.md) for what exists
> today, and [`mistakes.md`](mistakes.md) for what went wrong on the way.

> **Licensing is deliberately deferred** — there is no `LICENSE` file yet, which under default
> copyright means all rights reserved. See [ADR-0018](docs/adr/0018-licensing-deferred.md).

---

## The idea

Every feature people actually want from a multiplayer framework — rollback, delta compression,
interest management, lag compensation — requires the framework to **see and snapshot your game
state**. A transport-only library physically cannot provide them; that is why you normally
reimplement all of it yourself.

So `tempo` inverts the usual split. The Rust core owns a **columnar world arena**. State you mark
as replicated lives there; everything else stays in native objects in your language. Host
languages read the arena **zero-copy** and write through a **command buffer staged once per tick** —
never one FFI call per entity.

Three things fall out of that single decision:

- **Rollback save/restore is a memcpy of the arena** — cheap, and identical in all six languages.
- **Delta compression and interest management see every field**, so they need no user code.
- **Lag compensation** can ring-buffer historical worlds for server-side rewind.

You still drive the loop. `tempo` is a library you call, not a framework that owns `main()`.

## What it looks like

```rust
use tempo::prelude::*;

#[derive(Replicate)]
struct Player {
    #[replicate(quantize = "0.001", min = "-1000", max = "1000", priority = 2.0)]
    position: Vec2,
    #[replicate(quantize = "0.01", min = "0", max = "100")]
    health: Fx,
    #[replicate(bits = 10)]
    score: u32,
}

let mut world = World::new();
let players = world.register_component::<Player>()?;

let e = world.spawn();
players.write(&mut world, e, &Player {
    position: Vec2::from_ints(3, 4),
    health: Fx::from_int(100),
    score: 700,
})?;

// Replicate to a peer: a bit-packed delta against what that peer already holds.
let delta = encode_delta(&world, previous_baseline.as_ref())?;
apply_delta(&mut client_world, &delta.bytes)?;
```

The same program in Python, TypeScript, Go, C, or C++ will be the same shape — those bindings are
Phase 3 and 4.

Run the end-to-end demo to see the whole stack working:

```
cargo run -p authoritative-demo
```

```
link             lead   arrived   mean B corrected starved   resync
perfect             1    100.0%     20.0         0       1        0
broadband           3     99.5%     20.0         0       3        0
mobile             20     88.0%     19.7         0      20        0
```

Zero corrections at 300 ms round trip with 10% loss, and snapshots averaging 20 bytes.

## Documentation

| | |
|---|---|
| [`docs/adr/`](docs/adr/) | Every architectural decision, with the alternatives and why they lost |
| [`docs/spec/`](docs/spec/) | Wire protocol, C ABI, fixed-point math, schema hashing — the normative specs |
| [`docs/design/`](docs/design/) | Deep dives: replication pipeline, rollback, interest management, trust model |
| [`docs/guides/`](docs/guides/) | Per-language quickstarts, topology selection, bandwidth tuning |
| [`docs/ops/`](docs/ops/) | Deployment, orchestration, observability, control plane |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Phases and current status |

Start with [`docs/README.md`](docs/README.md) for the recommended reading order.

## Repository layout

```
crates/          the Rust core, services, and CLI
bindings/        python · typescript · go · c · cpp
integrations/    bevy · godot · three · pixi · react
examples/        runnable demos, including a cross-language matrix
conformance/     shared test vectors every language must satisfy
docs/            specs, ADRs, design notes, guides, ops
```

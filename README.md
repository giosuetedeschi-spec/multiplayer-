# tempo

A plug-and-play multiplayer engine with one shared Rust core and first-class bindings for
**Rust, Go, Python, TypeScript, C and C++**.

Authoritative server, listen-server, and peer-to-peer mesh. Rollback. Fixed ticks. Four kinds of
delta compression. Interest management. Lag compensation. Client prediction and reconciliation.
NAT traversal. Matchmaking. One import, one API, six languages.

> **Status: pre-alpha, under active construction.** The specifications in [`docs/`](docs/) are
> the source of truth and are written ahead of the implementation on purpose. See
> [`docs/ROADMAP.md`](docs/ROADMAP.md) for what exists today.

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
    #[replicate(quantize = "0.001", delta = "dirty_mask")]
    position: Vec2,
    #[replicate(quantize = "0.01")]
    health: Fx,
    #[replicate(priority = 0.2)]
    score: u32,
}

let mut server = Session::server(Config {
    topology: Topology::Dedicated,
    tick_rate: 60,
    send_rate: 20,
    ..Default::default()
})?;

loop {
    let frame = server.begin_tick()?;
    for input in frame.inputs() {
        // your gameplay logic, against zero-copy views of the arena
    }
    server.end_tick(frame)?;
}
```

The same program in Python, TypeScript, Go, C, or C++ is the same shape — see
[`docs/guides/`](docs/guides/).

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

# ADR-0026: Rust, not Go, for the core

**Status:** accepted

## Context

Go is an outstanding language for network services. Docker, Kubernetes, etcd, CockroachDB, Caddy and
most of the modern infrastructure layer are written in it, and for good reason: goroutines make
concurrent I/O straightforward, the standard library's networking is excellent, compile times are
fast, and deployment is a single static binary. "Networked backend" and "write it in Go" are
correctly associated in most engineers' minds.

So the question is a fair one, and it was asked directly: `tempo` is a networking framework, so why
is the core not Go?

The answer is that `tempo`'s core is not a network service. It is an **embeddable, latency-critical,
fixed-timestep simulation kernel** that must be loaded into five foreign runtimes including a
browser. That is a different problem, and it is the one place Go is genuinely a poor fit.

## Decision

**The core (`tempo-core` and every crate beneath it) is Rust. Go remains a fully supported
first-class binding.**

Four requirements decide it, in descending order of how disqualifying they are.

### 1. It must be embeddable — this alone is decisive

[ADR-0001](0001-core-owns-replicated-state.md) and [ADR-0008](0008-c-abi-single-boundary.md) require
one core loaded *inside* a Python interpreter, a Node process, a Go program, a C++ application and a
browser tab.

Go cannot do this well, and in one case cannot do it at all:

- **`c-shared` brings the whole Go runtime with it.** Loading it into CPython means a second
  scheduler, a second garbage collector and its background threads living inside the host process.
- **Every crossing is a cgo transition**, which switches to a Go stack and costs on the order of tens
  of nanoseconds each way, in both directions. Our hot path is designed around a constant number of
  crossings per tick precisely because they are expensive; a Go core would add that cost to *every*
  binding rather than only the Go one.
- **Two `c-shared` Go libraries in one process is unsupported.** A user whose application already
  links a Go library would be unable to load ours.
- **The browser is fatal.** `GOOS=js/wasm` emits the full runtime and GC, several megabytes before
  any of our code, against a size budget where we are counting kilobytes
  ([ADR-0019](0019-build-and-distribution.md)). TinyGo is smaller but drops reflection and parts of
  the concurrency model we would be relying on. Rust compiles to wasm with no runtime at all.

There is no configuration of Go that makes a browser-embeddable, multi-tenant-safe shared library.
This requirement is not negotiable, so the question is settled here; the remaining points are why
Rust would still win even if it were not.

### 2. Garbage collection versus a hard per-frame budget

Go's collector is genuinely impressive — sub-millisecond pauses are routine. But our workload is its
worst case in two specific ways:

- **The live set is large and long-lived.** The arena, the rollback snapshot ring
  ([ADR-0013](0013-rollback-model.md)), and the lag-compensation history buffer
  ([ADR-0014](0014-lag-compensation.md)) can be hundreds of megabytes and are *all reachable*. GC
  cost scales with the live set that must be traced, not with garbage produced.
- **The budget is hard and the peak is spiky.** At 60 Hz a frame is 16.6 ms, and a rollback frame may
  run eight simulation steps inside it. A 1 ms pause landing on that frame is a visible hitch.

The workaround is off-heap allocation via `mmap` and `unsafe`, which is exactly what our arena is —
and doing it in Go means abandoning the memory safety that is Go's main advantage while keeping the
runtime's costs. At that point Go is being used as a worse C.

Rust has no collector. The arena's lifetime is explicit, and freeing it is a decision rather than an
event.

### 3. Memory layout control

The columnar arena, the zero-copy views handed across the ABI, the bit-packed codec
([ADR-0009](0009-custom-bitpacked-wire-format.md)) and `memcpy`-speed snapshot save/restore all
depend on knowing and controlling layout exactly. Rust gives `#[repr(C)]`, `#[repr(packed)]`, const
generics, guaranteed niche layout, and portable SIMD. Go gives struct field ordering and a promise
not to move objects — with cgo pointer rules that forbid storing Go pointers in C memory, which is
precisely what a zero-copy view handed to Python would be.

### 4. Determinism

Roughly neutral, and worth saying so rather than claiming an advantage that is not there. Go is
actually good here: it forbids x87 excess precision and FMA contraction. But since all simulation
arithmetic is integer ([ADR-0002](0002-fixed-point-determinism.md)), both languages are fine. This
consideration does not distinguish them.

### Where Go genuinely would be better

Stated plainly, because the intuition behind the question is sound and applies elsewhere:

- **The matchmaker, orchestrator and control plane** ([ADR-0024](0024-phasing-and-sequencing.md), P6
  and P8) are I/O-bound, connection-heavy, throughput-oriented services with soft latency
  requirements. Go is an excellent fit for exactly that shape of work.
- **User game servers.** A studio writing an authoritative server in Go is fully supported and is a
  good choice ([ADR-0004](0004-language-parity-and-perf-ceilings.md)). The heavy lifting happens in
  the core either way, so Go's ceiling here is high.

We nonetheless write the in-repo services in Rust, for a reason unrelated to language quality: the
relay speaks the wire protocol and the matchmaker mints connect tokens
([ADR-0015](0015-connect-tokens-and-security.md)). In Go those would either duplicate the codec and
the cryptography — creating a second implementation to keep in sync, which is the failure mode this
project is organised to avoid — or call into the core via cgo, giving up Go's ergonomics anyway.

## Consequences

- The core is embeddable everywhere, including the browser, with no runtime and no collector.
- Rust's learning curve and compile times are a real cost to core development, paid by contributors
  rather than users.
- Go users get a first-class binding rather than a first-class implementation. Given that the core
  does the work in every language, this is a much smaller difference than it sounds — but it is a
  real one, and Go developers who want to read the engine will be reading Rust.
- We forgo goroutines for the transport layer and use async Rust instead, which is less pleasant to
  write and more pleasant to bound.

## Alternatives considered

**Go core with cgo bindings for the other five languages.** Rejected on §1: it cannot target the
browser at an acceptable size, cannot safely coexist with another Go shared library in one process,
and taxes every binding with cgo transitions.

**Go for the services, Rust for the core — two languages in the repository.** Genuinely reasonable,
and the split most infrastructure projects would choose. Rejected because the services need the wire
codec and the token cryptography, so the boundary between "service" and "core" does not fall where
the language boundary would need to. Revisit if a future service is purely orchestration with no
protocol involvement.

**C or C++ core.** Maximum portability, trivially embeddable, no runtime. Rejected: no memory safety
across a boundary exposed to six languages, far weaker tooling for the conformance and property
testing this design depends on, and no equivalent of Cargo for the build and distribution matrix in
[ADR-0019](0019-build-and-distribution.md).

**Zig core.** Excellent control, genuinely simple, good C interop, strong cross-compilation. Rejected
on ecosystem maturity for the cryptography, QUIC and WebRTC dependencies we need, and on the smaller
pool of contributors. Not a criticism of the language.

# ADR-0019: Prebuilt binaries in every package ecosystem

**Status:** accepted

## Context

`tempo`'s core promise is a plug-and-play import. That promise dies at the install step if
`pip install tempo` requires a Rust toolchain, or `npm i tempo` triggers a ten-minute compile, or
`go get` fails because cgo cannot find a header.

This is where most polyglot Rust projects actually fail. The library is excellent and nobody can
install it.

The matrix is large: six ecosystems × three operating systems × two architectures, plus musl versus
glibc on Linux, plus a WebAssembly target, plus Python's ABI versions and Node's N-API versions.

## Decision

**Every package ships prebuilt binaries. Building from source is a fallback, never the default
path.**

| Ecosystem | Artifact | Mechanism |
|---|---|---|
| Rust | source crate | Cargo compiles it; this is the one case where source is correct |
| Python | wheels | `maturin`, abi3 wheels so one wheel covers Python 3.8+; manylinux, musllinux, macOS universal2, Windows |
| Node/Bun/Deno | platform packages | `napi-rs`; per-platform packages referenced via `optionalDependencies` so only the matching one downloads |
| Browser | wasm bundle | `wasm-bindgen`, shipped inside the same npm package with conditional exports |
| Go | prebuilt archives | Static archives per platform, committed and selected by build tags; cgo links, does not compile Rust |
| C / C++ | release archives | Static and dynamic libraries plus `tempo.h`, with pkg-config and CMake config files |

**Build matrix.** `cross` and `cargo-zigbuild` in CI produce linux-{x86_64,aarch64}-{gnu,musl},
macos-{x86_64,aarch64}, windows-{x86_64,aarch64}, and wasm32-unknown-unknown from a single Linux
runner, which keeps CI cost and complexity manageable.

**Reproducibility.** Release builds pin the Rust toolchain version, and artifacts are checksummed
with checksums published alongside. A given tag produces byte-identical binaries — which matters
more here than usual, because the wire format and fixed-point maths must be identical across
platforms and reproducible builds make that verifiable rather than merely tested.

**Size.** Go archives and wasm bundles are size-sensitive. The wasm build is compiled with
`opt-level = "z"`, LTO, and `wasm-opt`, and its size is tracked in CI with a budget, because a
browser game engine that adds two megabytes to the bundle will not be adopted.

**The escape hatch.** Every package can build from source with an explicit opt-in
(`TEMPO_BUILD_FROM_SOURCE=1`) for unusual platforms and for users who need to audit what they run.

## Consequences

- Install is a download in every ecosystem, with no toolchain requirement.
- Release engineering is substantial and permanent: a release is roughly twenty artifacts, and every
  one must be tested on its target platform. This is automated from the start because doing it by
  hand does not scale past the first release.
- Committing Go archives makes the repository large. Git LFS or a release-artifact fetch step is
  used rather than raw binaries in history.
- Prebuilt binaries are a supply-chain surface. Checksums, provenance attestation, and reproducible
  builds are the mitigations, and they are set up before the first public release rather than after
  an incident.
- Platforms outside the matrix — BSD, unusual libc, older glibc — fall back to source builds. The
  matrix expands based on demand rather than speculation.

## Alternatives considered

**Source distribution everywhere.** Simplest release process, always correct for the target machine.
Rejected: it requires a Rust toolchain in every user's environment and every CI pipeline, which is
disqualifying for the plug-and-play promise.

**A separately installed shared library** — install `libtempo` via the system package manager, and
have language packages link against it. Clean separation and one copy on disk. Rejected because it
makes installation a two-step process that varies per operating system, which is exactly the friction
being designed out.

**WebAssembly everywhere, including native.** One artifact for all platforms, no cross-compilation
matrix at all. Genuinely tempting for the simplicity. Rejected on performance: wasm gives up SIMD
consistency and native memory layout control, and the arena's cache behaviour is central to the
design. Retained for browsers, where it is the only option.

**Bundling a compiler.** Some projects vendor a toolchain. Rejected as enormous and hostile.

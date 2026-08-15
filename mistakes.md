# mistakes.md

A running, honest log of errors made while building `tempo` — mine, kept so the same class of
mistake gets caught faster the next time.

Rules for this file:

- Log the mistake **and the class it belongs to**, not just the fix. The class is the reusable part.
- Log mistakes caught before they shipped as well as after. A near-miss teaches the same lesson.
- Never delete an entry. If something turns out not to have been a mistake, append a correction.

---

## 2026-08-15 — Hand-computed five spec constants wrong

**What.** `docs/spec/fixed-point.md` §1.1 committed raw `i64` values for `PI`, `INV_TAU`, `E`,
`LOG2_E` and `LN_2`. Five of the eleven were wrong in the final hex digit — I computed them mentally
and truncated where the spec I was writing on the same page requires **round half away from zero**.

**Caught by.** Running `tempo-tablegen`, which prints the constants specifically so the spec table
can be checked against generated output. Caught before any code consumed them.

**Class: asserting a computed value I did not compute.** Writing "3.14159… × 2³² = 0x…" in prose
feels like stating a fact, but it is arithmetic, and arithmetic done in my head is not evidence. The
spec is normative — a wrong constant there would have become a wrong constant in six
implementations, and `PI` being one ULP off is precisely the kind of error that produces a desync
after ten minutes of play rather than immediately.

**Rule.** Any numeric literal that is the result of a calculation gets generated and printed by a
program before it is committed. If it is worth putting in a normative document, it is worth running.

---

## 2026-08-15 — `atan2` read one past the end of its table

**What.** `docs/spec/fixed-point.md` §2.3 specified `ATAN_TABLE` as 1025 entries indexed
`i = r >> 22`, then interpolated using `ATAN_TABLE[i + 1]`. When `|y| == |x|` the ratio `r` is
exactly `ONE`, so `i == 1024` and the interpolation reads index 1025 — out of bounds.

**Caught by.** Re-reading the spec while setting up the generator, after noticing I had used a
duplicate sentinel entry for the sine table (4097 entries, not 4096) and had not applied the same
reasoning to `atan`.

**Class: solving a boundary case once and not propagating the fix.** I correctly identified that
interpolation needs an `N+1`th entry when writing the sine table, then wrote three more tables
without re-applying it. The sine case was not a special insight; it was a general property of
"interpolate between entry `i` and `i+1`".

**Aggravating factor.** `|y| == |x|` is 45 degrees. This is not an obscure input — it is one of the
most common angles in any game with axis-aligned movement, so this would have been an immediate
crash rather than a rare one. That is luck, not design: had the boundary been at some unusual ratio,
it would have shipped.

**Rule.** When a fix addresses a *category* of thing (every interpolated table, every quantized
field, every delta-encoded component), immediately enumerate the other members of that category and
check each one. Write the enumeration down rather than trusting recall.

---

## 2026-08-15 — `panic = "abort"` contradicted the ABI spec

**What.** The release profile in the workspace `Cargo.toml` set `panic = "abort"`, copied in as a
routine size-and-speed default. `docs/spec/abi.md` §1 requires every `extern "C"` entry point to
catch unwinding and convert it to an error code. With `panic = "abort"`, `catch_unwind` cannot do
that — the process dies instead, taking the host Python or Node process with it.

**Caught by.** Writing the ABI spec and the Cargo profile in the same session, and noticing the
contradiction while trimming the workspace member list. Nothing tested it, because no ABI code
exists yet.

**Class: a habitual default silently contradicting a deliberate decision.** `panic = "abort"` is the
right default for a standalone binary and the wrong one for a library that will be embedded in five
foreign runtimes. I applied the reflex without checking it against the architecture I had just
finished writing down.

**Rule.** Configuration copied in from habit gets checked against the ADRs and specs, same as code.
Build configuration is a design decision wearing a costume.

---

## 2026-08-15 — Listed workspace members that did not exist

**What.** The initial workspace manifest listed `crates/tempo-wire` and `crates/tempo-core` before
either directory existed, so every `cargo` invocation failed with a manifest error until they were
removed.

**Caught by.** The first `cargo run`.

**Class: writing the finished state instead of the current state.** Harmless here — the failure was
immediate and loud — but it is the same instinct that produces a README documenting features that do
not exist, which is not harmless.

**Rule.** Manifests, indexes and roadmaps describe what is true now. `docs/ROADMAP.md` marks phases
as done, in progress, or planned for exactly this reason, and the same discipline applies to build
files.

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

## 2026-08-15 — Wrote two tests that contradicted the spec they were testing

**What.** Both `sqrt` tests failed on first run. Neither was an implementation bug:

- `sqrt_is_the_exact_floor` asserted `sqrt(2) == Fx::SQRT_2`. But §2.1 specifies `sqrt` as the
  exact **floor**, while §1.1 specifies constants as the **nearest** representable value. For an
  irrational result those differ by up to one ULP, and `sqrt(2)` lands exactly one ULP below the
  constant. The spec was right; my assertion was wrong.
- `sqrt_satisfies_its_defining_inequality` checked `r² ≤ x < (r+1ulp)²` using `Fx` multiplication,
  which itself truncates. The truncation swallowed the difference and the check was simultaneously
  wrong and nearly vacuous.

**Caught by.** Running the tests. Both failed loudly and immediately.

**Class: writing the assertion from intuition rather than from the document open in the next tab.**
"Square root of two should equal the square-root-of-two constant" is what a reasonable person
expects. It is not what the specification says, and I had written that specification an hour
earlier. The test encoded my expectation instead of the requirement.

**Second-order lesson.** A test that checks a rounding rule must not perform its arithmetic in the
type whose rounding is under test. The second failure was not just a wrong expectation — the check
could not have detected the error it was written to catch.

**Rule.** When testing behaviour that a spec pins exactly, quote the spec section in the test and
assert the stated property, in a representation the property is actually stated in. If the test and
the spec disagree, find out which is wrong before changing either.

**Recurrence, same day, `tempo-wire`.** `quantized_bit_widths_are_exact` asserted that a
single-representable-value field needs 1 bit. The comment directly above the assertion said "a
single step needs no bits at all" — I wrote the correct reasoning and then the wrong number on the
next line. `ceil(log2(1))` is 0. Third instance of this class in one session, which is the signal
worth recording: the failure mode is not carelessness about the rule, it is not *checking the
arithmetic* once the rule is settled. The follow-up was to add a codec test for the zero-width
field, since a 0-bit read and write is a real path the bit primitives have to handle.

---

## 2026-08-15 — Built the demo without the client lead, then miscounted the consequence

Two bugs from one end-to-end demo, both invisible to 250 passing unit tests.

**What (first).** The demo had client and server simulating tick *N* at the same wall-clock moment.
So the client's input for tick *N* was sent at *N* and arrived after the server had already
simulated it. The server substituted a default input every tick, the prediction disagreed every
tick, and the client corrected on **199 of 200 snapshots — on a lossless LAN**.

This is precisely what ADR-0016 exists to prevent, and I had written that ADR and implemented
`ClockSync` before writing the demo. I then built the demo without using it.

**What (second).** Fixing the lead exposed the next one. `Predictor::reconcile` treated "the
snapshot's tick is not in my history" as a single case and resynced. But that conflates two
opposite situations: *old news* (a duplicated or delayed snapshot for a tick already confirmed) and
*we have fallen behind*. Resyncing on old news clears history, after which the next snapshot is also
older than everything held — so it resyncs too, and the client never predicts again. A cascade from
one late packet. It showed as 199 resyncs on a 40 ms link.

**Caught by.** Running the demo and reading the numbers, not by any test. Both bugs produced
*plausible* output — the session ran, state stayed synchronised, nothing crashed. Only the
correction and resync counts revealed that prediction was doing no useful work.

**Class: building the integration without using the parts built for it.** Each layer was correct in
isolation and tested in isolation. The demo was the first thing to ask whether they fit together,
and the answer was no — twice. The second bug is a variant of a mistake already in this file
(solving a case and not enumerating its siblings): "not found" had two causes and I handled one.

**What it changed.** `Reconciliation::Stale` is now a distinct outcome from `Resynced`, documented
with why conflating them cascades. The demo derives its lead from `ClockSync` rather than assuming
zero. Inputs are sent with eight-tick redundancy, since an input cannot usefully be retransmitted —
by the time it arrived its tick would have passed. Corrections and resyncs are now zero on every
profile including 300 ms round trip at 10% loss, and the end-to-end test asserts that.

**Rule.** A subsystem is not finished when its unit tests pass; it is finished when something uses
it end to end and the *numbers* are right. Instrument the integration with counters that would look
wrong if the feature were silently doing nothing — "it ran without errors" is not evidence that it
worked.

---

## 2026-08-15 — Reached for TCP's retransmission strategy in a game engine

**What.** Reliable channels retransmitted on a round-trip timeout with exponential backoff — the
TCP/QUIC design, applied without asking whether the workload matched. Two failures, found by one
integration test that dropped every third packet:

1. **Resonance.** A fixed retry interval lands on a periodic multiple. At a 50 ms timeout with
   packets every 20 ms, retries land every 60 ms; a network dropping every third packet drops one
   every 60 ms too. Every single retransmission hit a dropped packet and the channel starved
   *forever* — the test delivered 1 of 20 messages and then stopped.
2. **Backoff was actively harmful.** After adding jitter, delivery worked but took **three seconds**
   for twenty small messages. Doubling from the first retry meant the unluckiest message was waiting
   hundreds of milliseconds, and on an ordered channel everyone waits for the unluckiest one.

**Caught by.** The one test that simulated realistic loss rather than testing a mechanism in
isolation. Forty-two unit tests passed throughout — each verified that retransmission *happened*,
none asked whether it happened *usefully*.

**Class: importing a solution without checking that the problem is the same.** Exponential backoff
exists to prevent congestion collapse during bulk transfer, where the sender is the cause of the
congestion and the correct response is to slow down. A game sending a 4-byte spawn message is not
congesting anything, and gameplay is blocked until it arrives. The workload is inverted, so the
canonical answer is inverted too: retry *faster* than the round trip, not slower, and trade a little
bandwidth for latency.

**What it changed.** Retry interval is now roughly half the round trip with jitter of half again,
and backoff begins only after eight attempts — late enough to distinguish a lossy peer from a
departed one. Twenty messages under 33% loss now clear in well under a second.

**Rule.** When adopting a standard algorithm, write down what it optimises for and check that
against this workload before writing the code. And keep at least one test per subsystem that
simulates adversarial conditions end to end: mechanism tests confirm a thing happens, and only a
realistic one confirms it helps.

---

## 2026-08-15 — Assumed sender and receiver hold identical bytes

**What.** Three separate assertions — a test helper, a long-running sync test, and the crate's
doctest — all checked `server.state_hash() == client.state_hash()` after applying a delta. All
three failed, and all three were wrong.

Quantization is lossy. The sender holds raw values; the receiver holds quantized ones. Their arenas
differ by up to half a step per field **by design**, so their state hashes differ too. That is not
a desync.

**Caught by.** The first delta test failing immediately. The failure was loud, but I had written
the same wrong assumption in three places before running anything.

**Class: a mental model that was almost right.** "Replication makes the peers agree" is true at the
level of gameplay and false at the level of bytes. I had reasoned carefully enough to get the
*implementation* right — `encode_delta` returns `as_sent` precisely because the receiver's state
differs from the sender's — and then wrote tests as though it did not. Getting the hard part right
does not automatically fix the assumptions around it.

**What it changed.** More than the tests. State-hash comparability is now documented on
`World::state_hash` and in the crate docs, because a user will reach for exactly this comparison to
build desync detection and will get a false positive on every quantized field. Hashes are
comparable between peers running the same simulation; they are not comparable between a server's
authoritative world and a client's replicated view.

**Rule.** When three copies of an assertion fail together, fix the belief, not the three
assertions — and check whether the belief is one a user would also hold. If so, it belongs in the
documentation, not just the test.

---

## 2026-08-15 — `live_count` derived from an invariant that `alloc_at` breaks

**What.** `EntityAllocator::live_count` computed `slot_count - free.len()`, assuming every slot not
on the free list is alive. `alloc_at` — which applies a remote spawn at an index the authority
chose — breaks that: claiming slot 150 in an empty allocator creates 150 slots that are neither
alive nor free. A client that received a spawn at index 150 reported 151 live entities instead of 1.

**Caught by.** A delta test asserting the client had 4 entities. It reported 151.

**Class: a derived quantity outliving the invariant it was derived from.** The formula was correct
when `alloc` was the only way to create a slot. `alloc_at` was added later, for a different purpose,
and nothing connected the two. This is the same failure as the `atan` table: solving something for
one case and not revisiting the others.

**Aggravating factor.** This is the ordinary path, not a corner case — every client applying a
remote spawn at a non-contiguous index hits it. It survived because the unit tests for the allocator
exercised `alloc` and `alloc_at` separately and never asked for the count after a sparse claim.

**Rule.** When adding a second way to mutate a structure, list every derived quantity and cached
invariant it touches, and add a test for each. Cheap derived values should be tracked explicitly
rather than recomputed from an assumption.

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

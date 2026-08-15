# Specification: the tempo C ABI

**Status:** normative for ABI version 1. **Implementation lands in Phase 3** — this specifies the
contract the core is being built to satisfy.

Rationale: [ADR-0008](../adr/0008-c-abi-single-boundary.md).

---

## 1. Principles

Five rules constrain every function in this ABI. They exist because the ABI is the one place where a
mistake is paid for six times.

1. **Constant crossings per tick.** No function may exist whose natural use is once per entity or
   once per field. The absence of `tempo_set_field` is a design property, not an oversight.
2. **Opaque handles.** All objects are `uint64_t` handles. No struct layout is public except the
   plain-data descriptor structs in §3.
3. **No unwinding across the boundary.** Every entry point catches panics and converts them to error
   codes.
4. **No hot-path callbacks.** Callbacks exist for lifecycle events only. The core never calls into a
   host language during a replication sweep — doing so would mean acquiring the GIL from inside the
   core.
5. **Views are tick-scoped.** Every view carries the generation it was created in. Use after its tick
   is a diagnosable error, never undefined behaviour.

---

## 2. Errors

```c
typedef int32_t TempoResult;

#define TEMPO_OK                    0
#define TEMPO_ERR_INVALID_HANDLE   -1
#define TEMPO_ERR_INVALID_ARG      -2
#define TEMPO_ERR_STALE_VIEW       -3
#define TEMPO_ERR_SCHEMA_FROZEN    -4
#define TEMPO_ERR_SCHEMA_MISMATCH  -5
#define TEMPO_ERR_NOT_CONNECTED    -6
#define TEMPO_ERR_WOULD_BLOCK      -7
#define TEMPO_ERR_BUFFER_TOO_SMALL -8
#define TEMPO_ERR_INTERNAL       -100

const char* tempo_last_error(void);   // thread-local, valid until the next call on this thread
```

Every function returns `TempoResult`. Out-parameters are written only on `TEMPO_OK`.

---

## 3. Lifecycle

```c
uint32_t tempo_abi_version(void);       // major<<16 | minor — checked by every binding at load

TempoResult tempo_session_create(const TempoConfig* cfg, uint64_t* out_session);
TempoResult tempo_session_destroy(uint64_t session);
```

`TempoConfig` is a plain C struct with an explicit `size` field as its first member, so fields may be
appended without breaking existing callers — the standard extensible-struct pattern.

---

## 4. Schema registration

```c
typedef struct {
    const char* name;
    uint32_t    type;          // TEMPO_TYPE_*
    uint32_t    flags;         // which optional parameters are present
    int64_t     quantize, min, max;   // raw Fx values
    uint32_t    bits, variants, max_len;
    float       base_priority;
} TempoFieldDesc;

TempoResult tempo_register_component(uint64_t session, const char* name,
                                     const TempoFieldDesc* fields, size_t field_count,
                                     uint32_t* out_component_id);

TempoResult tempo_schema_id(uint64_t session, uint8_t out_id[16]);
TempoResult tempo_schema_canonical(uint64_t session, char* buf, size_t buf_len, size_t* out_len);
```

Bindings translate their native declaration into `TempoFieldDesc` arrays. Canonicalisation and
hashing happen **once, in Rust** — bindings never implement §3 of
[schema-and-hashing.md](schema-and-hashing.md) themselves, which removes six opportunities to
canonicalise differently.

---

## 5. The tick cycle

This is the hot path, and its shape is the whole point of the ABI.

```c
TempoResult tempo_begin_tick(uint64_t session, uint64_t* out_frame);

// Zero-copy read: base pointer, element stride, count. Iteration is pointer
// arithmetic in the host language; there is no per-element call into Rust.
TempoResult tempo_view_component(uint64_t frame, uint32_t component_id,
                                 TempoView* out_view);

typedef struct {
    const uint8_t* base;
    size_t         stride;
    size_t         count;
    const uint32_t* entity_indices;
    uint64_t       generation;      // must equal the frame's generation
} TempoView;

// Writes are staged into a command buffer owned by the frame and applied at end_tick.
TempoResult tempo_stage_writes(uint64_t frame, uint32_t component_id,
                               const uint32_t* entity_indices,
                               const uint8_t* data, size_t count);

TempoResult tempo_end_tick(uint64_t frame);
```

A tick therefore costs a constant number of boundary crossings — roughly `2 + components_touched` —
independent of entity count. This is the property ADR-0001 exists to preserve, and any proposed ABI
addition that breaks it is rejected on that basis alone.

---

## 6. Events and I/O

```c
TempoResult tempo_poll_events(uint64_t session, TempoEvent* buf, size_t cap, size_t* out_count);
TempoResult tempo_send(uint64_t session, uint32_t channel, const uint8_t* data, size_t len);
```

Events — connect, disconnect, spawn, despawn, rollback, desync, schema mismatch — are **polled in
batches**, not delivered by callback. Polling keeps control flow in the host language and keeps the
core free of foreign-runtime concerns.

---

## 7. Threading

- A session is **not** internally synchronised. Calls on one session must be externally serialised.
- Distinct sessions are independent and may be driven from different threads.
- `tempo_last_error` is thread-local.
- Network I/O runs on core-owned threads; the boundary between them and the caller is inside the
  core, not exposed here.

---

## 8. Header stability

`tempo.h` is generated by `cbindgen`, committed, and diffed in CI. An unintended ABI change appears
in review as a header diff rather than as a downstream binding crash.

Within a major ABI version, changes are strictly additive: functions are never removed or
resignatured, enum values are never reused, and struct fields are appended only after a `size` field.

# Architecture

Technical reference for `minicache`. The README covers usage; this covers how it works and why it is built this way.

---

## 1. What this is

An in-process cache whose storage lives in POSIX shared memory, so several processes on one host share a single cache with no server, no daemon, and no IPC round trip. A read or write is a hash, a bounds check, and a memcpy — there is no other process involved in servicing it.

The design target is **many processes, small values, high read rate, tolerant of loss**. Everything below follows from that.

## 2. Why the obvious implementation cannot work

A conventional Rust cache is a `HashMap<Bytes, Bytes>` behind a lock. None of that can be placed in shared memory.

Each process maps the shared region at a **different virtual address**. A `HashMap` stores pointers to heap allocations in the process that created them; `Vec` stores a pointer, length and capacity; `Bytes` stores a pointer and a refcount. A second process following any of those pointers reads unrelated memory in its own address space.

So the entire contents of the mapping must be:

- `#[repr(C)]`, with a layout that does not vary by compiler version or platform
- free of pointers — anything referring to something else does so by **offset or index**
- valid when interpreted from a zero-filled region, since that is how it begins life

This is the constraint that shapes every other decision here. It is also why there is no LRU: an intrusive linked list is expressible with indices, but maintaining it requires mutating shared state on every *read*, which would mean readers taking locks. See §9.

## 3. Storage: POSIX shared memory

The cache is identified by an **app name**, not a filesystem path. `ShmCache::open("myapp", 4096)` resolves the storage internally via `shm_open(2)`.

### Why not a path

A path on a normal filesystem makes the cache a durable file, which brings two costs nobody wants for cache data:

- **Writeback.** The kernel periodically flushes dirty pages to disk. A hot cache dirties pages continuously, producing a steady stream of disk writes for data that is by definition disposable, plus SSD write amplification.
- **A tail-latency cliff.** Under memory pressure the kernel may evict a clean page, knowing it can re-read it from the file. The next cache *hit* on that slot then takes a major page fault and blocks on disk. Throughput benchmarks look unchanged; p99 collapses.

### Why not a tmpfs path

Because there is no portable one.

| Platform | tmpfs | `/dev/shm` | `/tmp` |
| :--- | :--- | :--- | :--- |
| Linux | yes | yes | usually tmpfs |
| macOS | **none** | **none** | APFS data volume — a real disk |

macOS has no tmpfs at all. Notably `/tmp` on macOS is *not* RAM either, so a naive "just point it at `/tmp`" default is disk-backed on macOS while being RAM-backed on Linux — the same code silently behaving differently per platform.

`shm_open` is the one interface that is RAM-backed on both. A name is therefore the only identifier that can mean the same thing everywhere, which is why the API takes one.

### What the name maps to

| Platform | Backing | Visible as |
| :--- | :--- | :--- |
| Linux | tmpfs | a file under `/dev/shm/<name>` |
| macOS | kernel object | nothing in the filesystem |

Neither reaches disk. Neither survives a reboot.

### Name constraints

At most `MAX_APP_NAME_LEN` (30) bytes of `[A-Za-z0-9._-]`.

POSIX requires a leading `/` and forbids any other. macOS caps the whole name at 31 bytes including that slash and rejects longer ones, so the limit is 30 and it is enforced uniformly — a name that works on one platform works on the other. Over-long names are **rejected**, never truncated: silently truncating would map two different apps onto one cache.

### Lifetime

A shared-memory object **outlives every process that used it**. It is reclaimed only by `ShmCache::unlink` or a reboot.

That is what allows a restarted process to attach to a warm cache. It also means a long-running host accumulates objects if nothing ever unlinks them, so tests, benchmarks and the example all unlink what they create.

## 4. Memory layout

```
┌──────────────────────────────────────────────┐
│ Header                          64 bytes     │
│   magic      u64    "MINICACH"               │
│   version    u32    format version           │
│   num_slots  u32    authoritative slot count │
│   ready      u32    init handshake           │
│   _pad       44 bytes                        │
├──────────────────────────────────────────────┤
│ Slot[0]                       1152 bytes     │
│   seq        u64    seqlock                  │
│   key_len    u32                             │
│   val_len    u32                             │
│   key        [u8; 64]                        │
│   val        [u8; 1024]                      │
├──────────────────────────────────────────────┤
│ Slot[1]                                      │
│ …                                            │
│ Slot[num_slots-1]                            │
└──────────────────────────────────────────────┘
```

Both structs are `#[repr(C, align(64))]`. The alignment is one cache line, so no two slots share a line and concurrent writers to different slots do not false-share.

`Slot` is 1104 bytes of fields rounded up to **1152** by the alignment. Total size is `64 + num_slots × 1152`:

| Slots | Size |
| ---: | ---: |
| 256 | 288 KB |
| 4096 | 4.5 MB |
| 65536 | 72 MB |

Keys and values are stored **inline**, not indirected. This wastes space on short values — every slot costs 1152 bytes regardless — but it removes the need for an allocator inside the mapping, which would itself have to be lock-free, crash-safe and pointer-free. Fixed slots are the simplest thing that satisfies §2.

### The zero-filled invariant

A freshly created object is all zeroes, and that must be a **valid empty cache**:

- `seq == 0` → even → unlocked
- `key_len == 0` → matches no key (an empty key cannot be written) → every lookup misses

So no slot initialisation pass is needed. The creator sizes the object and writes only the header.

## 5. Initialisation protocol

Several processes may call `open` simultaneously on a cache that does not exist. Exactly one must create and size it; the rest must attach without corrupting it.

```
     shm_open(O_CREAT|O_EXCL)
        │
   ┌────┴─────┐
  won        EEXIST
   │            │
   │       shm_open(O_RDWR)
   │            │
ftruncate       wait for size ≥ sizeof(Header)
   │            │
   └────┬───────┘
        │
      mmap
        │
  CAS magic: 0 → MAGIC
        │
   ┌────┴────┐
  won       lost
   │          │
 write     wait for ready == 1
 header       │
 ready=1   adopt header.num_slots
```

Two separate races, resolved by two different mechanisms:

**Creation** is resolved by `O_CREAT | O_EXCL`. The winner is the only process permitted to size the object.

This matters because of a macOS constraint: **`ftruncate` on a shared-memory object may be called exactly once, and only before the object is mapped.** A second call fails with `EINVAL`. Winning the exclusive create is what guarantees no other process has mapped it yet. A joiner therefore never resizes anything — if the mapping turns out to be smaller than the header claims, that is a hard error rather than something to repair.

A joiner can arrive after the object exists but before the creator has sized it, so it waits for the size to become non-zero before mapping.

**Header initialisation** is resolved by a compare-exchange on `magic`, exploiting the zero-fill: `0` means uninitialised. The winner fills in `version` and `num_slots`, then publishes `ready = 1` with a `Release` store so that any process observing `ready == 1` with an `Acquire` load also sees a complete header.

### Slot count is owned by the creator

`num_slots` passed to `open` is honoured **only** if that call creates the cache. A joiner adopts `header.num_slots` and ignores its own argument. `capacity()` reports what was actually adopted.

This is not cosmetic. Slot selection is `hash(key) % num_slots`, so two processes using different moduli hash the same key to different slots and **silently stop sharing**. Nothing errors; every read simply misses. That is close to the worst possible failure mode, because it looks like an ordinary cold cache.

> This was a real bug, not a hypothetical. `open` stored the caller's value rather than the header's, and two handles on the same object with different counts saw nothing of each other. Fixed, with `existing_slot_count_wins_over_the_callers` guarding it.

## 6. Hashing

FNV-1a, 64-bit, implemented inline.

The requirement is that **the hash be identical in every process**. `std::collections::hash_map::RandomState` is seeded per process from the OS; using it would give each process a different mapping from key to slot, reproducing exactly the silent non-sharing failure of §5. Any `Hasher` chosen here must be deterministic and stable across processes, builds and platforms.

FNV-1a is not the fastest or best-distributed choice, but at 64-byte keys it is a handful of multiplies and it has no state to seed. Slot selection is `fnv1a(key) % num_slots`, so `num_slots` need not be a power of two.

## 7. Concurrency: a seqlock per slot

There is one lock per slot, encoded in `seq`:

- **even** — stable, no writer
- **odd** — a writer holds the slot

### Write

```
1. spin/CAS seq from even S to S+1          (Acquire)   take the slot
2. write key_len, val_len, key, val                     payload
3. store seq = S+2                          (Release)   publish
```

The `Acquire` on acquisition prevents the payload writes from being hoisted above the lock. The `Release` on publication guarantees that any reader observing `S+2` also observes every payload byte.

Payload writes go through **raw pointers**, never `&mut`. Another process may be reading these bytes concurrently, so forming a `&mut` would assert an exclusivity that does not exist and is undefined behaviour under Rust's aliasing rules.

### Read

```
1. load seq → S                             (Acquire)
2. if S is odd, back off and retry
3. read key_len/val_len, bounds-check them
4. copy key and value into local buffers
5. fence                                    (Acquire)
6. load seq again; if ≠ S, discard and retry
7. compare key; return the copy
```

Steps 4–6 are the heart of it. Everything read in steps 3–4 is **untrusted** until step 6 confirms the slot did not move underneath the sample. The fence at step 5 stops the copies from sinking below the re-read.

Two consequences worth stating explicitly:

**Lengths must be bounds-checked before use (step 3).** A torn read can yield an arbitrary `key_len`/`val_len` — those fields are being written concurrently. Using an unchecked length to form a slice turns a benign torn read into an out-of-bounds read. They are validated against `MAX_KEY_LEN`/`MAX_VAL_LEN` before any slice exists.

**Readers never write to shared memory.** The read path is pure load-and-copy. A reader that crashes at any point cannot damage the cache, and readers do not contend with each other at all.

### Why reads copy

`read` returns an owned `Bytes` rather than a slice into the mapping. The copy is not incidental — it is what makes step 6 meaningful. If a borrowed slice were returned, the re-check would validate bytes the caller had not yet looked at, and another process could rewrite them at any moment afterwards.

The alternative that preserves zero-copy is a **write-once arena**: never mutate a published record, append a new one and swap an atomic `(generation, offset)` descriptor. Readers can then safely borrow, because published bytes are immutable until the arena wraps. It was rejected for v1 because it brings back an arena-sizing knob, wraparound staleness, a re-validation obligation on every call site, and a slice aliasing memory another process may write — technically UB even when it works in practice.

For sub-1KB values the memcpy is a small part of a read (§11), and most callers copy anyway. If values grow past a few KB this trade-off should be revisited; the arena is the fallback.

## 8. Failure model

The guiding assumption: **a process using this cache may die at any instant, including mid-write.** A design that can be permanently wedged by one `kill -9` is not usable.

### No process-shared mutexes

`pthread_mutex` with `PTHREAD_PROCESS_SHARED` is the textbook primitive and is deliberately not used. A process that dies holding one leaves it locked forever, and robust mutexes (`pthread_mutexattr_setrobust`) are not portable to macOS.

The seqlock has no such failure mode by construction — a dead writer leaves a single slot with an odd `seq`, and nothing else is affected.

### Bounded waits, measured on the clock

Both sides give up after a bounded wait rather than spinning forever:

| | Timeout | On expiry |
| :--- | :--- | :--- |
| Writer | 50 ms | `Err(SlotStalled)` |
| Reader | 5 ms | `None` (a miss) |

The reader's bound is shorter because a reader may legitimately give up: a spurious miss is valid cache behaviour, whereas a spurious write failure loses data.

The waits are **wall-clock bounded, not iteration bounded**, and this distinction is load-bearing:

> The first implementation spun a fixed 1024 iterations. That cannot distinguish a writer that *died* from one that was merely *descheduled* — the OS can preempt a lock-holding process for milliseconds. Under ordinary three-way write contention this failed roughly 8% of runs with `SlotStalled` on a perfectly healthy cache. Verified across 25 runs, fixed, then verified again across 60.

Backoff escalates: `SPIN_ATTEMPTS` (64) iterations of `spin_loop`, then `yield_now`. Yielding matters because the lock holder may be a descheduled process on this very core, where continued spinning actively prevents it from making progress. The clock is only consulted after the spin phase, so an uncontended slot never pays for `Instant::now()`.

### Known limitation: a stalled slot is not recovered

If a writer dies mid-write, its slot keeps an odd `seq` **permanently**. Writers to that slot get `SlotStalled`; readers get a miss. One slot out of `num_slots` is dead.

Because a shared-memory object survives process exit, this persists until the object is unlinked or the machine reboots — it is not cleared by restarting the application.

The fix is for a writer to *steal* the lock once the timeout expires rather than fail. This is safe here because `write` always replaces the key and value completely, so a stolen slot is fully overwritten rather than left half-formed. Not yet implemented.

## 9. Eviction

There is none, in the usual sense. Slots are **direct-mapped**: `hash(key) % num_slots` selects exactly one slot, and writing a key evicts whatever occupied it.

Consequences:

- Capacity is exactly `num_slots` entries, but the *effective* capacity is lower because of hash collisions — two hot keys landing in one slot will evict each other repeatedly.
- There is no LRU, LFU, or TTL. Recency is not tracked at all.
- A lookup is O(1) with no probing: one slot is examined, and a key mismatch is a miss.

An LRU would require an intrusive doubly-linked list over slot indices, and every *read* would have to move its entry to the head — mutating shared state, which means readers taking write locks. That destroys the two properties this design is built on: readers never writing, and a crashed reader being harmless. CLOCK or a sampled approximation is the realistic path to better hit rates, since both can be updated with a single relaxed store on a per-slot bit rather than a global list.

## 10. Limits

| Constant | Value | Notes |
| :--- | ---: | :--- |
| `MAX_APP_NAME_LEN` | 30 | macOS caps shm names at 31 including the leading `/` |
| `MAX_KEY_LEN` | 64 | inline in the slot |
| `MAX_VAL_LEN` | 1024 | inline in the slot |
| `SPIN_ATTEMPTS` | 64 | before yielding |
| `WRITE_LOCK_TIMEOUT` | 50 ms | then `SlotStalled` |
| `READ_LOCK_TIMEOUT` | 5 ms | then miss |

Keys and values over the limit are rejected with `KeyTooLong` / `ValueTooLong`. They are never truncated.

Platform support is **Unix only**; Windows fails with a `compile_error!`, since it has no POSIX shared memory.

## 11. Performance

Apple Silicon, 4096 slots, 256-byte values, uncontended, single-threaded.

| Operation | Latency | Throughput |
| :--- | ---: | ---: |
| write (overwrite) | 9.8 ns | ~102M ops/sec |
| read (hit) | 46 ns | ~22M ops/sec |
| read (miss) | 4.2 ns | ~240M ops/sec |
| read (hit, spread over 4096 keys) | 54 ns | ~18M ops/sec |

### Reads are dominated by fixed overhead, not the copy

A read hit costs **4.7× a write**, despite both performing the same 256-byte memcpy. Benchmarking across value sizes isolates why:

| Value size | Read hit |
| ---: | ---: |
| 8 B | 46 ns |
| 256 B | 46 ns |
| 1024 B | 64 ns |

Flat from 8 B to 256 B. Roughly 46 ns is **fixed cost independent of value size**, so it is not the memcpy. Two candidates, both avoidable and not yet separated by measurement:

- the heap allocation inside `Bytes::copy_from_slice`
- zeroing the 1 KB stack buffer on every read

A `read_into(&mut [u8]) -> Option<usize>` variant would sidestep both and should land near write speed. The `shm_read_hit_by_size` benchmark exists to verify that if implemented.

Note this does **not** undermine the copy-on-read decision of §7 — the copy itself is cheap. It is the allocation wrapped around it that is not.

### Shared memory does not show up in these numbers

These figures are within noise of the same benchmarks against a disk-backed file, because a microbenchmark keeps every page resident in the page cache and never touches the disk. The reasons for shared memory in §3 — no writeback, no page-fault stalls under memory pressure — are invisible to a benchmark by construction.

## 12. Testing

### Two real processes

`tests/shm_ipc.rs` spawns actual OS processes via `src/bin/shm_worker.rs`, located through `env!("CARGO_BIN_EXE_shm_worker")`.

Threads would prove nothing. They share an address space, so a thread-based test passes even if the design accidentally depends on process-local state — which it very nearly did, twice: the `RandomState` hashing trap of §6 and the slot-count bug of §5 are both invisible to threads.

### Self-checking values

Every value written by the worker embeds a counter, a checksum over itself, and a payload derived from the counter. A torn read therefore **fails its checksum** rather than passing as a plausible byte string. The reader asserts every observed value is intact and that it reaches the writer's final counter.

A representative run: 148,208 hits, 146,003 distinct values observed across the process boundary, zero torn reads.

### The test is verified to be able to fail

A test for a race that has never failed proves nothing. Disabling the reader's `seq` re-check (§7 step 6) makes it fail within 38 hits, confirming it genuinely detects tearing rather than passing vacuously.

### Coverage of the failure modes above

| Test | Guards |
| :--- | :--- |
| `two_processes_share_one_key_without_torn_reads` | seqlock correctness across processes |
| `concurrent_writers_race_to_create_and_contend` | creation race + multi-writer contention (§8) |
| `reader_starting_late_still_sees_the_final_value` | object lifetime beyond process exit |
| `existing_slot_count_wins_over_the_callers` | the §5 silent non-sharing bug |
| `joining_smaller_keeps_every_slot_reachable` | joiner adopting the wrong modulus |
| `nothing_is_written_to_disk` | regression to path-backed storage |
| `rejects_unusable_app_names` | name validation, including exact-limit acceptance |
| `unlink_starts_the_next_open_fresh` | teardown semantics |
| `threads_never_observe_a_torn_value` | fast single-process seqlock check |

## 13. Open work

Ordered by how much they matter.

1. **Recover stalled slots** (§8). The only outright bug remaining. A slot lost to a crashed writer stays dead until the object is unlinked.
2. **`read_into`** (§11). Removes the allocation that dominates reads; likely a 4–5× improvement on the hot path.
3. **A real eviction policy** (§9). CLOCK, or sampled-LRU. Direct-mapped collisions currently cap the achievable hit rate.
4. **Configurable key/value limits.** They are compile-time constants inline in `Slot`; making them per-cache means sizing slots at creation and recording it in the header.
5. **Linux validation.** All development and measurement was on macOS. Linux is exercised only by CI.


<div align="center">

# minicache
![Build Status](https://img.shields.io/github/actions/workflow/status/imduchuyyy/minicache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-GPL-blue.svg)

**A shared-memory cache library in Rust.**

</div>

`minicache` is a library you embed directly in your application. Several processes on the same host name the same cache and share it — there is no server to run and no network hop.

## Features

- **🔗 Cross-Process**: processes share one cache over shared memory, with no daemon and no IPC round trip.
- **🧠 Pure RAM**: backed by POSIX shared memory, never a file on disk. No writeback, no page-fault stalls, nothing left behind after a reboot.
- **🔓 Lock-Free**: a seqlock per slot. No process-shared mutexes, so a process that dies mid-write cannot poison or deadlock the cache.
- **🛡 Crash-Safe Reads**: readers never write to shared memory, so a crashed reader cannot damage anything.
- **🪶 Tiny Dependency Tree**: `bytes`, `memmap2`, `libc`. That's it.

## Usage

You give the cache a name, not a path. Every process that uses the same name shares one cache, and whichever opens it first creates it.

```rust
use minicache::ShmCache;

let cache = ShmCache::open("myapp", 256)?; // app name, slot count

cache.write(b"mykey", b"myvalue")?;
assert_eq!(cache.read(b"mykey").as_deref(), Some(&b"myvalue"[..]));

ShmCache::unlink("myapp")?; // destroy it; otherwise it lives until reboot
# Ok::<(), minicache::Error>(())
```

The name must be at most 30 bytes of alphanumerics, `-`, `_`, or `.` — that is what macOS accepts for a shared-memory object.

Slots are direct-mapped by hash, so a collision overwrites the previous occupant — ordinary cache behaviour, but note there is **no LRU ordering**. Keys are capped at `MAX_KEY_LEN` (64 bytes) and values at `MAX_VAL_LEN` (1 KB).

`read` copies the value out of the mapping. That copy is what lets it verify it got a consistent snapshot rather than a half-written one; see the `ShmCache` docs for the seqlock details.

### Where the memory actually lives

The cache is a POSIX shared-memory object, so it is RAM on both platforms — but they expose it differently:

| | Backing | Visible as |
| :--- | :--- | :--- |
| **Linux** | tmpfs | a file under `/dev/shm/` |
| **macOS** | kernel object | nothing in the filesystem |

macOS has no tmpfs and no `/dev/shm`, which is why the API takes a name rather than a path — there is no single path that means "RAM" on both.

The object **outlives the processes that use it** and is only reclaimed by `ShmCache::unlink` or a reboot. That is what lets a restarted process pick up a warm cache, but it also means a long-running box accumulates them if you never unlink.

Unix only: it will not compile on Windows.

## Benchmarks

**Environment**: `cargo bench`, MacBook Pro (Apple Silicon), 4096 slots, 256-byte values, uncontended.

| Operation | Latency | Throughput |
| :--- | :--- | :--- |
| **write** (overwrite) | **9.8 ns** | ~102M ops/sec |
| **read** (hit) | **46 ns** | ~22M ops/sec |
| **read** (miss) | **4.2 ns** | ~240M ops/sec |
| **read** (hit, spread over 4096 keys) | **54 ns** | ~18M ops/sec |

Read hits cost the same at 8-byte and 256-byte values (~46 ns) and only rise at 1 KB (~64 ns), so most of that number is fixed per-read overhead rather than the copy itself. Writes, which do the same memcpy, run at 9.8 ns.

These are within noise of the same benchmarks against a disk-backed file, because a benchmark keeps every page resident in the page cache and so never actually touches the disk. Shared memory is not chosen here for throughput — it is chosen to remove periodic writeback of a cache nobody wants durable, and to remove the tail-latency cliff where the kernel evicts a page under memory pressure and a later cache *hit* has to block on disk to fault it back in. Neither of those shows up in a microbenchmark.

## Architecture

[ARCHITECTURE.md](ARCHITECTURE.md) documents the memory layout, the seqlock protocol and its memory ordering, the initialisation races, the failure model, and the measured performance characteristics.

## Testing

```bash
cargo test
```

`tests/shm_ipc.rs` spawns real OS processes that read and write the same key concurrently. Threads would share an address space and prove nothing, so these are separate processes sharing only the mapping. Every value is self-checking, so a torn read fails its checksum rather than passing as plausible bytes.

## License

This project is licensed under the GPL License.

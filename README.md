
<div align="center">

# minicache
![Build Status](https://img.shields.io/github/actions/workflow/status/imduchuyyy/minicache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-GPL-blue.svg)

**A shared-memory cache library in Rust.**

</div>

`minicache` is a library you embed directly in your application. Several processes on the same host open the same memory-mapped file and share one cache — there is no server to run and no network hop.

## Features

- **🔗 Cross-Process**: processes share one cache over `mmap`, with no daemon and no IPC round trip.
- **🔓 Lock-Free**: a seqlock per slot. No process-shared mutexes, so a process that dies mid-write cannot poison or deadlock the cache.
- **🛡 Crash-Safe Reads**: readers never write to shared memory, so a crashed reader cannot damage anything.
- **🪶 Tiny Dependency Tree**: `bytes` and `memmap2`. That's it.

## Usage

Every process opens the same path and gets the same cache. Whichever process opens it first creates it.

```rust
use minicache::ShmCache;

let cache = ShmCache::open("/tmp/mycache.shm", 256)?; // path, slot count

cache.write(b"mykey", b"myvalue")?;
assert_eq!(cache.read(b"mykey").as_deref(), Some(&b"myvalue"[..]));
# Ok::<(), minicache::Error>(())
```

Slots are direct-mapped by hash, so a collision overwrites the previous occupant — ordinary cache behaviour, but note there is **no LRU ordering**. Keys are capped at `MAX_KEY_LEN` (64 bytes) and values at `MAX_VAL_LEN` (1 KB).

`read` copies the value out of the mapping. That copy is what lets it verify it got a consistent snapshot rather than a half-written one; see the `ShmCache` docs for the seqlock details.

## Benchmarks

**Environment**: `cargo bench`, MacBook Pro (Apple Silicon), 4096 slots, 256-byte values, uncontended.

| Operation | Latency | Throughput |
| :--- | :--- | :--- |
| **write** (overwrite) | **9.9 ns** | ~101M ops/sec |
| **read** (hit) | **47 ns** | ~21M ops/sec |
| **read** (miss) | **4.1 ns** | ~240M ops/sec |
| **read** (hit, spread over 4096 keys) | **55 ns** | ~18M ops/sec |

Read hits cost the same at 8-byte and 256-byte values (~47 ns) and only rise at 1 KB (~63 ns), so most of that number is fixed per-read overhead rather than the copy itself. Writes, which do the same memcpy, run at 9.9 ns.

## Testing

```bash
cargo test
```

`tests/shm_ipc.rs` spawns real OS processes that read and write the same key concurrently. Threads would share an address space and prove nothing, so these are separate processes sharing only the mapping. Every value is self-checking, so a torn read fails its checksum rather than passing as plausible bytes.

## License

This project is licensed under the GPL License.

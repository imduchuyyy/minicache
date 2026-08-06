
<div align="center">

# minicache
![Build Status](https://img.shields.io/github/actions/workflow/status/imduchuyyy/minicache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-GPL-blue.svg)

**A high-performance, memory-optimized cache library in Rust.**

</div>

`minicache` is a library you embed directly in your application — there is no server and no network hop. It offers two caches:

- **`LruCache` / `ShardedCache`** — in-process, single application, zero-copy reads.
- **`ShmCache`** — backed by a memory-mapped file, so several processes on the same host share one cache.

## Features

- **🚀 High Performance**: Extremely fast operations (~16ns for updates) using optimized data structures.
- **💾 Memory Efficient**: Uses `u32` indices instead of pointers (`usize`) to reduce memory footprint by up to 50% on 64-bit systems.
- **⚡️ Zero Copy**: `LruCache` reads hand back a `Bytes` view rather than copying.
- **🔗 Cross-Process**: `ShmCache` shares one cache between processes over `mmap`, with no server to run.
- **🪶 Tiny Dependency Tree**: `bytes` and `memmap2`. That's it.

## Benchmarks

I ran a benchmark on my local machine to test the raw throughput of the cache implementation (excluding network overhead). The results are impressive:

**Environment**: `cargo run --release`, MacBook Pro (Apple Silicon).
**Parameters**: 500k Capacity, 1M Operations.

| Operation | Throughput | Latency (Total) |
| :--- | :--- | :--- |
| **PUT** (Insert) | **~5.38 Million ops/sec** | 185ms (for 1M items) |
| **GET** (Hit) | **~12.09 Million ops/sec** | 8.27ms (for 100k items) |
| **GET** (Miss) | **~6.98 Million ops/sec** | 14.33ms (for 100k items) |

The use of `Bytes` and the index-based approach yields massive performance benefits, making this simple implementation competitive with production-grade systems.

## Usage

### In-process cache

```rust
use bytes::Bytes;
use minicache::ShardedCache;

let cache = ShardedCache::new(1000, 16); // capacity, shards
cache.put(Bytes::from("mykey"), Bytes::from("myvalue"));
assert_eq!(cache.get(&Bytes::from("mykey")), Some(Bytes::from("myvalue")));
```

### Shared between processes

Every process opens the same path and gets the same cache. Whichever process opens it first creates it.

```rust
use minicache::shm::ShmCache;

let cache = ShmCache::open("/tmp/mycache.shm", 256)?; // path, slot count

cache.write(b"mykey", b"myvalue")?;
assert_eq!(cache.read(b"mykey").as_deref(), Some(&b"myvalue"[..]));
# Ok::<(), minicache::shm::Error>(())
```

Slots are direct-mapped by hash, so a collision overwrites the previous occupant — ordinary cache behaviour, but note there is no LRU ordering here. Keys are capped at `MAX_KEY_LEN` (64 bytes) and values at `MAX_VAL_LEN` (1 KB).

`read` copies the value out of the mapping. That copy is what lets it verify it got a consistent snapshot rather than a half-written one; see the module docs in `src/shm.rs` for the seqlock details.

There are no process-shared mutexes, so a process that dies mid-write cannot poison or deadlock the cache, and readers never write to shared memory at all.

## Testing

```bash
cargo test
```

`tests/shm_ipc.rs` spawns real OS processes that read and write the same key concurrently. Threads would share an address space and prove nothing, so these are separate processes sharing only the mapping. Every value is self-checking, so a torn read fails its checksum rather than passing as plausible bytes.

## License

This project is licensed under the GPL License.

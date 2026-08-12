# shmcache
![build](https://img.shields.io/github/actions/workflow/status/imduchuyyy/shmcache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-GPL--3.0-blue)

`shmcache` is a Rust crate for sharing a small key/value cache between processes on the same host, using POSIX shared memory, a per-slot seqlock, and no daemon in the middle.

You embed it in your application rather than running it. Several processes open the same cache by name and they are all looking at the same memory, so there is no server to start, no socket to connect to, and no IPC round trip on the hot path.

## Overview
The whole interface is one struct with four methods. You give it an application name and a slot count, and whichever process gets there first creates the cache.

```rust
impl ShmCache {
    /// Open the cache named `app_name`, creating it if nobody has yet.
    pub fn open(app_name: &str, num_slots: usize) -> Result<Self, Error> {
        …
    }

    /// Store `value` under `key`, replacing whatever occupied the slot.
    pub fn write(&self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        …
    }

    /// Copy the value for `key` out of shared memory, if it is there.
    pub fn read(&self, key: &[u8]) -> Option<Bytes> {
        …
    }

    /// Destroy the shared memory object. Otherwise it lives until reboot.
    pub fn unlink(app_name: &str) -> Result<(), Error> {
        …
    }
}
```

Slots are direct-mapped by hash, so writing a key whose hash lands on an occupied slot evicts the previous occupant. That is ordinary cache behaviour, but worth saying out loud: there is no LRU ordering and no chaining. Keys are capped at `MAX_KEY_LEN` (64 bytes), values at `MAX_VAL_LEN` (1 KB), and the name at `MAX_APP_NAME_LEN` (30 bytes of alphanumerics, `-`, `_`, or `.`, which is what macOS accepts for a shared memory object).

## Shared Memory
The cache lives in a POSIX shared memory object, which is RAM on both supported platforms, though they expose it rather differently:

| | Backing | Visible as |
| :--- | :--- | :--- |
| **Linux** | tmpfs | a file under `/dev/shm/` |
| **macOS** | kernel object | nothing in the filesystem |

macOS has no tmpfs and no `/dev/shm`, so there is no single path that means "RAM" on both systems. That is why the API takes a name instead of a path.

Because it is shared memory and not a file, nothing is ever written back to disk. No writeback, no page-fault stalls to fetch a page the kernel evicted, and nothing left over after a reboot. The object does outlive the processes that mapped it, though, and is only reclaimed by `ShmCache::unlink` or a reboot. That is what lets a restarted process pick up a warm cache, but it also means a long-lived machine will accumulate abandoned caches if you never unlink them.

Unix only. It will not compile on Windows.

## Lock-free Synchronization
Each slot carries its own seqlock: the writer bumps a sequence counter to an odd value, writes the key and value, then bumps it to even again. A reader takes the counter before and after copying the slot out, and only trusts what it read if the two agree and the value was even.

There are no process-shared mutexes anywhere, which matters far more across processes than it does across threads: a `pthread_mutex` held by a process that gets killed stays locked forever, and robust mutexes aren't available on macOS. With a seqlock the damage is contained instead. A writer killed mid-write leaves one slot with an odd counter, so readers of that key get a miss and writers of it get `Error::SlotStalled`. One slot out of `num_slots` is dead; the rest of the cache carries on. Nothing is poisoned and nothing deadlocks. (Recovering such a slot is not implemented yet, and is the known limitation in §8 of the architecture doc.)

Readers never write to shared memory at all, so a crashed or misbehaving reader cannot damage the cache for anyone else.

The copy in `read` is not an oversight, it is the point: you cannot hand out a reference into a slot that a writer may overwrite while you hold it. Copying first is what makes the after-the-fact sequence check meaningful. [ARCHITECTURE.md](ARCHITECTURE.md) has the memory ordering, the initialisation races, and the failure model in full.

## Getting Started
Add it to your `Cargo.toml` under `[dependencies]`:

```toml
[dependencies]
shmcache = "0.1"
```

Then import it:

```rust
use shmcache::ShmCache;
```

A complete program, which is also [example/main.rs](example/main.rs):

```rust
use shmcache::ShmCache;

const APP: &str = "shmcache-demo";

fn main() -> Result<(), shmcache::Error> {
    let cache = ShmCache::open(APP, 256)?;
    println!("opened {APP:?} with {} slots", cache.capacity());

    cache.write(b"hello", b"world")?;

    match cache.read(b"hello") {
        Some(value) => println!("hello -> {}", String::from_utf8_lossy(&value)),
        None => println!("hello -> miss"),
    }

    ShmCache::unlink(APP)?;
    Ok(())
}
```

Run it with `cargo run --example hello`, and you should see:

```shell
opened "shmcache-demo" with 256 slots
hello -> world
```

The interesting part is what happens when you drop the `unlink` and run it from two different processes: the second one prints `hello -> world` without ever having written anything.

Dependencies are `bytes`, `memmap2`, and `libc`. That is the entire tree.

## Benchmarks
```shell
cargo bench
```

Measured with `cargo bench` on a MacBook Pro (Apple Silicon), 4096 slots, 256-byte values, uncontended:

| Operation | Latency | Throughput |
| :--- | :--- | :--- |
| **write** (overwrite) | **9.8 ns** | ~102M ops/sec |
| **read** (hit) | **46 ns** | ~22M ops/sec |
| **read** (miss) | **4.2 ns** | ~240M ops/sec |
| **read** (hit, spread over 4096 keys) | **54 ns** | ~18M ops/sec |

A read hit costs about the same at 8 bytes as at 256 bytes, and only creeps up to ~64 ns at 1 KB, so most of that number is fixed per-read overhead rather than the copy. Writes do the same memcpy in 9.8 ns.

One honest caveat: these numbers are within noise of the same benchmarks run against a disk-backed file, because a benchmark keeps every page resident in the page cache and never actually touches the disk. Shared memory is not the choice here for throughput. It is the choice for not periodically writing back a cache nobody wants durable, and for not hitting the tail-latency cliff where the kernel evicts a page under memory pressure and some later cache *hit* has to block on disk to fault it back in. Neither of those shows up in a microbenchmark.

## Testing
```shell
cargo test
```

`tests/shm_ipc.rs` spawns real OS processes that hammer the same key concurrently. Threads would share an address space and prove nothing here, so these are genuinely separate processes sharing only the mapping. Every value is self-checking, which means a torn read fails its checksum instead of quietly passing as plausible bytes.

## License
GPL-3.0-only. See [LICENSE](LICENSE).

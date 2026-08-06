//! Raw throughput of the shared-memory cache, excluding any IPC coordination.
//!
//! These run single-threaded against an uncontended mapping, so they measure the cost
//! of the hash, the seqlock, and the copy — not the cost of contention.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use minicache::ShmCache;

const SLOTS: usize = 4096;

/// Unlinks the shared-memory object when the bench ends, since it would otherwise
/// survive until reboot.
struct TempShm(String, ShmCache);

impl TempShm {
    fn new(tag: &str) -> Self {
        let name = format!("mc-b{tag}-{:x}", std::process::id());
        let _ = ShmCache::unlink(&name);
        let cache = ShmCache::open(&name, SLOTS).unwrap();
        TempShm(name, cache)
    }
}

impl Drop for TempShm {
    fn drop(&mut self) {
        let _ = ShmCache::unlink(&self.0);
    }
}

fn bench_write(c: &mut Criterion) {
    let shm = TempShm::new("write");
    let val = vec![b'v'; 256];

    c.bench_function("shm_write_overwrite", |b| {
        b.iter(|| shm.1.write(black_box(b"key"), black_box(&val)).unwrap())
    });
}

fn bench_read_hit(c: &mut Criterion) {
    let shm = TempShm::new("read-hit");
    let val = vec![b'v'; 256];
    shm.1.write(b"key", &val).unwrap();

    c.bench_function("shm_read_hit", |b| {
        b.iter(|| black_box(shm.1.read(black_box(b"key"))))
    });
}

/// Read hits across value sizes.
///
/// If the cost tracked value size, this would scale with the memcpy. If it is roughly
/// flat, the allocation inside `Bytes::copy_from_slice` dominates instead.
fn bench_read_by_value_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("shm_read_hit_by_size");
    for size in [8usize, 256, 1024] {
        let shm = TempShm::new(&format!("size-{size}"));
        shm.1.write(b"key", &vec![b'v'; size]).unwrap();
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| black_box(shm.1.read(black_box(b"key"))))
        });
    }
    group.finish();
}

fn bench_read_miss(c: &mut Criterion) {
    let shm = TempShm::new("read-miss");

    c.bench_function("shm_read_miss", |b| {
        b.iter(|| black_box(shm.1.read(black_box(b"absent"))))
    });
}

/// Reads across many distinct keys, so the working set no longer fits in cache and the
/// benchmark reflects real memory traffic rather than one hot slot.
fn bench_read_spread(c: &mut Criterion) {
    let shm = TempShm::new("read-spread");
    let val = vec![b'v'; 256];
    let keys: Vec<[u8; 8]> = (0..SLOTS as u64).map(|i| i.to_le_bytes()).collect();
    for k in &keys {
        shm.1.write(k, &val).unwrap();
    }

    c.bench_function("shm_read_spread", |b| {
        let mut i = 0usize;
        b.iter(|| {
            i = (i + 1) % keys.len();
            black_box(shm.1.read(black_box(&keys[i])))
        })
    });
}

criterion_group!(
    benches,
    bench_write,
    bench_read_hit,
    bench_read_by_value_size,
    bench_read_miss,
    bench_read_spread
);
criterion_main!(benches);

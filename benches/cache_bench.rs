use bytes::Bytes;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use minicache::Cache;

fn bench_cache_put(c: &mut Criterion) {
    let mut cache = Cache::new(1000);
    let key = Bytes::from_static(b"key");
    let val = Bytes::from_static(b"value");

    c.bench_function("cache_put_overwrite", |b| {
        b.iter(|| cache.put(black_box(key.clone()), black_box(val.clone())))
    });
}

fn bench_cache_fill_evict(c: &mut Criterion) {
    let mut cache = Cache::new(100);
    let val = Bytes::from_static(b"value");

    c.bench_function("cache_fill_evict", |b| {
        let mut i = 0u64;
        b.iter(|| {
            // Create a unique key (cheaply as possible, though formatting strings has cost)
            // To avoid string formatting cost dominating, we can cycle through a pre-generated set.
            // But for simplicity, let's just do a simple put.
            // Better: use a small rotating set of keys that exceeds capacity slightly?
            // If we want to test eviction, we need distinct keys.

            // Let's rely on just overwriting for the basic bench,
            // but we can try a cyclic insertion of 200 items into 100 capacity.
            let key_data = (i % 200).to_le_bytes();
            let key = Bytes::copy_from_slice(&key_data);
            i += 1;

            cache.put(black_box(key), black_box(val.clone()))
        })
    });
}

criterion_group!(benches, bench_cache_put, bench_cache_fill_evict);
criterion_main!(benches);

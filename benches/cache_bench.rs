use criterion::{black_box, criterion_group, criterion_main, Criterion};
use minicache::Cache;

fn bench_cache_push(c: &mut Criterion) {
    let mut cache = Cache::new(1000);
    c.bench_function("cache_push", |b| {
        b.iter(|| {
            cache.push(black_box(vec![1, 2, 3]), black_box(vec![4, 5, 6]))
        })
    });
}

criterion_group!(benches, bench_cache_push);
criterion_main!(benches);
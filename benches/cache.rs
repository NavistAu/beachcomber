use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use shellstate::cache::Cache;
use shellstate::provider::{ProviderResult, Value};
use std::sync::Arc;
use std::thread;

fn bench_cache_read_global(c: &mut Criterion) {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("testhost".to_string()));
    result.insert("short", Value::String("test".to_string()));
    cache.put("hostname", None, result);

    c.bench_function("cache_read_global", |b| {
        b.iter(|| {
            let entry = cache.get("hostname", None);
            criterion::black_box(entry);
        })
    });
}

fn bench_cache_read_path_scoped(c: &mut Criterion) {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache.put("git", Some("/home/user/project"), result);

    c.bench_function("cache_read_path_scoped", |b| {
        b.iter(|| {
            let entry = cache.get("git", Some("/home/user/project"));
            criterion::black_box(entry);
        })
    });
}

fn bench_cache_write(c: &mut Criterion) {
    let cache = Cache::new();

    c.bench_function("cache_write", |b| {
        b.iter(|| {
            let mut result = ProviderResult::new();
            result.insert("name", Value::String("testhost".to_string()));
            cache.put("hostname", None, result);
        })
    });
}

fn bench_cache_read_contention(c: &mut Criterion) {
    let cache = Arc::new(Cache::new());
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("testhost".to_string()));
    cache.put("hostname", None, result);

    let mut group = c.benchmark_group("cache_read_contention");
    for num_threads in [1, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|_| {
                            let cache = Arc::clone(&cache);
                            thread::spawn(move || {
                                for _ in 0..100 {
                                    let entry = cache.get("hostname", None);
                                    criterion::black_box(entry);
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().unwrap();
                    }
                })
            },
        );
    }
    group.finish();
}

fn bench_cache_field_extraction(c: &mut Criterion) {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    result.insert("ahead", Value::Int(2));
    result.insert("behind", Value::Int(0));
    result.insert("staged", Value::Int(0));
    result.insert("unstaged", Value::Int(3));
    result.insert("untracked", Value::Int(1));
    result.insert("conflicted", Value::Int(0));
    result.insert("stash", Value::Int(0));
    result.insert("state", Value::String("clean".to_string()));
    cache.put("git", Some("/project"), result);

    let mut group = c.benchmark_group("cache_field_extraction");

    group.bench_function("full_provider", |b| {
        b.iter(|| {
            let entry = cache.get("git", Some("/project")).unwrap();
            criterion::black_box(entry.result.to_json());
        })
    });

    group.bench_function("single_field", |b| {
        b.iter(|| {
            let entry = cache.get("git", Some("/project")).unwrap();
            let val = entry.result.get("branch").unwrap();
            criterion::black_box(val.as_text());
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_cache_read_global,
    bench_cache_read_path_scoped,
    bench_cache_write,
    bench_cache_read_contention,
    bench_cache_field_extraction,
);
criterion_main!(benches);

use beachcomber::cache::Cache;
use beachcomber::provider::Value;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

fn host_fields() -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("name".to_string(), Value::String("testhost".to_string()));
    m.insert("short".to_string(), Value::String("test".to_string()));
    m
}

fn git_refs_fields() -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("branch".to_string(), Value::String("main".to_string()));
    m.insert("commit".to_string(), Value::String("abc123".to_string()));
    m.insert("ahead".to_string(), Value::Int(2));
    m.insert("behind".to_string(), Value::Int(0));
    m.insert("stash".to_string(), Value::Int(0));
    m
}

fn git_status_fields() -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("staged".to_string(), Value::Int(0));
    m.insert("unstaged".to_string(), Value::Int(3));
    m.insert("untracked".to_string(), Value::Int(1));
    m.insert("conflicted".to_string(), Value::Int(0));
    m.insert("dirty".to_string(), Value::Bool(true));
    m
}

fn bench_cache_read_global(c: &mut Criterion) {
    let cache = Cache::new();
    cache.put_source("hostname", None, "host", host_fields(), None);

    c.bench_function("cache_read_global", |b| {
        b.iter(|| {
            let entry = cache.get_entry("hostname", None);
            criterion::black_box(entry);
        })
    });
}

fn bench_cache_read_path_scoped(c: &mut Criterion) {
    let cache = Cache::new();
    cache.put_source(
        "git",
        Some("/home/user/project"),
        "refs",
        git_refs_fields(),
        Some(120),
    );

    c.bench_function("cache_read_path_scoped", |b| {
        b.iter(|| {
            let entry = cache.get_entry("git", Some("/home/user/project"));
            criterion::black_box(entry);
        })
    });
}

fn bench_cache_write(c: &mut Criterion) {
    let cache = Cache::new();

    c.bench_function("cache_write", |b| {
        b.iter(|| {
            cache.put_source("hostname", None, "host", host_fields(), None);
        })
    });
}

fn bench_cache_read_contention(c: &mut Criterion) {
    let cache = Arc::new(Cache::new());
    cache.put_source("hostname", None, "host", host_fields(), None);

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
                                    let entry = cache.get_entry("hostname", None);
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
    // Two sources at the same (git, /project) key, mirroring how the
    // post-source-refactor scheduler populates the cache.
    cache.put_source(
        "git",
        Some("/project"),
        "refs",
        git_refs_fields(),
        Some(120),
    );
    cache.put_source(
        "git",
        Some("/project"),
        "status",
        git_status_fields(),
        Some(60),
    );

    let mut group = c.benchmark_group("cache_field_extraction");

    group.bench_function("flattened_entry", |b| {
        b.iter(|| {
            let entry = cache.get_entry("git", Some("/project")).unwrap();
            criterion::black_box(entry.flatten_fields());
        })
    });

    group.bench_function("single_field_via_get_field", |b| {
        b.iter(|| {
            let val = cache.get_field("git", Some("/project"), "branch").unwrap();
            criterion::black_box(val);
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

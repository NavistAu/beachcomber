use beachcomber::provider::registry::ProviderRegistry;
use criterion::{Criterion, criterion_group, criterion_main};

/// Every run of `provider`'s shipped Sources once at `path`, one after another:
/// the fields each answered.
fn execute_all_sources(provider: &str, path: &str) -> impl FnMut() -> Vec<usize> {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    let sources: Vec<_> = registry
        .provider_sources(provider)
        .unwrap_or_else(|| panic!("{provider} is shipped"))
        .iter()
        .map(|meta| registry.source(provider, &meta.name).expect("registered"))
        .collect();
    let path = path.to_string();
    move || {
        sources
            .iter()
            .map(|s| s.execute(Some(&path)).unwrap().fields.len())
            .collect()
    }
}

/// One run of the shipped Source `provider.source` at the instance at `~`,
/// as the scheduler runs it: its lines over the daemon's environment.
fn bench_shipped_source(c: &mut Criterion, provider: &str, source: &str) {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    let s = registry
        .source(provider, source)
        .unwrap_or_else(|| panic!("{provider}.{source} is shipped"));
    c.bench_function(&format!("provider_{provider}_execute"), |b| {
        b.iter(|| {
            criterion::black_box(s.execute(None).unwrap().fields.len());
        })
    });
}

fn bench_hostname_execute(c: &mut Criterion) {
    bench_shipped_source(c, "hostname", "host");
}

fn bench_user_execute(c: &mut Criterion) {
    bench_shipped_source(c, "user", "name");
}

fn bench_load_execute(c: &mut Criterion) {
    bench_shipped_source(c, "load", "loadavg");
}

fn bench_uptime_execute(c: &mut Criterion) {
    bench_shipped_source(c, "uptime", "time");
}

fn bench_git_execute(c: &mut Criterion) {
    // The beachcomber repo itself: its five Sources, one after another, through
    // the shipped file and the compiled-in plugin.
    let mut run = execute_all_sources("git", env!("CARGO_MANIFEST_DIR"));

    c.bench_function("provider_git_execute", |b| {
        b.iter(|| {
            criterion::black_box(run());
        })
    });
}

fn bench_git_vs_raw(c: &mut Criterion) {
    let repo_path = env!("CARGO_MANIFEST_DIR");
    let mut group = c.benchmark_group("git_comparison");

    group.bench_function("beachcomber_git_provider", |b| {
        let mut run = execute_all_sources("git", repo_path);
        b.iter(|| {
            criterion::black_box(run());
        })
    });

    group.bench_function("raw_git_status", |b| {
        b.iter(|| {
            let output = std::process::Command::new("git")
                .args(["status", "--porcelain=v2", "--branch"])
                .current_dir(repo_path)
                .output()
                .unwrap();
            criterion::black_box(output);
        })
    });

    group.finish();
}

fn bench_network_execute(c: &mut Criterion) {
    bench_shipped_source(c, "network", "interfaces");
}

fn bench_battery_execute(c: &mut Criterion) {
    bench_shipped_source(c, "battery", "state");
}

fn bench_gcloud_execute(c: &mut Criterion) {
    bench_shipped_source(c, "gcloud_configs", "config_dir");
}

criterion_group!(
    benches,
    bench_hostname_execute,
    bench_user_execute,
    bench_load_execute,
    bench_uptime_execute,
    bench_git_execute,
    bench_git_vs_raw,
    bench_network_execute,
    bench_battery_execute,
    bench_gcloud_execute,
);
criterion_main!(benches);

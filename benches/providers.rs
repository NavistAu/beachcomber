use criterion::{criterion_group, criterion_main, Criterion};
use shellstate::provider::git::GitProvider;
use shellstate::provider::hostname::HostnameProvider;
use shellstate::provider::load::LoadProvider;
use shellstate::provider::uptime::UptimeProvider;
use shellstate::provider::user::UserProvider;
use shellstate::provider::Provider;

fn bench_hostname_execute(c: &mut Criterion) {
    let p = HostnameProvider;
    c.bench_function("provider_hostname_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_user_execute(c: &mut Criterion) {
    let p = UserProvider;
    c.bench_function("provider_user_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_load_execute(c: &mut Criterion) {
    let p = LoadProvider;
    c.bench_function("provider_load_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_uptime_execute(c: &mut Criterion) {
    let p = UptimeProvider;
    c.bench_function("provider_uptime_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_git_execute(c: &mut Criterion) {
    // Use the shellstate repo itself as the target.
    let repo_path = env!("CARGO_MANIFEST_DIR");
    let p = GitProvider;

    c.bench_function("provider_git_execute", |b| {
        b.iter(|| {
            let result = p.execute(Some(criterion::black_box(repo_path)));
            criterion::black_box(result);
        })
    });
}

fn bench_git_vs_raw(c: &mut Criterion) {
    let repo_path = env!("CARGO_MANIFEST_DIR");
    let mut group = c.benchmark_group("git_comparison");

    group.bench_function("shellstate_git_provider", |b| {
        let p = GitProvider;
        b.iter(|| {
            let result = p.execute(Some(criterion::black_box(repo_path)));
            criterion::black_box(result);
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

criterion_group!(
    benches,
    bench_hostname_execute,
    bench_user_execute,
    bench_load_execute,
    bench_uptime_execute,
    bench_git_execute,
    bench_git_vs_raw,
);
criterion_main!(benches);

use beachcomber::provider::Provider;
use beachcomber::provider::battery::BatteryProvider;
use beachcomber::provider::gcloud::GcloudProvider;
use beachcomber::provider::git::GitProvider;
use beachcomber::provider::hostname::HostnameProvider;
use beachcomber::provider::kubecontext::KubecontextProvider;
use beachcomber::provider::load::LoadProvider;
use beachcomber::provider::network::NetworkProvider;
use beachcomber::provider::uptime::UptimeProvider;
use beachcomber::provider::user::UserProvider;
use criterion::{Criterion, criterion_group, criterion_main};

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
    // Use the beachcomber repo itself as the target.
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

    group.bench_function("beachcomber_git_provider", |b| {
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

fn bench_network_execute(c: &mut Criterion) {
    let p = NetworkProvider;
    c.bench_function("provider_network_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_battery_execute(c: &mut Criterion) {
    let p = BatteryProvider;
    c.bench_function("provider_battery_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_kubecontext_execute(c: &mut Criterion) {
    let p = KubecontextProvider;
    c.bench_function("provider_kubecontext_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
}

fn bench_gcloud_execute(c: &mut Criterion) {
    let p = GcloudProvider;
    c.bench_function("provider_gcloud_execute", |b| {
        b.iter(|| {
            let result = p.execute(None);
            criterion::black_box(result);
        })
    });
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
    bench_kubecontext_execute,
    bench_gcloud_execute,
);
criterion_main!(benches);

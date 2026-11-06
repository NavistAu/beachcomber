//! Loading a configuration: what every daemon start, config reload and
//! `comb check config` does before anything runs, and what every test that
//! builds a registry pays.
//!
//! `parse` is the shipped `defaults/*.toml` parsed into layers. `shipped` is
//! [`ProviderRegistry::shipped`]'s work: those layers composed into a
//! `Config` and loaded ([`ProviderRegistry::load`]): the built-in Sources, the
//! config Sources' lines compiled and linted, each `return` rendered into a
//! schedule, the providers' lines, and the seed planned and its templates
//! linted. `scanline` is the same with a representative user drop-in on top,
//! scanline's generated `10-scanline.toml`: three Sources, a macro library
//! and a client template tree. Each is from text to registry, as a daemon
//! start or a test builds it.

use beachcomber::config::Config;
use beachcomber::daemon::defaults::shipped_layers;
use beachcomber::provider::registry::ProviderRegistry;
use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::path::PathBuf;

const SCANLINE: &str = include_str!("../libbeachcomber/tests/fixtures/scanline/10-scanline.toml");

/// The shipped layers, and scanline's drop-in after them when `scanline`.
fn layers(scanline: bool) -> Vec<(PathBuf, toml::Value)> {
    let mut layers = shipped_layers();
    if scanline {
        layers.push((
            PathBuf::from("/conf.d/10-scanline.toml"),
            toml::from_str(SCANLINE).expect("the scanline fixture parses"),
        ));
    }
    layers
}

fn bench_load(c: &mut Criterion) {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let mut group = c.benchmark_group("load");
    group.bench_function("parse", |b| b.iter(shipped_layers));
    for (name, scanline) in [("shipped", false), ("scanline", true)] {
        let load = ProviderRegistry::load(&Config::from_layers(layers(scanline)), &vars);
        assert!(load.errors.is_empty(), "{name} loads: {:?}", load.errors);
        group.bench_function(name, |b| {
            b.iter(|| ProviderRegistry::load(&Config::from_layers(layers(scanline)), &vars))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_load);
criterion_main!(benches);

use criterion::{criterion_group, criterion_main, Criterion};
use shellstate::protocol::{split_key, Request, Response};

fn bench_parse_get_request(c: &mut Criterion) {
    let json = r#"{"op": "get", "key": "git.branch", "path": "/home/user/project"}"#;

    c.bench_function("parse_get_request", |b| {
        b.iter(|| {
            let req: Request = serde_json::from_str(criterion::black_box(json)).unwrap();
            criterion::black_box(req);
        })
    });
}

fn bench_parse_subscribe_request(c: &mut Criterion) {
    let json = r#"{"op": "subscribe", "key": "git", "path": "/project", "triggers": {"watch": true, "poll": "10s"}}"#;

    c.bench_function("parse_subscribe_request", |b| {
        b.iter(|| {
            let req: Request = serde_json::from_str(criterion::black_box(json)).unwrap();
            criterion::black_box(req);
        })
    });
}

fn bench_serialize_response_json(c: &mut Criterion) {
    let response = Response::ok(
        serde_json::json!({"branch": "main", "dirty": false, "ahead": 2}),
        150,
        false,
    );

    c.bench_function("serialize_response_json", |b| {
        b.iter(|| {
            let json = serde_json::to_string(criterion::black_box(&response)).unwrap();
            criterion::black_box(json);
        })
    });
}

fn bench_split_key(c: &mut Criterion) {
    c.bench_function("split_key_dotted", |b| {
        b.iter(|| {
            let result = split_key(criterion::black_box("git.branch"));
            criterion::black_box(result);
        })
    });

    c.bench_function("split_key_simple", |b| {
        b.iter(|| {
            let result = split_key(criterion::black_box("hostname"));
            criterion::black_box(result);
        })
    });
}

criterion_group!(
    benches,
    bench_parse_get_request,
    bench_parse_subscribe_request,
    bench_serialize_response_json,
    bench_split_key,
);
criterion_main!(benches);

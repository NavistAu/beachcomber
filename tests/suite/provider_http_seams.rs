use beachcomber::boundaries::http::{HttpFetcher, HttpResponse};
use beachcomber::config::HttpProviderConfig;
use beachcomber::provider::Provider;
use beachcomber::provider::http::HttpProvider;
use std::sync::Arc;
use std::time::Duration;

// ── Hand-written test double ──────────────────────────────────────────────────

struct StubFetcher {
    response: Result<HttpResponse, String>,
}

impl HttpFetcher for StubFetcher {
    fn fetch(
        &self,
        _method: String,
        _url: String,
        _headers: Vec<(String, String)>,
        _body: Option<Vec<u8>>,
        _timeout: Duration,
    ) -> Result<HttpResponse, String> {
        self.response.clone()
    }
}

fn ok_response(status: u16, body: &[u8]) -> Result<HttpResponse, String> {
    Ok(HttpResponse {
        status,
        headers: vec![],
        body: body.to_vec(),
    })
}

fn make_provider(fetcher: impl HttpFetcher + 'static) -> HttpProvider {
    let config = HttpProviderConfig {
        url: "https://example.com/api".to_string(),
        ..Default::default()
    };
    HttpProvider::with_fetcher("stub_http", config, Arc::new(fetcher))
}

fn execute_first_source(provider: &HttpProvider) -> beachcomber::provider::SourceResult {
    let sources = provider.sources();
    assert!(
        !sources.is_empty(),
        "provider must expose at least one source"
    );
    sources[0].execute(None)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A 500 response must produce an empty SourceResult (treated as failure/no data).
#[test]
fn http_provider_returns_failure_on_500() {
    let fetcher = StubFetcher {
        response: ok_response(500, b"Internal Server Error"),
    };
    let provider = make_provider(fetcher);
    let result = execute_first_source(&provider);
    assert!(
        result.fields.is_empty(),
        "500 response should produce no fields, got: {:?}",
        result.fields
    );
}

/// A network/transport error (Err variant) must produce an empty SourceResult.
#[test]
fn http_provider_returns_failure_on_transport_error() {
    let fetcher = StubFetcher {
        response: Err("connection timed out".to_string()),
    };
    let provider = make_provider(fetcher);
    let result = execute_first_source(&provider);
    assert!(
        result.fields.is_empty(),
        "transport error should produce no fields, got: {:?}",
        result.fields
    );
}

/// A 200 with non-JSON body must produce a single `body` field (text fallback).
/// The provider must not panic on malformed JSON.
#[test]
fn http_provider_returns_failure_on_malformed_json() {
    // "Failure" here means: body is not valid JSON but the provider should
    // degrade gracefully. The current implementation stores the raw text in
    // a `body` field rather than panicking, so we assert on that shape.
    let fetcher = StubFetcher {
        response: ok_response(200, b"not valid json {{{{"),
    };
    let provider = make_provider(fetcher);
    let result = execute_first_source(&provider);
    // Must not panic; must produce the `body` text-fallback field.
    assert!(
        result.fields.contains_key("body"),
        "malformed JSON should produce a 'body' text-fallback field, got: {:?}",
        result.fields
    );
    match result.fields.get("body") {
        Some(beachcomber::provider::Value::String(s)) => {
            assert_eq!(s, "not valid json {{{{");
        }
        other => panic!("expected Value::String for 'body', got: {:?}", other),
    }
}

/// A 200 with a plain-text body must produce a `body` field with the text.
#[test]
fn http_provider_returns_value_on_2xx_text() {
    let fetcher = StubFetcher {
        response: ok_response(200, b"ok"),
    };
    let provider = make_provider(fetcher);
    let result = execute_first_source(&provider);
    assert!(
        result.fields.contains_key("body"),
        "plain text 200 should produce 'body' field, got: {:?}",
        result.fields
    );
    match result.fields.get("body") {
        Some(beachcomber::provider::Value::String(s)) => {
            assert_eq!(s, "ok");
        }
        other => panic!("expected Value::String('ok'), got: {:?}", other),
    }
}

/// A 200 with a JSON object body must produce fields matching the JSON keys.
#[test]
fn http_provider_returns_json_fields_on_2xx_json() {
    let body = br#"{"status":"up","version":"1.2.3"}"#;
    let fetcher = StubFetcher {
        response: ok_response(200, body),
    };
    let provider = make_provider(fetcher);
    let result = execute_first_source(&provider);
    assert!(
        result.fields.contains_key("status"),
        "JSON 200 should produce 'status' field, got: {:?}",
        result.fields
    );
    assert!(
        result.fields.contains_key("version"),
        "JSON 200 should produce 'version' field, got: {:?}",
        result.fields
    );
}

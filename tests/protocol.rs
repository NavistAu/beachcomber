use beachcomber::protocol::{Format, Request, Response};

#[test]
fn parse_get_request_with_path() {
    let json = r#"{"op": "get", "key": "git.branch", "path": "/home/user/project"}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Get { key, path, format } => {
            assert_eq!(key, "git.branch");
            assert_eq!(path.as_deref(), Some("/home/user/project"));
            assert_eq!(format, Format::Json, "Default format should be Json");
        }
        _ => panic!("Expected Get request"),
    }
}

#[test]
fn parse_get_request_global() {
    let json = r#"{"op": "get", "key": "battery"}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Get { key, path, .. } => {
            assert_eq!(key, "battery");
            assert!(path.is_none(), "Global request should have no path");
        }
        _ => panic!("Expected Get request"),
    }
}

#[test]
fn parse_get_request_text_format() {
    let json = r#"{"op": "get", "key": "git.branch", "path": "/project", "format": "text"}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Get { format, .. } => {
            assert_eq!(format, Format::Text);
        }
        _ => panic!("Expected Get request"),
    }
}

#[test]
fn parse_poke_request() {
    let json = r#"{"op": "poke", "key": "git", "path": "/project"}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Poke { key, path } => {
            assert_eq!(key, "git");
            assert_eq!(path.as_deref(), Some("/project"));
        }
        _ => panic!("Expected Poke request"),
    }
}

#[test]
fn response_ok_serializes() {
    let response = Response::ok(serde_json::json!({"branch": "main"}), 50, false);
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["age_ms"], 50);
    assert_eq!(json["stale"], false);
}

#[test]
fn response_error_serializes() {
    let response = Response::error("unknown provider");
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"], "unknown provider");
}

#[test]
fn response_miss_serializes() {
    let response = Response::miss();
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["ok"], true);
    assert!(json["data"].is_null(), "Cache miss should have null data");
}

#[test]
fn key_splits_provider_and_field() {
    let (provider, field) = beachcomber::protocol::split_key("git.branch");
    assert_eq!(provider, "git");
    assert_eq!(field, Some("branch"));
}

#[test]
fn key_without_field() {
    let (provider, field) = beachcomber::protocol::split_key("battery");
    assert_eq!(provider, "battery");
    assert_eq!(field, None);
}

#[test]
fn key_with_dotted_field() {
    let (provider, field) = beachcomber::protocol::split_key("mise.tools");
    assert_eq!(provider, "mise");
    assert_eq!(field, Some("tools"));
}

#[test]
fn parse_store_request() {
    let json = r#"{"op":"store","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Store { key, data, ttl, path } => {
            assert_eq!(key, "myapp");
            assert_eq!(data["status"], "healthy");
            assert_eq!(data["version"], "1.2.3");
            assert!(ttl.is_none());
            assert!(path.is_none());
        }
        _ => panic!("Expected Store request"),
    }
}

#[test]
fn parse_store_request_with_ttl_and_path() {
    let json = r#"{"op":"store","key":"myapp","data":{"count":42},"ttl":"30s","path":"/home/user/project"}"#;
    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::Store { key, data, ttl, path } => {
            assert_eq!(key, "myapp");
            assert_eq!(data["count"], 42);
            assert_eq!(ttl.as_deref(), Some("30s"));
            assert_eq!(path.as_deref(), Some("/home/user/project"));
        }
        _ => panic!("Expected Store request"),
    }
}

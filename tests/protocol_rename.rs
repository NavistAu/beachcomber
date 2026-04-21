use serde_json::json;
use beachcomber::protocol::Request;

#[test]
fn refresh_request_deserializes_from_refresh_op() {
    let payload = json!({"op": "refresh", "key": "git", "path": null});
    let req: Request = serde_json::from_value(payload).unwrap();
    match req {
        Request::Refresh { key, path } => {
            assert_eq!(key, "git");
            assert_eq!(path, None);
        }
        _ => panic!("expected Refresh variant"),
    }
}

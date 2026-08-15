use similar_asserts::assert_eq;

use super::*;

/// 构造出的请求带上了 x-user-id —— 上游靠它选凭据,漏了那条路由整个不可用。
#[test]
fn request_carries_the_user_id_in_metadata() {
    let account = Account {
        id: 42,
        username: "alice".to_owned(),
    };

    let request = as_user(
        &account,
        proto::GetTracksRequest::default(),
    );

    assert_eq!(
        request
            .metadata()
            .get("x-user-id")
            .map(|value| value.to_str().unwrap()),
        Some("42"),
    );
}

/// 加 metadata 不动消息体。
#[test]
fn request_body_is_untouched() {
    let account = Account {
        id: 1,
        username: "alice".to_owned(),
    };
    let message = proto::GetTracksRequest {
        platform: proto::Platform::Netease as i32,
        track_ids: vec!["1".to_owned(), "2".to_owned()],
    };

    let request = as_user(&account, message.clone());

    assert_eq!(request.into_inner(), message);
}

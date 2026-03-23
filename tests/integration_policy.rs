use ilert::ilert::ILert;
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path, query_param, body_json};
use serde_json::json;

use ilagent::config::ILConfig;
use ilagent::consumers::policy::handle_policy_update;

const SAMPLE_PAYLOAD: &str = r#"{
    "uuid": "550x8400-x29b-11d4-x716-446655440000",
    "location": "powerplant",
    "field": "clover",
    "slot": "three50",
    "eventTime": "2025-07-16T15:42:30.621Z",
    "eventType": "active",
    "data": {
        "email": "support@ilert.com",
        "shift": "1"
    }
}"#;

#[tokio::test]
async fn policy_update_single_routing_key() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/escalation-policies/resolve"))
        .and(query_param("routing-key", "powerplant"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42,
            "name": "Powerplant Policy",
            "escalationRules": [
                {"escalationTimeout": 0},
                {"escalationTimeout": 5}
            ]
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .and(body_json(json!({"email": "support@ilert.com"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 99,
            "email": "support@ilert.com",
            "username": "support"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/escalation-policies/42/levels/1"))
        .and(body_json(json!({"escalationTimeout": 5, "users": [{"id": 99}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("active".to_string());
    config.policy_routing_keys = Some("location".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, SAMPLE_PAYLOAD).await;
    assert!(!should_retry);
}

#[tokio::test]
async fn policy_update_multiple_routing_keys_concatenated() {
    let mock_server = MockServer::start().await;

    // routing keys "location,slot" should be concatenated to "powerplantthree50"
    Mock::given(method("GET"))
        .and(path("/api/escalation-policies/resolve"))
        .and(query_param("routing-key", "powerplantthree50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 10,
            "escalationRules": [
                {"escalationTimeout": 0},
                {"escalationTimeout": 3}
            ]
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 99, "email": "support@ilert.com"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/escalation-policies/10/levels/1"))
        .and(body_json(json!({"escalationTimeout": 3, "users": [{"id": 99}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("active".to_string());
    config.policy_routing_keys = Some("location,slot".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, SAMPLE_PAYLOAD).await;
    assert!(!should_retry);
}

#[tokio::test]
async fn policy_update_shift_defaults_to_zero() {
    let mock_server = MockServer::start().await;

    let payload_no_shift = r#"{
        "location": "plant-a",
        "eventType": "active",
        "data": {"email": "test@ilert.com"}
    }"#;

    Mock::given(method("GET"))
        .and(path("/api/escalation-policies/resolve"))
        .and(query_param("routing-key", "plant-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 5,
            "escalationRules": [{"escalationTimeout": 0}]
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1, "email": "test@ilert.com"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/escalation-policies/5/levels/0"))
        .and(body_json(json!({"escalationTimeout": 0, "users": [{"id": 1}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("active".to_string());
    config.policy_routing_keys = Some("location".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, payload_no_shift).await;
    assert!(!should_retry);
}

#[tokio::test]
async fn policy_update_with_custom_field_paths() {
    let mock_server = MockServer::start().await;

    let payload = r#"{
        "zone": "east",
        "user": {"contact": {"mail": "custom@ilert.com"}},
        "meta": {"tier": "2"}
    }"#;

    Mock::given(method("GET"))
        .and(path("/api/escalation-policies/resolve"))
        .and(query_param("routing-key", "east"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7,
            "escalationRules": [
                {"escalationTimeout": 0},
                {"escalationTimeout": 0},
                {"escalationTimeout": 10}
            ]
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .and(body_json(json!({"email": "custom@ilert.com"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 3})))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/api/escalation-policies/7/levels/2"))
        .and(body_json(json!({"escalationTimeout": 10, "users": [{"id": 3}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.policy_routing_keys = Some("zone".to_string());
    config.map_key_email = Some("user.contact.mail".to_string());
    config.map_key_shift = Some("meta.tier".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, payload).await;
    assert!(!should_retry);
}

#[tokio::test]
async fn policy_update_user_resolve_server_error_retries() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.policy_routing_keys = Some("location".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, SAMPLE_PAYLOAD).await;
    assert!(should_retry, "should retry on server error (5xx)");
}

#[tokio::test]
async fn policy_update_user_resolve_client_error_no_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.policy_routing_keys = Some("location".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, SAMPLE_PAYLOAD).await;
    assert!(!should_retry, "should not retry on client error (4xx)");
}

#[tokio::test]
async fn policy_update_filtered_message_no_api_calls() {
    let mock_server = MockServer::start().await;

    let mut ilert_client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let mut config = ILConfig::new();
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("inactive".to_string());
    config.policy_routing_keys = Some("location".to_string());

    let should_retry = handle_policy_update(&ilert_client, &config, SAMPLE_PAYLOAD).await;
    assert!(!should_retry, "filtered messages should not retry");
}

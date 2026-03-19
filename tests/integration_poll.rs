use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path_regex};
use ilert::ilert::ILert;
use ilagent::models::event_db::EventQueueItem;
use ilagent::poll::send_queued_event;

fn alert_event() -> EventQueueItem {
    let mut event = EventQueueItem::new_with_required("il1apikey123", "ALERT", "Server down", Some("host-1".to_string()));
    event.id = Some("test-event-1".to_string());
    event
}

fn resolve_event() -> EventQueueItem {
    let mut event = EventQueueItem::new_with_required("il1apikey123", "RESOLVE", "Resolved", Some("host-1".to_string()));
    event.id = Some("test-event-2".to_string());
    event
}

fn event_with_path() -> EventQueueItem {
    let mut event = alert_event();
    event.event_api_path = Some("/v1/events/mqtt/il1apikey123".to_string());
    event
}

fn event_with_priority() -> EventQueueItem {
    let mut event = alert_event();
    event.priority = Some("HIGH".to_string());
    event
}

// --- send_queued_event: 202 success ---

#[tokio::test]
async fn send_event_202_returns_no_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202)
            .insert_header("correlation-id", "corr-123"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(!should_retry, "202 should not retry");
}

// --- send_queued_event: 429 rate limited ---

#[tokio::test]
async fn send_event_429_returns_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(should_retry, "429 should retry");
}

// --- send_queued_event: 500 server error ---

#[tokio::test]
async fn send_event_500_returns_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(should_retry, "500 should retry");
}

#[tokio::test]
async fn send_event_502_returns_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(502))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(should_retry, "502 should retry");
}

// --- send_queued_event: 404 bad URL ---

#[tokio::test]
async fn send_event_404_returns_no_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(!should_retry, "404 should not retry");
}

// --- send_queued_event: 400 bad request ---

#[tokio::test]
async fn send_event_400_returns_no_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &alert_event()).await;
    assert!(!should_retry, "400 should not retry");
}

// --- send_queued_event: bad event type drops without retry ---

#[tokio::test]
async fn send_event_bad_event_type_drops() {
    let mock_server = MockServer::start().await;

    // should never be called
    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(0)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let mut event = alert_event();
    event.event_type = "INVALID".to_string();
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry, "bad event type should drop without retry");
}

// --- send_queued_event: bad priority drops without retry ---

#[tokio::test]
async fn send_event_bad_priority_drops() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(0)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let mut event = alert_event();
    event.priority = Some("MEDIUM".to_string());
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry, "bad priority should drop without retry");
}

// --- send_queued_event: RESOLVE event type works ---

#[tokio::test]
async fn send_resolve_event_succeeds() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &resolve_event()).await;
    assert!(!should_retry);
}

// --- send_queued_event: custom event_api_path ---

#[tokio::test]
async fn send_event_uses_custom_api_path() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &event_with_path()).await;
    assert!(!should_retry);
}

// --- send_queued_event: event with priority ---

#[tokio::test]
async fn send_event_with_priority_succeeds() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let should_retry = send_queued_event(&ilert_client, &event_with_priority()).await;
    assert!(!should_retry);
}

// --- send_queued_event: event with no id ---

#[tokio::test]
async fn send_event_without_id_still_works() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client = ILert::new_with_opts(
        Some(mock_server.uri().as_str()), None, Some(5)
    ).unwrap();

    let event = EventQueueItem::new_with_required("k1", "ALERT", "test", None);
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry);
}

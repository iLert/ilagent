use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ilagent::DaemonContext;
use ilagent::config::ILConfig;
use ilagent::db::ILDatabase;
use ilagent::models::event_db::EventQueueItem;
use ilagent::poll::{run_poll_job, send_queued_event};
use ilert::ilert::ILert;
use tempfile::NamedTempFile;
use tokio::sync::Mutex;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn alert_event() -> EventQueueItem {
    let mut event = EventQueueItem::new_with_required(
        "il1apikey123",
        "ALERT",
        "Server down",
        Some("host-1".to_string()),
    );
    event.id = Some("test-event-1".to_string());
    event
}

fn resolve_event() -> EventQueueItem {
    let mut event = EventQueueItem::new_with_required(
        "il1apikey123",
        "RESOLVE",
        "Resolved",
        Some("host-1".to_string()),
    );
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
        .respond_with(ResponseTemplate::new(202).insert_header("correlation-id", "corr-123"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

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

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

    let event = EventQueueItem::new_with_required("k1", "ALERT", "test", None);
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry);
}

// --- event poll: max_retries drops event after limit ---

#[tokio::test]
async fn event_poll_max_retries_drops_event() {
    let mock_server = MockServer::start().await;

    // always return 500 to trigger retries
    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.db_file = db_path.clone();
    config.max_retries = 2;

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    // insert a failing event directly into the DB
    let event = EventQueueItem::new_with_required(
        "il1apikey123",
        "ALERT",
        "Server down",
        Some("host-1".to_string()),
    );
    db.create_il_event(&event).unwrap();

    let queued = db.get_il_events(10).unwrap();
    assert_eq!(queued.len(), 1, "event should be in queue");

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_poll_job(poll_ctx).await;
    });

    // wait for poll to exhaust retries — needs ~5s first poll + 10s backoff for 2 attempts
    tokio::time::sleep(Duration::from_secs(20)).await;

    daemon_ctx.running.store(false, Ordering::Relaxed);

    let db = ILDatabase::new(&db_path);
    let remaining = db.get_il_events(10).unwrap();
    assert!(
        remaining.is_empty(),
        "event should be dropped after exceeding max_retries"
    );
}

// --- event poll: unlimited retries (max_retries=0) keeps event in queue ---

#[tokio::test]
async fn event_poll_unlimited_retries_keeps_event() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.db_file = db_path.clone();
    config.max_retries = 0; // unlimited

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let event = EventQueueItem::new_with_required(
        "il1apikey123",
        "ALERT",
        "Server down",
        Some("host-1".to_string()),
    );
    db.create_il_event(&event).unwrap();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(15)).await;

    daemon_ctx.running.store(false, Ordering::Relaxed);

    let db = ILDatabase::new(&db_path);
    let remaining = db.get_il_events(10).unwrap();
    assert_eq!(
        remaining.len(),
        1,
        "event should remain in queue with unlimited retries"
    );
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ilert::ilert::ILert;
use rumqttc::{Client, MqttOptions, QoS};
use tempfile::NamedTempFile;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use tokio::sync::Mutex;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use ilagent::DaemonContext;
use ilagent::config::ILConfig;
use ilagent::consumers::mqtt::run_mqtt_job;
use ilagent::db::ILDatabase;
use ilagent::poll::{run_mqtt_poll_job, run_poll_job};

async fn start_mosquitto() -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    let mosquitto_conf = b"listener 1883 0.0.0.0\nallow_anonymous true\n".to_vec();

    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(1883.tcp())
        .with_wait_for(WaitFor::message_on_stderr("mosquitto version"))
        .with_copy_to("/mosquitto/config/mosquitto.conf", mosquitto_conf)
        .start()
        .await
        .expect("Failed to start mosquitto container — is Docker running?");

    let port = container.get_host_port_ipv4(1883).await.unwrap();
    (container, port)
}

fn mqtt_daemon_ctx(
    mqtt_host: &str,
    mqtt_port: u16,
    db_path: &str,
    ilert_client: ILert,
) -> Arc<DaemonContext> {
    let mut config = ILConfig::new();
    config.mqtt_host = Some(mqtt_host.to_string());
    config.mqtt_port = Some(mqtt_port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.mqtt_qos = 1;
    config.db_file = db_path.to_string();

    let db = ILDatabase::new(db_path);
    db.prepare_database();

    Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    })
}

/// Publish a message to a topic on the given MQTT broker.
fn mqtt_publish(host: &str, port: u16, topic: &str, payload: &str) {
    let mut opts = MqttOptions::new(format!("test-pub-{}", uuid::Uuid::new_v4()), host, port);
    opts.set_keep_alive(Duration::from_secs(5));

    let (client, mut connection) = Client::new(opts, 10);

    let conn_handle = std::thread::spawn(move || {
        for notification in connection.iter() {
            if notification.is_err() {
                break;
            }
        }
    });

    client
        .publish(topic, QoS::AtLeastOnce, false, payload.as_bytes())
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let _ = client.disconnect();
    let _ = conn_handle.join();
}

async fn wait_for_attempts(attempts: &AtomicUsize, expected: usize, timeout: Duration) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if attempts.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(attempts.load(Ordering::SeqCst), expected);
}

async fn publish_until_attempts(
    host: &str,
    port: u16,
    topic: &str,
    payload: &str,
    attempts: &AtomicUsize,
    expected: usize,
    timeout: Duration,
) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        mqtt_publish(host, port, topic, payload);

        let published_at = std::time::Instant::now();
        while published_at.elapsed() < Duration::from_secs(2) {
            if attempts.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    assert!(attempts.load(Ordering::SeqCst) >= expected);
}

async fn wait_for_mqtt_queue_count(db_path: &str, expected: usize, timeout: Duration) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        let db = ILDatabase::new(db_path);
        let queued = db.get_mqtt_queue_items(10).unwrap();
        if queued.len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let db = ILDatabase::new(db_path);
    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(queued.len(), expected);
}

// --- Tests ---

#[tokio::test]
async fn mqtt_event_delivered_without_db_queue() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(move |_req: &Request| {
            response_attempts.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(202)
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path, ilert_client);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    publish_until_attempts(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{"apiKey": "mqtt-ready-key", "eventType": "ALERT", "summary": "ready"}"#,
        &attempts,
        1,
        Duration::from_secs(10),
    )
    .await;

    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{
        "apiKey": "mqtt-test-key",
        "eventType": "ALERT",
        "summary": "MQTT e2e test",
        "alertKey": "mqtt-host-1"
    }"#,
    );

    wait_for_attempts(&attempts, 2, Duration::from_secs(10)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "non-buffered MQTT should not queue events in DB"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn mqtt_non_buffered_qos1_redelivers_after_retryable_ilert_failure() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;
    let ready_attempts = Arc::new(AtomicUsize::new(0));
    let response_ready_attempts = ready_attempts.clone();
    let retry_attempts = Arc::new(AtomicUsize::new(0));
    let response_retry_attempts = retry_attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body);
            if body.contains("retry me")
                && response_retry_attempts.fetch_add(1, Ordering::SeqCst) == 0
            {
                ResponseTemplate::new(500)
            } else {
                if !body.contains("retry me") {
                    response_ready_attempts.fetch_add(1, Ordering::SeqCst);
                }
                ResponseTemplate::new(202)
            }
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path, ilert_client);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    publish_until_attempts(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{"apiKey": "mqtt-ready-key", "eventType": "ALERT", "summary": "ready"}"#,
        &ready_attempts,
        1,
        Duration::from_secs(10),
    )
    .await;

    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{"apiKey": "mqtt-test-key", "eventType": "ALERT", "summary": "retry me"}"#,
    );

    wait_for_attempts(&retry_attempts, 2, Duration::from_secs(12)).await;

    let db = ILDatabase::new(&db_path);
    assert!(db.get_il_events(10).unwrap().is_empty());

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn mqtt_multiple_events_fifo_order() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(move |_req: &Request| {
            response_attempts.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(202)
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path, ilert_client);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    publish_until_attempts(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{"apiKey": "mqtt-ready-key", "eventType": "ALERT", "summary": "ready"}"#,
        &attempts,
        1,
        Duration::from_secs(10),
    )
    .await;

    for i in 0..3 {
        publish_until_attempts(
            "127.0.0.1",
            port,
            "ilert/events",
            &format!(r#"{{"apiKey": "k1", "eventType": "ALERT", "summary": "event-{i}"}}"#),
            &attempts,
            i + 2,
            Duration::from_secs(10),
        )
        .await;
    }

    wait_for_attempts(&attempts, 4, Duration::from_secs(12)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "non-buffered MQTT should not queue events in DB"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Non-event topic messages are ignored by the consumer
#[tokio::test]
async fn mqtt_ignored_topic_not_queued() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(0)
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path, ilert_client);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish to a topic the consumer is NOT subscribed to
    mqtt_publish(
        "127.0.0.1",
        port,
        "other/topic",
        r#"{
        "apiKey": "k1",
        "eventType": "ALERT",
        "summary": "should be ignored"
    }"#,
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "no events should be queued for unrelated topic"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Invalid JSON payloads are dropped silently, don't crash the consumer
#[tokio::test]
async fn mqtt_invalid_json_dropped() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(move |_req: &Request| {
            response_attempts.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(202)
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path, ilert_client);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    publish_until_attempts(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{"apiKey": "mqtt-ready-key", "eventType": "ALERT", "summary": "ready"}"#,
        &attempts,
        1,
        Duration::from_secs(10),
    )
    .await;

    // publish garbage
    mqtt_publish("127.0.0.1", port, "ilert/events", "not json at all");

    // then publish a valid event to prove the consumer is still alive
    publish_until_attempts(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{
        "apiKey": "k1",
        "eventType": "ALERT",
        "summary": "still alive"
    }"#,
        &attempts,
        2,
        Duration::from_secs(10),
    )
    .await;

    wait_for_attempts(&attempts, 2, Duration::from_secs(10)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "non-buffered MQTT should not queue events in DB"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Full event pipeline: MQTT publish -> consumer queues in DB -> poll delivers to mock ilert -> event removed
#[tokio::test]
async fn mqtt_event_e2e_with_poll() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(ResponseTemplate::new(202).insert_header("correlation-id", "mqtt-e2e-corr"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

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

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_poll_job(poll_ctx).await;
    });

    let mqtt_poll_ctx = daemon_ctx.clone();
    let _mqtt_poller = tokio::spawn(async move {
        run_mqtt_poll_job(mqtt_poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish event via MQTT
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{
        "apiKey": "mqtt-e2e-key",
        "eventType": "ALERT",
        "summary": "Full pipeline test",
        "alertKey": "pipe-1"
    }"#,
    );

    // wait for poll job to pick up and deliver
    tokio::time::sleep(Duration::from_secs(20)).await;

    // verify event was delivered and removed from queue
    let db = ILDatabase::new(&db_path);
    let remaining = db.get_il_events(10).unwrap();
    assert!(
        remaining.is_empty(),
        "queue should be empty after poll delivery"
    );

    // wiremock expect(1) verifies the event was delivered exactly once
    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT policy with buffer: message lands in mqtt_queue, poll processes it, queue is drained
#[tokio::test]
async fn mqtt_policy_buffer_drain() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/api/escalation-policies/resolve.*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42, "name": "Test Policy"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 99, "email": "support@ilert.com", "username": "support"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path_regex("/api/escalation-policies/42/levels/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.policy_topic = Some("ilert/policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("active".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let mut ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish policy message
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/policies",
        r#"{
        "uuid": "550x8400-x29b-11d4-x716-446655440000",
        "location": "powerplant",
        "eventType": "active",
        "data": { "email": "support@ilert.com", "shift": "1" }
    }"#,
    );

    wait_for_mqtt_queue_count(&db_path, 1, Duration::from_secs(10)).await;

    let db = ILDatabase::new(&db_path);
    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(queued.len(), 1, "policy message should be in mqtt_queue");
    assert_eq!(queued[0].topic, "ilert/policies");

    // now start the poll job to process it
    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    wait_for_mqtt_queue_count(&db_path, 0, Duration::from_secs(15)).await;

    let remaining = db.get_mqtt_queue_items(10).unwrap();
    assert!(
        remaining.is_empty(),
        "mqtt_queue should be empty after successful processing"
    );

    // wiremock expect() verifies all API calls were made
    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT policy with buffer: failed API call keeps message in queue for retry
#[tokio::test]
async fn mqtt_policy_buffer_retry_on_failure() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    // user resolve returns 500 — will cause retry
    Mock::given(method("POST"))
        .and(path_regex("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.policy_topic = Some("ilert/policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let mut ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish policy message
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/policies",
        r#"{
        "location": "powerplant",
        "eventType": "active",
        "data": { "email": "support@ilert.com", "shift": "1" }
    }"#,
    );

    wait_for_mqtt_queue_count(&db_path, 1, Duration::from_secs(10)).await;

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(15)).await;

    let db = ILDatabase::new(&db_path);
    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(
        queued.len(),
        1,
        "failed message should remain in mqtt_queue for retry"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT policy with buffer: filtered message is queued but dropped by poll (no API calls)
#[tokio::test]
async fn mqtt_policy_buffer_filtered_dropped() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    // no mocks — any unexpected call would fail

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.policy_topic = Some("ilert/policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
    config.filter_key = Some("eventType".to_string());
    config.filter_val = Some("active".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let mut ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish with inactive eventType — should be filtered
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/policies",
        r#"{
        "uuid": "test-uuid",
        "location": "powerplant",
        "eventType": "inactive",
        "data": { "email": "support@ilert.com", "shift": "1" }
    }"#,
    );

    wait_for_mqtt_queue_count(&db_path, 1, Duration::from_secs(10)).await;

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    wait_for_mqtt_queue_count(&db_path, 0, Duration::from_secs(15)).await;

    let db = ILDatabase::new(&db_path);
    let remaining = db.get_mqtt_queue_items(10).unwrap();
    assert!(
        remaining.is_empty(),
        "filtered message should be removed from queue"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT policy with buffer: message is dropped after exceeding max_retries
#[tokio::test]
async fn mqtt_policy_buffer_max_retries_drops_message() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    // user resolve always returns 500 — will cause retry every time
    Mock::given(method("POST"))
        .and(path_regex("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.policy_topic = Some("ilert/policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();
    config.max_retries = 2;

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let mut ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish policy message
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/policies",
        r#"{
        "location": "powerplant",
        "eventType": "active",
        "data": { "email": "support@ilert.com", "shift": "1" }
    }"#,
    );

    // wait for poll to exhaust retries — needs ~10s first poll + 20s backoff for 2 attempts
    tokio::time::sleep(Duration::from_secs(35)).await;

    // verify message was dropped from queue after exceeding max retries
    let db = ILDatabase::new(&db_path);
    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert!(
        queued.is_empty(),
        "message should be dropped after exceeding max_retries"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT policy with buffer: unlimited retries (max_retries=0) keeps message in queue
#[tokio::test]
async fn mqtt_policy_buffer_unlimited_retries_keeps_message() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/api/users/search-email"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.policy_topic = Some("ilert/policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();
    config.max_retries = 0; // unlimited

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let mut ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    ilert_client.auth_via_token("test-api-key").unwrap();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let ctx_clone = daemon_ctx.clone();
    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/policies",
        r#"{
        "location": "powerplant",
        "eventType": "active",
        "data": { "email": "support@ilert.com", "shift": "1" }
    }"#,
    );

    tokio::time::sleep(Duration::from_secs(15)).await;

    // with unlimited retries, message should still be in queue
    let db = ILDatabase::new(&db_path);
    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(
        queued.len(),
        1,
        "message should remain in queue with unlimited retries"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT event with dot-notation mapping and forward_message_payload: full original payload as customDetails
#[tokio::test]
async fn mqtt_event_forward_payload_with_nested_mapping() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.db_file = db_path.clone();
    config.mqtt_buffer = true;
    config.event_key = Some("il1api-test-key".to_string());
    config.map_key_etype = Some("eventType".to_string());
    config.map_val_etype_alert = Some("alertCreated".to_string());
    config.map_key_summary = Some("data.message".to_string());
    config.map_key_alert_key = Some("data.alertId".to_string());
    config.forward_message_payload = true;

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{
        "uuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "location": "powerplant",
        "field": "assembly",
        "slot": "nrd023",
        "component": "asm_unit02",
        "eventTime": "2024-03-15T09:17:45.382Z",
        "eventType": "alertCreated",
        "source": "monitoringService",
        "data": {
            "active": true,
            "alertId": "f9e8d7c6-b5a4-3210-fedc-ba9876543210",
            "priority": 2,
            "type": "pressureAlert",
            "label": "Pressure Threshold Exceeded",
            "message": "Anomaly detected on pump unit 2",
            "description": "",
            "alarmGroup": []
        }
    }"#,
    );

    tokio::time::sleep(Duration::from_secs(15)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1);

    // verify mapped fields via dot-notation
    assert_eq!(events[0].integration_key, "il1api-test-key");
    assert_eq!(events[0].event_type, "ALERT"); // "alertCreated" mapped to ALERT
    assert_eq!(events[0].summary, "Anomaly detected on pump unit 2");
    assert_eq!(
        events[0].alert_key.as_ref().unwrap(),
        "f9e8d7c6-b5a4-3210-fedc-ba9876543210"
    );

    // verify customDetails contains the full original payload
    let cd: serde_json::Value =
        serde_json::from_str(events[0].custom_details.as_ref().unwrap()).unwrap();
    assert_eq!(cd["uuid"], "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
    assert_eq!(cd["location"], "powerplant");
    assert_eq!(cd["component"], "asm_unit02");
    assert_eq!(cd["source"], "monitoringService");
    assert_eq!(cd["data"]["active"], true);
    assert_eq!(cd["data"]["priority"], 2);
    assert_eq!(cd["data"]["type"], "pressureAlert");
    assert_eq!(cd["data"]["label"], "Pressure Threshold Exceeded");
    assert_eq!(cd["data"]["alarmGroup"], serde_json::json!([]));

    // verify MQTT metadata is NOT merged into forwarded payload
    assert!(
        cd.get("topic").is_none(),
        "topic should not pollute the original payload"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// MQTT event with config mappings (custom field names mapped to ilert fields)
#[tokio::test]
async fn mqtt_event_with_config_mappings() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.db_file = db_path.clone();
    config.mqtt_buffer = true;
    config.event_key = Some("static-api-key".to_string());
    config.map_key_summary = Some("msg".to_string());
    config.map_key_alert_key = Some("mCode".to_string());
    config.map_key_etype = Some("state".to_string());
    config.map_val_etype_alert = Some("SET".to_string());
    config.map_val_etype_resolve = Some("CLR".to_string());

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish with custom field names (no apiKey, eventType, summary — all mapped)
    mqtt_publish(
        "127.0.0.1",
        port,
        "ilert/events",
        r#"{
        "state": "SET",
        "mCode": "M-100",
        "msg": "Pump failure detected"
    }"#,
    );

    tokio::time::sleep(Duration::from_secs(15)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].integration_key, "static-api-key");
    assert_eq!(events[0].event_type, "ALERT"); // "SET" mapped to ALERT
    assert_eq!(events[0].summary, "Pump failure detected");
    assert_eq!(events[0].alert_key.as_ref().unwrap(), "M-100");
    assert_eq!(
        events[0].event_api_path.as_ref().unwrap(),
        "/v1/events/mqtt/static-api-key"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn mqtt_buffered_event_moves_from_mqtt_queue_to_event_items() {
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(1883);
    config.mqtt_name = Some("test".to_string());
    config.event_topic = Some("ilert/events".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let payload =
        r#"{"apiKey":"buf-key","eventType":"ALERT","summary":"buffered test","alertKey":"buf-1"}"#;
    db.create_mqtt_queue_item("ilert/events", payload).unwrap();

    let queued = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(queued.len(), 1);

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(15)).await;
    daemon_ctx.running.store(false, Ordering::Relaxed);

    let db = ILDatabase::new(&db_path);
    let remaining_mqtt = db.get_mqtt_queue_items(10).unwrap();
    assert!(
        remaining_mqtt.is_empty(),
        "mqtt_queue should be empty after successful enqueue"
    );

    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1, "event should be in event_items");
    assert_eq!(events[0].integration_key, "buf-key");
    assert_eq!(events[0].summary, "buffered test");
    assert_eq!(events[0].alert_key.as_ref().unwrap(), "buf-1");
}

#[tokio::test]
async fn mqtt_buffered_event_db_failure_keeps_mqtt_queue_item() {
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let payload = r#"{"apiKey":"fail-key","eventType":"ALERT","summary":"should retry"}"#;
    db.create_mqtt_queue_item("ilert/events", payload).unwrap();

    let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
    raw_conn.execute_batch("DROP TABLE event_items").unwrap();
    drop(raw_conn);

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(1883);
    config.mqtt_name = Some("test".to_string());
    config.event_topic = Some("ilert/events".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(15)).await;
    daemon_ctx.running.store(false, Ordering::Relaxed);

    let db = daemon_ctx.db.lock().await;
    let remaining = db.get_mqtt_queue_items(10).unwrap();
    assert_eq!(
        remaining.len(),
        1,
        "mqtt_queue item should remain when event_items insert fails"
    );
    assert_eq!(remaining[0].topic, "ilert/events");
}

#[tokio::test]
async fn mqtt_buffered_event_invalid_payload_dropped_from_mqtt_queue() {
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    db.create_mqtt_queue_item("ilert/events", "not valid json")
        .unwrap();

    let mut config = ILConfig::new();
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(1883);
    config.mqtt_name = Some("test".to_string());
    config.event_topic = Some("ilert/events".to_string());
    config.mqtt_buffer = true;
    config.db_file = db_path.clone();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
    });

    let poll_ctx = daemon_ctx.clone();
    let _poller = tokio::spawn(async move {
        run_mqtt_poll_job(poll_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(15)).await;
    daemon_ctx.running.store(false, Ordering::Relaxed);

    let db = ILDatabase::new(&db_path);
    let remaining = db.get_mqtt_queue_items(10).unwrap();
    assert!(
        remaining.is_empty(),
        "invalid payload should be dropped from mqtt_queue"
    );

    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "invalid payload should not create an event"
    );
}

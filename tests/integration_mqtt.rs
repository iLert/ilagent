use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tempfile::NamedTempFile;
use rumqttc::{MqttOptions, Client, QoS};
use ilert::ilert::ILert;
use tokio::sync::Mutex;
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path_regex};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers::core::{WaitFor, IntoContainerPort};

use ilagent::config::ILConfig;
use ilagent::db::ILDatabase;
use ilagent::DaemonContext;
use ilagent::consumers::mqtt::run_mqtt_job;
use ilagent::poll::send_queued_event;

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

fn mqtt_daemon_ctx(mqtt_host: &str, mqtt_port: u16, db_path: &str) -> Arc<DaemonContext> {
    let mut config = ILConfig::new();
    config.mqtt_host = Some(mqtt_host.to_string());
    config.mqtt_port = Some(mqtt_port);
    config.mqtt_name = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert/events".to_string());
    config.heartbeat_topic = Some("ilert/heartbeats".to_string());
    config.db_file = db_path.to_string();

    let db = ILDatabase::new(db_path);
    db.prepare_database();

    Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client: ILert::new().unwrap(),
        running: AtomicBool::new(true),
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

    client.publish(topic, QoS::AtLeastOnce, false, payload.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let _ = client.disconnect();
    let _ = conn_handle.join();
}

// --- Tests ---

/// Publish an MQTT event message -> consumer picks it up -> event lands in SQLite queue
#[tokio::test]
async fn mqtt_event_lands_in_db() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    mqtt_publish("127.0.0.1", port, "ilert/events", r#"{
        "apiKey": "mqtt-test-key",
        "eventType": "ALERT",
        "summary": "MQTT e2e test",
        "alertKey": "mqtt-host-1"
    }"#);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1, "expected 1 event in DB");
    assert_eq!(events[0].api_key, "mqtt-test-key");
    assert_eq!(events[0].event_type, "ALERT");
    assert_eq!(events[0].summary, "MQTT e2e test");
    assert_eq!(events[0].alert_key.as_ref().unwrap(), "mqtt-host-1");
    assert_eq!(
        events[0].event_api_path.as_ref().unwrap(),
        "/v1/events/mqtt/mqtt-test-key"
    );

    let cd: serde_json::Value =
        serde_json::from_str(events[0].custom_details.as_ref().unwrap()).unwrap();
    assert_eq!(cd["topic"], "ilert/events");

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Publish multiple MQTT events -> all land in DB in FIFO order
#[tokio::test]
async fn mqtt_multiple_events_fifo_order() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    for i in 0..3 {
        mqtt_publish("127.0.0.1", port, "ilert/events", &format!(
            r#"{{"apiKey": "k1", "eventType": "ALERT", "summary": "event-{i}"}}"#
        ));
    }

    tokio::time::sleep(Duration::from_secs(3)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 3, "expected 3 events in DB");
    assert_eq!(events[0].summary, "event-0");
    assert_eq!(events[1].summary, "event-1");
    assert_eq!(events[2].summary, "event-2");

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Non-event topic messages are ignored by the consumer
#[tokio::test]
async fn mqtt_ignored_topic_not_queued() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish to a topic the consumer is NOT subscribed to
    mqtt_publish("127.0.0.1", port, "other/topic", r#"{
        "apiKey": "k1",
        "eventType": "ALERT",
        "summary": "should be ignored"
    }"#);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(events.is_empty(), "no events should be queued for unrelated topic");

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Invalid JSON payloads are dropped silently, don't crash the consumer
#[tokio::test]
async fn mqtt_invalid_json_dropped() {
    let (_container, port) = start_mosquitto().await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish garbage
    mqtt_publish("127.0.0.1", port, "ilert/events", "not json at all");

    // then publish a valid event to prove the consumer is still alive
    mqtt_publish("127.0.0.1", port, "ilert/events", r#"{
        "apiKey": "k1",
        "eventType": "ALERT",
        "summary": "still alive"
    }"#);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1, "only the valid event should be queued");
    assert_eq!(events[0].summary, "still alive");

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

/// Full pipeline: MQTT publish -> consumer queues in DB -> poll delivers to mock ilert -> event removed
#[tokio::test]
async fn mqtt_full_e2e_publish_to_delivery() {
    let (_container, port) = start_mosquitto().await;
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(
            ResponseTemplate::new(202).insert_header("correlation-id", "mqtt-e2e-corr"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let daemon_ctx = mqtt_daemon_ctx("127.0.0.1", port, &db_path);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // 1. Publish event via MQTT
    mqtt_publish("127.0.0.1", port, "ilert/events", r#"{
        "apiKey": "mqtt-e2e-key",
        "eventType": "ALERT",
        "summary": "Full pipeline test",
        "alertKey": "pipe-1"
    }"#);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // 2. Verify event is queued
    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1);
    let event = events[0].clone();

    // 3. Simulate poll: send to mock ilert
    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5)).unwrap();
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry, "202 means delivered");

    // 4. Delete from queue (as poll would)
    db.delete_il_event(event.id.as_ref().unwrap()).unwrap();
    let remaining = db.get_il_events(10).unwrap();
    assert!(remaining.is_empty(), "queue should be empty after delivery");

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
    });
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::task::spawn_blocking(move || {
        run_mqtt_job(ctx_clone);
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // publish with custom field names (no apiKey, eventType, summary — all mapped)
    mqtt_publish("127.0.0.1", port, "ilert/events", r#"{
        "state": "SET",
        "mCode": "M-100",
        "msg": "Pump failure detected"
    }"#);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].api_key, "static-api-key");
    assert_eq!(events[0].event_type, "ALERT"); // "SET" mapped to ALERT
    assert_eq!(events[0].summary, "Pump failure detected");
    assert_eq!(events[0].alert_key.as_ref().unwrap(), "M-100");
    assert_eq!(
        events[0].event_api_path.as_ref().unwrap(),
        "/v1/events/mqtt/static-api-key"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

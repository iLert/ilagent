use actix_web::{App, test, web};
use ilert::ilert::ILert;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::NamedTempFile;
use tokio::sync::Mutex;

use ilagent::config::ILConfig;
use ilagent::db::ILDatabase;
use ilagent::http_server::{WebContextContainer, config_app};
use ilagent::{DaemonContext, KafkaProbeState, MqttProbeState};

fn test_container() -> (web::Data<Mutex<WebContextContainer>>, NamedTempFile) {
    let file = NamedTempFile::new().unwrap();
    let db = ILDatabase::new(file.path().to_str().unwrap());
    db.prepare_database();
    let ilert_client = ILert::new().unwrap();
    let container = web::Data::new(Mutex::new(WebContextContainer { db, ilert_client }));
    (container, file)
}

// --- health endpoints ---

#[actix_rt::test]
async fn get_index_returns_version() {
    let (container, _f) = test_container();
    let app =
        test::init_service(App::new().app_data(container.clone()).configure(config_app)).await;

    let req = test::TestRequest::get().uri("/").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = test::read_body(resp).await;
    let expected = format!("ilagent/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(body, expected.as_bytes());
}

#[actix_rt::test]
async fn get_ready_returns_204() {
    let (container, _f) = test_container();
    let app =
        test::init_service(App::new().app_data(container.clone()).configure(config_app)).await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_rt::test]
async fn get_health_returns_204() {
    let (container, _f) = test_container();
    let app =
        test::init_service(App::new().app_data(container.clone()).configure(config_app)).await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

// --- POST /api/events ---

#[actix_rt::test]
async fn post_event_valid_inserts_to_db() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    let payload = json!({
        "apiKey": "il1api123",
        "eventType": "ALERT",
        "summary": "Server down",
        "alertKey": "host-1"
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["integrationKey"], "il1api123");
    assert_eq!(body["eventType"], "ALERT");
    assert_eq!(body["summary"], "Server down");
    assert_eq!(body["alertKey"], "host-1");

    // verify it's actually in the database
    let c = container.lock().await;
    let events = c.db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].integration_key, "il1api123");
    assert_eq!(events[0].summary, "Server down");
}

#[actix_rt::test]
async fn post_event_with_all_fields() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    let payload = json!({
        "apiKey": "k1",
        "eventType": "RESOLVE",
        "summary": "Resolved",
        "details": "Detail text",
        "alertKey": "alert-42",
        "priority": "HIGH",
        "customDetails": {"env": "prod"},
        "images": [{"src": "https://img.example.com/a.png"}],
        "links": [{"href": "https://example.com", "text": "Dashboard"}]
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["eventType"], "RESOLVE");
    assert_eq!(body["priority"], "HIGH");
    assert_eq!(body["customDetails"]["env"], "prod");
}

#[actix_rt::test]
async fn post_event_bad_event_type_returns_400() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    let payload = json!({
        "apiKey": "k1",
        "eventType": "INVALID",
        "summary": "test"
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("eventType"));

    // verify nothing was inserted
    let c = container.lock().await;
    let events = c.db.get_il_events(10).unwrap();
    assert!(events.is_empty());
}

#[actix_rt::test]
async fn post_event_bad_priority_returns_400() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    let payload = json!({
        "apiKey": "k1",
        "eventType": "ALERT",
        "summary": "test",
        "priority": "MEDIUM"
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_rt::test]
async fn post_event_missing_required_fields_returns_400() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    // missing apiKey, eventType, summary
    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&json!({"details": "only details"}))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_rt::test]
async fn post_event_not_json_returns_400() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/events")
        .insert_header(("content-type", "application/json"))
        .set_payload("not json")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_rt::test]
async fn post_multiple_events_all_queued() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    for i in 0..3 {
        let payload = json!({
            "apiKey": format!("key{}", i),
            "eventType": "ALERT",
            "summary": format!("event {}", i)
        });
        let req = test::TestRequest::post()
            .uri("/api/events")
            .set_json(&payload)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    let c = container.lock().await;
    let events = c.db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 3);
}

// --- health/ready with daemon context ---

fn test_daemon_ctx(
    mqtt_probe: Option<MqttProbeState>,
) -> (
    web::Data<Mutex<WebContextContainer>>,
    web::Data<Arc<DaemonContext>>,
    NamedTempFile,
) {
    test_daemon_ctx_full(mqtt_probe, None)
}

fn test_daemon_ctx_full(
    mqtt_probe: Option<MqttProbeState>,
    kafka_probe: Option<KafkaProbeState>,
) -> (
    web::Data<Mutex<WebContextContainer>>,
    web::Data<Arc<DaemonContext>>,
    NamedTempFile,
) {
    let file = NamedTempFile::new().unwrap();
    let db_path = file.path().to_str().unwrap();
    let db = ILDatabase::new(db_path);
    db.prepare_database();
    let ilert_client = ILert::new().unwrap();
    let container = web::Data::new(Mutex::new(WebContextContainer {
        db: ILDatabase::new(db_path),
        ilert_client: ILert::new().unwrap(),
    }));

    let mut config = ILConfig::new();
    config.db_file = db_path.to_string();

    let daemon_ctx = Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe,
        kafka_probe,
    });

    (container, web::Data::new(daemon_ctx), file)
}

#[actix_rt::test]
async fn health_returns_204_while_running() {
    let (container, daemon_data, _f) = test_daemon_ctx(None);
    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_rt::test]
async fn health_returns_503_after_shutdown() {
    let (container, daemon_data, _f) = test_daemon_ctx(None);
    daemon_data.running.store(false, Ordering::Relaxed);

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);
}

#[actix_rt::test]
async fn ready_returns_503_mqtt_before_connection() {
    let probe = MqttProbeState::new(2);
    let (container, daemon_data, _f) = test_daemon_ctx(Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["component"], "mqtt");
    assert_eq!(body["connected"], false);
    assert_eq!(body["subscriptions_ready"], false);
}

#[actix_rt::test]
async fn ready_returns_503_mqtt_connected_but_no_suback() {
    let probe = MqttProbeState::new(2);
    probe.set_connected();
    let (container, daemon_data, _f) = test_daemon_ctx(Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert_eq!(body["subscriptions_ready"], false);
}

#[actix_rt::test]
async fn ready_returns_503_mqtt_partial_suback() {
    let probe = MqttProbeState::new(2);
    probe.set_connected();
    probe.record_suback_success();
    let (container, daemon_data, _f) = test_daemon_ctx(Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert_eq!(body["subscriptions_ready"], false);
}

#[actix_rt::test]
async fn ready_returns_204_mqtt_fully_ready() {
    let probe = MqttProbeState::new(2);
    probe.set_connected();
    probe.record_suback_success();
    probe.record_suback_success();
    let (container, daemon_data, _f) = test_daemon_ctx(Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_rt::test]
async fn ready_returns_503_after_mqtt_disconnect() {
    let probe = MqttProbeState::new(2);
    probe.set_connected();
    probe.record_suback_success();
    probe.record_suback_success();
    assert!(probe.is_ready());

    probe.reset();
    probe.record_error("Connection lost".to_string());

    let (container, daemon_data, _f) = test_daemon_ctx(Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["error"], "Connection lost");
}

#[actix_rt::test]
async fn ready_returns_503_when_shutting_down() {
    let (container, daemon_data, _f) = test_daemon_ctx(None);
    daemon_data.running.store(false, Ordering::Relaxed);

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["component"], "daemon");
}

#[actix_rt::test]
async fn ready_returns_204_http_only_mode() {
    let (container, daemon_data, _f) = test_daemon_ctx(None);

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

// --- Kafka readiness ---

#[actix_rt::test]
async fn ready_returns_503_kafka_before_consumer_started() {
    let probe = KafkaProbeState::new();
    let (container, daemon_data, _f) = test_daemon_ctx_full(None, Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["component"], "kafka");
    assert_eq!(body["consumer_started"], false);
    assert_eq!(body["subscribed"], false);
}

#[actix_rt::test]
async fn ready_returns_503_kafka_started_not_subscribed() {
    let probe = KafkaProbeState::new();
    probe.consumer_started.store(true, Ordering::Relaxed);
    let (container, daemon_data, _f) = test_daemon_ctx_full(None, Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["consumer_started"], true);
    assert_eq!(body["subscribed"], false);
}

#[actix_rt::test]
async fn ready_returns_204_kafka_subscribed_and_running() {
    let probe = KafkaProbeState::new();
    probe.consumer_started.store(true, Ordering::Relaxed);
    probe.subscribed.store(true, Ordering::Relaxed);
    let (container, daemon_data, _f) = test_daemon_ctx_full(None, Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_rt::test]
async fn ready_returns_503_kafka_worker_exited() {
    let probe = KafkaProbeState::new();
    probe.consumer_started.store(true, Ordering::Relaxed);
    probe.subscribed.store(true, Ordering::Relaxed);
    probe.worker_exited.store(true, Ordering::Relaxed);
    probe.record_error("broker connection lost".to_string());
    let (container, daemon_data, _f) = test_daemon_ctx_full(None, Some(probe));

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .configure(config_app),
    )
    .await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["component"], "kafka");
    assert_eq!(body["worker_exited"], true);
    assert_eq!(body["error"], "broker connection lost");
}

use actix_web::{App, test, web};
use ilert::ilert::ILert;
use serde_json::json;
use tempfile::NamedTempFile;
use tokio::sync::Mutex;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ilagent::db::ILDatabase;
use ilagent::http_server::{WebContextContainer, config_app};
use ilagent::models::event::EventQueueItemJson;
use ilagent::poll::send_queued_event;

fn test_setup(mock_uri: &str, db_path: &str) -> web::Data<Mutex<WebContextContainer>> {
    let db = ILDatabase::new(db_path);
    db.prepare_database();
    let ilert_client = ILert::new_with_opts(Some(mock_uri), None, Some(5), None).unwrap();
    web::Data::new(Mutex::new(WebContextContainer { db, ilert_client }))
}

/// Full pipeline: POST event via HTTP → lands in SQLite → poll sends to mock ilert → event removed
#[actix_rt::test]
async fn e2e_post_event_poll_deliver_and_delete() {
    let mock_server = MockServer::start().await;
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202).insert_header("correlation-id", "e2e-corr-1"))
        .expect(1)
        .mount(&mock_server)
        .await;

    let container = test_setup(mock_server.uri().as_str(), &db_path);

    // 1. POST event via HTTP
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
        "summary": "E2E test alert",
        "alertKey": "e2e-host-1"
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    // 2. Verify event is in the queue
    {
        let c = container.lock().await;
        let events = c.db.get_il_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].integration_key, "il1api123");
    }

    // 3. Simulate poll: read from DB, send to mock ilert, delete on success
    let (event, ilert_client) = {
        let c = container.lock().await;
        let events = c.db.get_il_events(1).unwrap();
        let event = events[0].clone();

        // create a separate ilert client pointing at mock
        let client = ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
        (event, client)
    };

    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(!should_retry, "202 means delivered successfully");

    // 4. Delete from queue (as poll would)
    {
        let c = container.lock().await;
        c.db.delete_il_event(event.id.as_ref().unwrap()).unwrap();
        let remaining = c.db.get_il_events(10).unwrap();
        assert!(remaining.is_empty(), "queue should be empty after delivery");
    }
}

/// Full pipeline: POST event → poll gets 429 → event stays in queue for retry
#[actix_rt::test]
async fn e2e_post_event_rate_limited_stays_in_queue() {
    let mock_server = MockServer::start().await;
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&mock_server)
        .await;

    let container = test_setup(mock_server.uri().as_str(), &db_path);

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
        "summary": "Rate limit test"
    });

    let req = test::TestRequest::post()
        .uri("/api/events")
        .set_json(&payload)
        .to_request();
    test::call_service(&app, req).await;

    // simulate poll attempt
    let event = {
        let c = container.lock().await;
        c.db.get_il_events(1).unwrap()[0].clone()
    };

    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let should_retry = send_queued_event(&ilert_client, &event).await;
    assert!(should_retry, "429 should signal retry");

    // event should still be in queue
    let c = container.lock().await;
    let remaining = c.db.get_il_events(10).unwrap();
    assert_eq!(remaining.len(), 1, "event must stay in queue for retry");
}

/// Full pipeline with MQTT-style event_api_path
#[actix_rt::test]
async fn e2e_mqtt_event_uses_custom_path() {
    let mock_server = MockServer::start().await;
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/mqtt/.*"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    // simulate what MQTT consumer does: parse → to_db with path → insert
    let db = ILDatabase::new(&db_path);
    db.prepare_database();

    let config = ilagent::config::ILConfig::new();
    let json_event = EventQueueItemJson::parse_event_json(
        &config,
        r#"{"apiKey": "mqttkey1", "eventType": "ALERT", "summary": "MQTT alert"}"#,
        "factory/sensors",
    )
    .unwrap();

    let api_path = format!("/v1/events/mqtt/{}", json_event.integrationKey);
    let db_event = EventQueueItemJson::to_db(json_event, Some(api_path));
    let inserted = db.create_il_event(&db_event).unwrap().unwrap();

    // poll sends it
    let ilert_client =
        ILert::new_with_opts(Some(mock_server.uri().as_str()), None, Some(5), None).unwrap();
    let should_retry = send_queued_event(&ilert_client, &inserted).await;
    assert!(!should_retry);
}

/// Multiple events queued, poll processes in FIFO order
#[actix_rt::test]
async fn e2e_fifo_delivery_order() {
    let mock_server = MockServer::start().await;
    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&mock_server)
        .await;

    let container = test_setup(mock_server.uri().as_str(), &db_path);

    let app = test::init_service(
        App::new()
            .app_data(container.clone())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app),
    )
    .await;

    // insert 3 events in order
    for i in 0..3 {
        let payload = json!({
            "apiKey": "k1",
            "eventType": "ALERT",
            "summary": format!("event-{}", i)
        });
        let req = test::TestRequest::post()
            .uri("/api/events")
            .set_json(&payload)
            .to_request();
        test::call_service(&app, req).await;
    }

    // verify FIFO order from DB
    let c = container.lock().await;
    let events = c.db.get_il_events(10).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].summary, "event-0");
    assert_eq!(events[1].summary, "event-1");
    assert_eq!(events[2].summary, "event-2");
}

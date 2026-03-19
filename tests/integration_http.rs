use actix_web::{test, web, App};
use tokio::sync::Mutex;
use tempfile::NamedTempFile;
use serde_json::json;
use ilert::ilert::ILert;

use ilagent::db::ILDatabase;
use ilagent::http_server::{config_app, WebContextContainer};

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
    let app = test::init_service(
        App::new().app_data(container.clone()).configure(config_app)
    ).await;

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
    let app = test::init_service(
        App::new().app_data(container.clone()).configure(config_app)
    ).await;

    let req = test::TestRequest::get().uri("/ready").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_rt::test]
async fn get_health_returns_204() {
    let (container, _f) = test_container();
    let app = test::init_service(
        App::new().app_data(container.clone()).configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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
            .configure(config_app)
    ).await;

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

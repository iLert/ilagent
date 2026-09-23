use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ilagent::config::ILConfig;
use ilagent::consumers::pull::{QUEUE_BACKPRESSURE_THRESHOLD, cursor_db_key, run_pull_job};
use ilagent::db::ILDatabase;
use ilagent::models::event_db::EventQueueItem;
use ilagent::{DaemonContext, PullProbeState};

fn build_pull_config(url: &str, name: &str) -> ILConfig {
    let mut config = ILConfig::new();
    config.pull_url = Some(url.to_string());
    config.pull_name = Some(name.to_string());
    config.pull_interval = 1;
    config.event_key = Some("il1api-test".to_string());
    config.db_file = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();
    config
}

fn build_ctx(config: ILConfig) -> Arc<DaemonContext> {
    let db = ILDatabase::new(config.db_file.as_str());
    db.prepare_database();
    let ilert_client = ilert::ilert::ILert::new().expect("ilert client");

    Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
        edge_connector_probe: None,
        pull_probe: Some(PullProbeState::new()),
    })
}

/// Run the pull job for `secs`, then shut it down cleanly.
async fn run_for(ctx: Arc<DaemonContext>, secs: u64) {
    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_pull_job(job_ctx).await;
    });
    tokio::time::sleep(Duration::from_secs(secs)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();
}

async fn queued_events(ctx: &Arc<DaemonContext>) -> Vec<EventQueueItem> {
    ctx.db.lock().await.get_il_events(500).unwrap()
}

async fn stored_cursor(ctx: &Arc<DaemonContext>, name: &str) -> Option<String> {
    ctx.db.lock().await.get_il_value(&cursor_db_key(name))
}

#[tokio::test]
async fn pull_fetches_and_enqueues_records() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/alerts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "a1", "name": "Disk full"},
                {"id": "a2", "name": "CPU high"}
            ]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&format!("{}/api/alerts", server.uri()), "test");
    config.pull_items_path = Some("items".to_string());
    config.pull_dedup_key = Some("id".to_string());
    config.map_key_summary = Some("name".to_string());
    config.map_key_alert_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 2).await;

    let events = queued_events(&ctx).await;
    assert_eq!(events.len(), 2, "both records should be queued");

    let summaries: Vec<String> = events.iter().map(|e| e.summary.clone()).collect();
    assert!(summaries.contains(&"Disk full".to_string()));
    assert!(summaries.contains(&"CPU high".to_string()));

    // Pull events use the plain /events path, not a typed consumer segment
    assert!(
        events.iter().all(|e| e.event_api_path.is_none()),
        "pull events must not carry a typed api path"
    );
    assert!(events.iter().all(|e| e.integration_key == "il1api-test"));
}

#[tokio::test]
async fn pull_dedups_repeated_snapshot() {
    let server = MockServer::start().await;

    // A snapshot source hands back the same open alerts on every poll.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "same-1", "name": "Still broken"}]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "snapshot");
    config.pull_items_path = Some("items".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    // Long enough for several cycles at a 1s interval.
    run_for(ctx.clone(), 4).await;

    let events = queued_events(&ctx).await;
    assert_eq!(
        events.len(),
        1,
        "the same record must only be enqueued once across polls"
    );
}

#[tokio::test]
async fn pull_without_dedup_key_re_enqueues_every_cycle() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "same-1", "name": "Still broken"}]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "no-dedup");
    config.pull_items_path = Some("items".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 4).await;

    // This is the documented cost of running without a dedup key — asserted so the
    // behaviour is deliberate rather than accidental.
    assert!(
        queued_events(&ctx).await.len() > 1,
        "without a dedup key the same record is re-enqueued on every poll"
    );
}

#[tokio::test]
async fn pull_token_cursor_pages_and_persists() {
    let server = MockServer::start().await;

    // First page: no cursor sent yet.
    Mock::given(method("GET"))
        .and(query_param("cursor", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "b1"}],
            "next_cursor": "page3"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "page3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "next_cursor": "page3"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "a1"}],
            "next_cursor": "page2"
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "paged");
    config.pull_items_path = Some("items".to_string());
    config.pull_cursor_mode = Some("token".to_string());
    config.pull_cursor_path = Some("next_cursor".to_string());
    config.pull_cursor_param = Some("cursor".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 3).await;

    assert_eq!(
        queued_events(&ctx).await.len(),
        2,
        "records from both pages should be queued"
    );
    assert_eq!(
        stored_cursor(&ctx, "paged").await,
        Some("page3".to_string()),
        "cursor should persist at the last page"
    );
}

#[tokio::test]
async fn pull_stops_paging_when_cursor_repeats() {
    let server = MockServer::start().await;

    // A server that always claims the same next cursor would loop forever without the
    // unchanged-cursor guard.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "stuck"}],
            "next_cursor": "always-the-same"
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "stuck");
    config.pull_items_path = Some("items".to_string());
    config.pull_cursor_mode = Some("token".to_string());
    config.pull_cursor_path = Some("next_cursor".to_string());
    config.pull_cursor_param = Some("cursor".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 3).await;

    // Dedup keeps it at one event; the point is that the job terminated at all.
    assert_eq!(queued_events(&ctx).await.len(), 1);
    assert_eq!(
        stored_cursor(&ctx, "stuck").await,
        Some("always-the-same".to_string())
    );
}

#[tokio::test]
async fn pull_resumes_from_stored_cursor() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "saved-position"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "resumed"}]
        })))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "resume");
    config.pull_items_path = Some("items".to_string());
    config.pull_cursor_mode = Some("token".to_string());
    config.pull_cursor_path = Some("next_cursor".to_string());
    config.pull_cursor_param = Some("cursor".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    // Simulate a previous run having stored a cursor.
    ctx.db
        .lock()
        .await
        .set_il_val(&cursor_db_key("resume"), "saved-position")
        .unwrap();

    run_for(ctx.clone(), 2).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_timestamp_cursor_sends_max_not_last() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("since", "1767225600000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1..)
        .mount(&server)
        .await;

    // Deliberately out of order: the max is in the middle.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "t1", "updated_at": 1767225000000i64},
                {"id": "t2", "updated_at": 1767225600000i64},
                {"id": "t3", "updated_at": 1767225300000i64}
            ]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "ts");
    config.pull_items_path = Some("items".to_string());
    config.pull_cursor_mode = Some("timestamp".to_string());
    config.pull_cursor_path = Some("updated_at".to_string());
    config.pull_cursor_param = Some("since".to_string());
    config.pull_cursor_format = Some("epoch_ms".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 3).await;

    assert_eq!(queued_events(&ctx).await.len(), 3);
    assert_eq!(
        stored_cursor(&ctx, "ts").await,
        Some("1767225600000".to_string()),
        "cursor must be the maximum timestamp, not the last record's"
    );
}

#[tokio::test]
async fn pull_sends_static_and_auth_headers() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(header("X-Tenant-ID", "tenant-42"))
        .and(header("X-API-Key", "secret-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "h1"}]})))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "headers");
    config.pull_items_path = Some("items".to_string());
    config.pull_headers = vec!["X-Tenant-ID=tenant-42".to_string()];
    config.pull_auth = Some("header".to_string());
    config.pull_auth_header = Some("X-API-Key".to_string());
    config.pull_auth_value = Some("secret-value".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 2).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_oauth_fetches_token_and_sends_bearer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok-abc",
            "expires_in": 3600
        })))
        // Token is cached across cycles, so this must not be hit once per poll.
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/events"))
        .and(header("Authorization", "Bearer tok-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "o1"}]})))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&format!("{}/api/events", server.uri()), "oauth");
    config.pull_items_path = Some("items".to_string());
    config.pull_auth = Some("oauth2".to_string());
    config.pull_token_url = Some(format!("{}/oauth/token", server.uri()));
    config.pull_client_id = Some("cid".to_string());
    config.pull_client_secret = Some("csecret".to_string());
    config.pull_scope = Some("token".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 4).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_posts_body_when_configured() {
    let server = MockServer::start().await;

    let rpc = json!({"jsonrpc": "2.0", "method": "problem.get", "id": 1});

    Mock::given(method("POST"))
        .and(body_json(&rpc))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"eventid": "z1", "name": "Zabbix problem"}]
        })))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "zabbix");
    config.pull_body = Some(rpc.to_string());
    config.pull_items_path = Some("result".to_string());
    config.pull_dedup_key = Some("eventid".to_string());
    config.map_key_summary = Some("name".to_string());

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 2).await;

    let events = queued_events(&ctx).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].summary, "Zabbix problem");
}

#[tokio::test]
async fn pull_does_not_advance_cursor_on_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "failing");
    config.pull_items_path = Some("items".to_string());
    config.pull_cursor_mode = Some("token".to_string());
    config.pull_cursor_path = Some("next_cursor".to_string());
    config.pull_cursor_param = Some("cursor".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);
    ctx.db
        .lock()
        .await
        .set_il_val(&cursor_db_key("failing"), "before-failure")
        .unwrap();

    run_for(ctx.clone(), 2).await;

    assert!(queued_events(&ctx).await.is_empty());
    assert_eq!(
        stored_cursor(&ctx, "failing").await,
        Some("before-failure".to_string()),
        "a failed fetch must leave the cursor where it was"
    );

    let probe = ctx.pull_probe.as_ref().unwrap();
    assert!(
        probe.last_error().is_some(),
        "probe should record the failure"
    );
    assert!(!probe.is_ready(), "readiness should reflect the failure");
}

#[tokio::test]
async fn pull_applies_backpressure_when_queue_is_deep() {
    let server = MockServer::start().await;

    // Any request at all is a failure of the backpressure check.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "x"}]})))
        .expect(0)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "backpressure");
    config.pull_items_path = Some("items".to_string());
    config.pull_dedup_key = Some("id".to_string());

    let ctx = build_ctx(config);

    {
        let db = ctx.db.lock().await;
        for i in 0..(QUEUE_BACKPRESSURE_THRESHOLD + 1) {
            let mut item = EventQueueItem::new();
            item.integration_key = "il1api-test".to_string();
            item.summary = format!("backlog {}", i);
            db.create_il_event(&item).unwrap();
        }
    }

    run_for(ctx.clone(), 2).await;
    // wiremock verifies .expect(0) on drop
}

#[tokio::test]
async fn pull_maps_word_severity_from_sophos_shaped_record() {
    let server = MockServer::start().await;

    // Shaped like a real Sophos Central SIEM event: severity is a word, and it sits on
    // the top-level `severity` key that collides with ilert's own typed field.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "s1", "name": "Malware detected", "severity": "high",
                 "type": "Event::Endpoint::Threat::Detected"},
                {"id": "s2", "name": "Policy changed", "severity": "low",
                 "type": "Event::Endpoint::UpdateSuccess"}
            ]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri(), "sophos-shaped");
    config.pull_items_path = Some("items".to_string());
    config.pull_dedup_key = Some("id".to_string());
    config.map_key_summary = Some("name".to_string());
    config.map_key_alert_key = Some("id".to_string());
    config.map_key_severity = Some("severity".to_string());
    config.map_val_severity = vec![
        "high=2".to_string(),
        "medium=3".to_string(),
        "low=4".to_string(),
    ];

    let ctx = build_ctx(config);
    run_for(ctx.clone(), 2).await;

    let events = queued_events(&ctx).await;
    assert_eq!(
        events.len(),
        2,
        "a word severity must not cause the record to be discarded during deserialize"
    );

    let mut by_key: Vec<(String, Option<i64>)> = events
        .iter()
        .map(|e| (e.alert_key.clone().unwrap_or_default(), e.severity))
        .collect();
    by_key.sort();

    assert_eq!(by_key[0], ("s1".to_string(), Some(2)));
    assert_eq!(by_key[1], ("s2".to_string(), Some(4)));
}

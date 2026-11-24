use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use ilagent::config::ILConfig;
use ilagent::consumers::pull::{
    MAX_CONSECUTIVE_PAGES, MAX_RESPONSE_BYTES, QUEUE_BACKPRESSURE_THRESHOLD, cursor_db_key,
    run_pull_job,
};
use ilagent::db::ILDatabase;
use ilagent::models::event_db::EventQueueItem;
use ilagent::{DaemonContext, PullProbeState};

const EVENT_KEY: &str = "il1api-test";

/// Snapshot config: no cursor, records at `items`, alertKey from `id`.
fn build_pull_config(url: &str) -> ILConfig {
    let mut config = ILConfig::new();
    config.pull_url = Some(url.to_string());
    config.pull_interval = 1;
    config.pull_items_path = Some("items".to_string());
    config.event_key = Some(EVENT_KEY.to_string());
    config.map_key_alert_key = Some("id".to_string());
    config.db_file = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();
    config
}

fn with_token_cursor(mut config: ILConfig) -> ILConfig {
    config.pull_cursor_path = Some("next_cursor".to_string());
    config.pull_cursor_param = Some("cursor".to_string());
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

/// Run the pull job until it has completed `cycles` passes through its loop, then shut
/// it down cleanly. Waiting on the probe rather than the clock keeps a slow CI runner
/// from cutting a test short.
async fn run_cycles(ctx: Arc<DaemonContext>, cycles: u64) {
    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_pull_job(job_ctx).await;
    });
    let probe = ctx.pull_probe.as_ref().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        // A job that exited early (panicked) is surfaced by handle.await below.
        while probe.cycles.load(Ordering::Relaxed) < cycles && !handle.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("pull job did not complete {} cycles in time", cycles));
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();
}

async fn queued_events(ctx: &Arc<DaemonContext>) -> Vec<EventQueueItem> {
    ctx.db.lock().await.get_il_events(500).unwrap()
}

fn cursor_key(ctx: &Arc<DaemonContext>) -> String {
    cursor_db_key(EVENT_KEY, ctx.config.pull_url.as_deref().unwrap())
}

async fn stored_cursor(ctx: &Arc<DaemonContext>) -> Option<String> {
    ctx.db.lock().await.get_il_value(&cursor_key(ctx))
}

async fn set_cursor(ctx: &Arc<DaemonContext>, value: &str) {
    ctx.db
        .lock()
        .await
        .set_il_val(&cursor_key(ctx), value)
        .unwrap();
}

async fn fill_queue(ctx: &Arc<DaemonContext>, count: i64) {
    let db = ctx.db.lock().await;
    for i in 0..count {
        let mut item = EventQueueItem::new();
        item.integration_key = EVENT_KEY.to_string();
        item.summary = format!("backlog {}", i);
        db.create_il_event(&item).unwrap();
    }
}

fn probe_error(ctx: &Arc<DaemonContext>) -> Option<String> {
    ctx.pull_probe.as_ref().unwrap().last_error()
}

// --- mapping ---

#[tokio::test]
async fn pull_fetches_maps_and_enqueues_records() {
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

    let mut config = build_pull_config(&format!("{}/api/alerts", server.uri()));
    config.map_key_summary = Some("name".to_string());

    let ctx = build_ctx(config);
    run_cycles(ctx.clone(), 1).await;

    let mut events = queued_events(&ctx).await;
    events.sort_by(|a, b| a.alert_key.cmp(&b.alert_key));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].alert_key.as_deref(), Some("a1"));
    assert_eq!(events[0].summary, "Disk full");
    assert_eq!(events[0].integration_key, EVENT_KEY);
    assert_eq!(events[0].event_type, "ALERT");
    assert!(
        events[0].event_api_path.is_none(),
        "pull events post to plain /events"
    );
    assert_eq!(events[1].alert_key.as_deref(), Some("a2"));
    assert!(probe_error(&ctx).is_none());
}

#[tokio::test]
async fn pull_vendor_fields_named_like_ilert_fields_do_not_drop_or_leak() {
    let server = MockServer::start().await;

    // Sophos-shaped: priority is a number, details an object, eventType a vendor word
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "s1",
                "name": "Malware detected",
                "priority": 1,
                "details": {},
                "labels": ["a"],
                "severity": "high",
                "eventType": "ThreatDetected"
            }]
        })))
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri());
    config.map_key_summary = Some("name".to_string());
    config.forward_message_payload = true;

    let ctx = build_ctx(config);
    run_cycles(ctx.clone(), 1).await;

    let events = queued_events(&ctx).await;
    assert_eq!(events.len(), 1, "the record must not be dropped");
    let event = &events[0];
    assert_eq!(event.event_type, "ALERT");
    assert_eq!(event.alert_key.as_deref(), Some("s1"));
    assert_eq!(event.summary, "Malware detected");
    assert!(event.priority.is_none());
    assert!(event.details.is_none());
    assert!(event.labels.is_none());
    assert!(event.severity.is_none());
    let details: serde_json::Value =
        serde_json::from_str(event.custom_details.as_deref().unwrap()).unwrap();
    assert_eq!(details["eventType"], "ThreatDetected", "payload forwarded");
}

#[tokio::test]
async fn pull_skips_non_object_records() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": ["just a string", 42, {"id": "ok"}]
        })))
        .mount(&server)
        .await;

    let ctx = build_ctx(build_pull_config(&server.uri()));
    run_cycles(ctx.clone(), 1).await;

    let events = queued_events(&ctx).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].alert_key.as_deref(), Some("ok"));
    assert!(probe_error(&ctx).is_none());
}

#[tokio::test]
async fn pull_snapshot_re_enqueues_every_cycle() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "same"}]})))
        .mount(&server)
        .await;

    let ctx = build_ctx(build_pull_config(&server.uri()));
    run_cycles(ctx.clone(), 2).await;

    // No local dedup: ilert folds these by alertKey.
    let events = queued_events(&ctx).await;
    assert!(events.len() >= 2);
    assert!(
        events
            .iter()
            .all(|e| e.alert_key.as_deref() == Some("same"))
    );
}

// --- token paging ---

#[tokio::test]
async fn pull_token_cursor_pages_persists_and_resumes() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "r1"}], "next_cursor": "page2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "r2"}], "next_cursor": "page3"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Last page: the cursor repeats, which ends the cycle. The next cycle resumes here.
    Mock::given(method("GET"))
        .and(query_param("cursor", "page3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "next_cursor": "page3"
        })))
        .expect(2..)
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    run_cycles(ctx.clone(), 2).await;

    let mut keys: Vec<String> = queued_events(&ctx)
        .await
        .into_iter()
        .filter_map(|e| e.alert_key)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["r1", "r2"]);
    assert_eq!(stored_cursor(&ctx).await.as_deref(), Some("page3"));
}

#[tokio::test]
async fn pull_resumes_from_stored_cursor() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "stored"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "resumed"}], "next_cursor": "stored"
        })))
        .expect(1..)
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    set_cursor(&ctx, "stored").await;
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_empty_page_with_new_token_advances_cursor() {
    let server = MockServer::start().await;

    // Sophos returns `items: []` together with a fresh next_cursor.
    Mock::given(method("GET"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "next_cursor": "c1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "after-empty"}], "next_cursor": "c1"
        })))
        .expect(1..)
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));

    run_cycles(ctx.clone(), 1).await;
    assert!(queued_events(&ctx).await.is_empty());
    assert_eq!(
        stored_cursor(&ctx).await.as_deref(),
        Some("c1"),
        "an empty page with a new token must persist it"
    );

    ctx.running.store(true, Ordering::Relaxed);
    let probe = ctx.pull_probe.as_ref().unwrap();
    let done = probe.cycles.load(Ordering::Relaxed);
    run_cycles(ctx.clone(), done + 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_stops_paging_when_cursor_repeats() {
    let server = MockServer::start().await;

    // without the unchanged-cursor guard a repeated cursor would loop forever
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "stuck"}],
            "next_cursor": "always-the-same"
        })))
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    run_cycles(ctx.clone(), 1).await;

    // First request has no cursor, the second sends it back and gets it again.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(
        stored_cursor(&ctx).await.as_deref(),
        Some("always-the-same")
    );
}

/// Hands out a new cursor on every request, so only the page cap ends the cycle.
struct EndlessPages(AtomicU32);

impl Respond for EndlessPages {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, Ordering::Relaxed);
        ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": format!("r{}", n)}],
            "next_cursor": format!("c{}", n)
        }))
    }
}

#[tokio::test]
async fn pull_respects_the_page_cap() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(EndlessPages(AtomicU32::new(0)))
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(
        server.received_requests().await.unwrap().len(),
        MAX_CONSECUTIVE_PAGES as usize
    );
    assert_eq!(
        queued_events(&ctx).await.len(),
        MAX_CONSECUTIVE_PAGES as usize
    );
    assert_eq!(
        stored_cursor(&ctx).await,
        Some(format!("c{}", MAX_CONSECUTIVE_PAGES - 1))
    );
}

// --- errors ---

#[tokio::test]
async fn pull_error_mid_cycle_keeps_the_cursor_of_the_last_good_page() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "good"}], "next_cursor": "next"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "next"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    set_cursor(&ctx, "start").await;
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
    assert_eq!(
        stored_cursor(&ctx).await.as_deref(),
        Some("next"),
        "the committed page advances, the failed one does not"
    );

    let probe = ctx.pull_probe.as_ref().unwrap();
    assert!(probe.last_error().unwrap().contains("status 500"));
    assert!(!probe.is_ready());
}

#[tokio::test]
async fn pull_missing_items_path_is_a_cycle_error() {
    let server = MockServer::start().await;

    // A JSON-RPC style error body must not pass as an empty poll.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "error": {"code": -32602, "message": "Invalid params", "data": "token=abc"},
            "next_cursor": "bogus"
        })))
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    set_cursor(&ctx, "before").await;
    run_cycles(ctx.clone(), 1).await;

    let err = probe_error(&ctx).expect("missing items path must be recorded");
    assert!(err.contains("'items' not found"), "got: {}", err);
    assert!(err.contains("jsonrpc"), "lists the keys: {}", err);
    assert!(!err.contains("token=abc"), "values must not leak: {}", err);
    assert!(!ctx.pull_probe.as_ref().unwrap().is_ready());
    assert_eq!(stored_cursor(&ctx).await.as_deref(), Some("before"));
    assert!(queued_events(&ctx).await.is_empty());
}

#[tokio::test]
async fn pull_oversized_response_is_a_cycle_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RESPONSE_BYTES + 1)),
        )
        .mount(&server)
        .await;

    let ctx = build_ctx(build_pull_config(&server.uri()));
    run_cycles(ctx.clone(), 1).await;

    let err = probe_error(&ctx).expect("oversized response must be recorded");
    assert!(err.contains("exceeds"), "got: {}", err);
    assert!(queued_events(&ctx).await.is_empty());
}

/// reqwest::Error's Display renders the full url, and that string reaches both the
/// error! line and the probe that `/ready` serves unauthenticated.
#[tokio::test]
async fn pull_transport_error_does_not_expose_the_url() {
    // port 1 is closed, which produces the transport error variant that carries the url
    let config = build_pull_config("http://127.0.0.1:1/siem/v1/events?api_key=SUPERSECRET");
    let ctx = build_ctx(config);

    run_cycles(ctx.clone(), 1).await;

    let err = probe_error(&ctx).expect("a refused connection must be recorded on the probe");
    assert!(
        err.starts_with("pull request failed"),
        "expected the transport error path, got: {}",
        err
    );
    assert!(
        !err.contains("SUPERSECRET"),
        "query-string secret reached the probe error: {}",
        err
    );
    assert!(
        !err.contains("127.0.0.1:1") && !err.contains("siem/v1/events"),
        "pull url reached the probe error: {}",
        err
    );
}

// --- oauth ---

fn oauth_config(server: &MockServer) -> ILConfig {
    let mut config = build_pull_config(&format!("{}/api/events", server.uri()));
    config.pull_oauth_url = Some(format!("{}/oauth/token", server.uri()));
    config.pull_oauth_client_id = Some("cid".to_string());
    config.pull_oauth_secret = Some("csecret".to_string());
    config.pull_oauth_scope = Some("token".to_string());
    config
}

#[tokio::test]
async fn pull_oauth_fetches_token_and_sends_bearer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(wiremock::matchers::body_string_contains(
            "grant_type=client_credentials",
        ))
        .and(wiremock::matchers::body_string_contains(
            "client_secret=csecret",
        ))
        .and(wiremock::matchers::body_string_contains("scope=token"))
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
        .expect(3..)
        .mount(&server)
        .await;

    let ctx = build_ctx(oauth_config(&server));
    run_cycles(ctx.clone(), 3).await;

    assert!(!queued_events(&ctx).await.is_empty());
    assert!(probe_error(&ctx).is_none());
}

async fn assert_token_refreshed_on(status: u16) {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok-old", "expires_in": 3600
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok-new", "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Revoked before its stated expiry.
    Mock::given(method("GET"))
        .and(path("/api/events"))
        .and(header("Authorization", "Bearer tok-old"))
        .respond_with(ResponseTemplate::new(status))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/events"))
        .and(header("Authorization", "Bearer tok-new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "o1"}]})))
        .expect(1..)
        .mount(&server)
        .await;

    let ctx = build_ctx(oauth_config(&server));
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1, "status {}", status);
    assert!(probe_error(&ctx).is_none(), "status {}", status);
}

#[tokio::test]
async fn pull_oauth_401_refreshes_token_and_retries() {
    assert_token_refreshed_on(401).await;
}

#[tokio::test]
async fn pull_oauth_403_refreshes_token_and_retries() {
    assert_token_refreshed_on(403).await;
}

#[tokio::test]
async fn pull_oauth_token_failure_is_a_probe_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = build_ctx(oauth_config(&server));
    run_cycles(ctx.clone(), 1).await;

    let err = probe_error(&ctx).unwrap();
    assert!(
        err.contains("oauth token request returned status 400"),
        "got: {}",
        err
    );
}

// --- headers and redirects ---

#[tokio::test]
async fn pull_sends_static_headers() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(header("X-Tenant-ID", "tenant-1"))
        .and(header("X-API-Key", "k=v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "h1"}]})))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&server.uri());
    config.pull_headers = vec![
        "X-Tenant-ID=tenant-1".to_string(),
        "X-API-Key=k=v".to_string(),
    ];

    let ctx = build_ctx(config);
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
}

#[tokio::test]
async fn pull_follows_a_same_origin_redirect_with_its_headers() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", "/new"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/new"))
        .and(header("X-API-Key", "secret-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "r1"}]})))
        .expect(1..)
        .mount(&server)
        .await;

    let mut config = build_pull_config(&format!("{}/old", server.uri()));
    config.pull_headers = vec!["X-API-Key=secret-value".to_string()];

    let ctx = build_ctx(config);
    run_cycles(ctx.clone(), 1).await;

    assert_eq!(queued_events(&ctx).await.len(), 1);
    assert!(probe_error(&ctx).is_none());
}

/// reqwest strips only the standard auth headers when a redirect changes host, so a key
/// in a custom header would reach whatever host the Location names.
#[tokio::test]
async fn pull_does_not_follow_a_cross_origin_redirect() {
    let source = MockServer::start().await;
    let elsewhere = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/collect", elsewhere.uri())),
        )
        .expect(1..)
        .mount(&source)
        .await;

    // Any request here, with or without the key, means the redirect was followed.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "x"}]})))
        .expect(0)
        .mount(&elsewhere)
        .await;

    let mut config = build_pull_config(&source.uri());
    config.pull_headers = vec!["X-API-Key=secret-value".to_string()];

    let ctx = build_ctx(config);
    run_cycles(ctx.clone(), 1).await;

    assert!(queued_events(&ctx).await.is_empty());
    let err = probe_error(&ctx).unwrap();
    assert!(
        err.contains("redirect status 302"),
        "expected the redirect error, got: {}",
        err
    );
    assert!(
        !err.contains(&elsewhere.uri()),
        "the Location reached the probe error: {}",
        err
    );
    // wiremock verifies .expect(0) on `elsewhere` on drop
}

// --- backpressure ---

#[tokio::test]
async fn pull_skips_fetch_when_queue_is_deep() {
    let server = MockServer::start().await;

    // Any request at all is a failure of the backpressure check.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "x"}]})))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = build_ctx(build_pull_config(&server.uri()));
    fill_queue(&ctx, QUEUE_BACKPRESSURE_THRESHOLD).await;

    run_cycles(ctx.clone(), 2).await;
    // wiremock verifies .expect(0) on drop
}

#[tokio::test]
async fn pull_stops_paging_when_queue_fills_mid_cycle() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "a"}, {"id": "b"}, {"id": "c"}],
            "next_cursor": "page2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(query_param("cursor", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "d"}], "next_cursor": "page3"
        })))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = build_ctx(with_token_cursor(build_pull_config(&server.uri())));
    // Below the threshold before the cycle, past it after the first page.
    fill_queue(&ctx, QUEUE_BACKPRESSURE_THRESHOLD - 2).await;

    run_cycles(ctx.clone(), 1).await;

    assert_eq!(
        stored_cursor(&ctx).await.as_deref(),
        Some("page2"),
        "the next cycle resumes where paging stopped"
    );
    assert!(probe_error(&ctx).is_none(), "backpressure is not an error");
}

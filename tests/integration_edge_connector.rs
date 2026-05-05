use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ilagent::config::ILConfig;
use ilagent::db::ILDatabase;
use ilagent::edge_connector::{MAX_CONSECUTIVE_REPOLLS, cursor_db_key, run_edge_connector_job};
use ilagent::{DaemonContext, EdgeConnectorProbeState};

use futures::StreamExt;
use rdkafka::Message;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use testcontainers::core::{ExecCommand, IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};

fn build_edge_config(poll_url: &str, delivery_url: &str, integration_key: &str) -> ILConfig {
    let mut config = ILConfig::new();
    config.edge_connector_key = Some(integration_key.to_string());
    config.edge_poll_interval = 1;
    config.edge_http_url = Some(delivery_url.to_string());
    config.edge_api_host = Some(poll_url.to_string());
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
        edge_connector_probe: Some(EdgeConnectorProbeState::new()),
    })
}

fn poll_response(items: Vec<serde_json::Value>) -> serde_json::Value {
    json!({ "items": items })
}

fn poll_item(id: i64, event_type: &str, alert_id: &str, summary: &str) -> serde_json::Value {
    json!({
        "id": id,
        "payload": {
            "id": alert_id,
            "summary": summary,
            "eventType": event_type,
            "status": "PENDING",
            "timestamp": "2026-05-04T10:00:00.000Z"
        }
    })
}

#[tokio::test]
async fn edge_connector_polls_and_delivers_via_http() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    let items = vec![
        poll_item(1, "alert-created", "100", "Server down"),
        poll_item(2, "alert-resolved", "100", "Server recovered"),
    ];

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .and(header("Authorization", "iec1:test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(items)))
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:test-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert!(probe.is_ready());
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 2);

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:test-key")).unwrap();
    assert_eq!(cursor, "2");
}

#[tokio::test]
async fn edge_connector_cursor_persists_across_polls() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                10,
                "alert-created",
                "1",
                "first",
            )])),
        )
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "10"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                20,
                "alert-created",
                "2",
                "second",
            )])),
        )
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:cursor-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(4)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:cursor-key")).unwrap();
    assert_eq!(cursor, "20");
}

#[tokio::test]
async fn edge_connector_delivery_failure_stops_batch() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    let items = vec![
        poll_item(1, "alert-created", "100", "ok"),
        poll_item(2, "alert-created", "101", "will fail"),
        poll_item(3, "alert-created", "102", "never reached"),
    ];

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(items.clone())))
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    // After first successful delivery (cursor=1), subsequent polls return the same items from cursor 1
    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![
            poll_item(2, "alert-created", "101", "will fail"),
            poll_item(3, "alert-created", "102", "never reached"),
        ])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                ResponseTemplate::new(200)
            } else {
                ResponseTemplate::new(500)
            }
        })
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:fail-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:fail-key")).unwrap();
    assert_eq!(
        cursor, "1",
        "cursor should only advance past the first successful item"
    );
}

#[tokio::test]
async fn edge_connector_4xx_skips_item_and_advances_cursor() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    let items = vec![
        poll_item(1, "alert-created", "100", "ok"),
        poll_item(2, "alert-created", "101", "bad request"),
        poll_item(3, "alert-created", "102", "also ok"),
    ];

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(items)))
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::Relaxed);
            if n == 1 {
                ResponseTemplate::new(400)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:skip-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(
        probe.items_delivered.load(Ordering::Relaxed),
        2,
        "items 1 and 3 should be counted as delivered"
    );

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:skip-key")).unwrap();
    assert_eq!(
        cursor, "3",
        "cursor should advance past all items including the 4xx-rejected one"
    );
}

#[tokio::test]
async fn edge_connector_delivers_correct_headers() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                42,
                "alert-created",
                "999",
                "test headers",
            )])),
        )
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .and(header("X-ilert-Event-Type", "alert-created"))
        .and(header("X-ilert-Alert-Id", "999"))
        .and(header("X-ilert-Edge-Item-Id", "42"))
        .and(header("Content-Type", "application/json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:header-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();
}

#[tokio::test]
async fn edge_connector_auth_failure_records_error() {
    let poll_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        "http://localhost:1/unused",
        "iec1:bad-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    let error = probe.last_error().unwrap();
    assert!(
        error.contains("authentication failed"),
        "expected auth error, got: {}",
        error
    );
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn edge_connector_ha_standby_does_not_deliver() {
    let poll_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("cluster-id", "test-cluster"))
        .and(query_param("instance-id", "node-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "role": "standby"
        })))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let mut config = build_edge_config(
        &poll_server.uri(),
        "http://localhost:1/unused",
        "iec1:ha-key",
    );
    config.edge_cluster_id = Some("test-cluster".to_string());
    config.edge_instance_id = Some("node-2".to_string());
    config.edge_standby_interval = 1;

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn edge_connector_ha_leader_delivers_with_last_processed_id() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("cluster-id", "ha-cluster"))
        .and(query_param("instance-id", "node-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [poll_item(5, "alert-created", "50", "ha test")],
            "role": "leader"
        })))
        .up_to_n_times(1)
        .named("first poll - returns 1 item")
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("last-processed-id", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "role": "leader"
        })))
        .up_to_n_times(5)
        .named("subsequent polls with last-processed-id")
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&delivery_server)
        .await;

    let mut config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:ha-leader-key",
    );
    config.edge_cluster_id = Some("ha-cluster".to_string());
    config.edge_instance_id = Some("node-1".to_string());

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 1);

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:ha-leader-key"));
    assert!(
        cursor.is_none(),
        "HA mode should not persist cursor to SQLite"
    );
}

#[tokio::test]
async fn edge_connector_immediate_repoll_on_full_batch() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    let mut full_batch: Vec<serde_json::Value> = Vec::new();
    for i in 1..=100 {
        full_batch.push(poll_item(
            i,
            "alert-created",
            &i.to_string(),
            &format!("item {}", i),
        ));
    }

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(full_batch)))
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                101,
                "alert-created",
                "101",
                "extra",
            )])),
        )
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "101"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:batch-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(4)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(
        probe.items_delivered.load(Ordering::Relaxed),
        101,
        "should have delivered all 101 items across 2 polls"
    );

    let db = ctx.db.lock().await;
    assert_eq!(
        db.get_il_value(&cursor_db_key("iec1:batch-key")).unwrap(),
        "101"
    );
}

#[tokio::test]
async fn edge_connector_caps_consecutive_repolls() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    let total_full_batches = MAX_CONSECUTIVE_REPOLLS + 5;

    for batch_idx in 0..total_full_batches {
        let start_id = (batch_idx as i64) * 100 + 1;
        let after_id = if batch_idx == 0 {
            "0".to_string()
        } else {
            (batch_idx as i64 * 100).to_string()
        };

        let mut batch: Vec<serde_json::Value> = Vec::new();
        for i in 0..100 {
            let id = start_id + i;
            batch.push(poll_item(
                id,
                "alert-created",
                &id.to_string(),
                &format!("item {}", id),
            ));
        }

        Mock::given(method("GET"))
            .and(path("/api/edge-connections/events"))
            .and(query_param("after-id", &after_id))
            .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(batch)))
            .up_to_n_times(1)
            .mount(&poll_server)
            .await;
    }

    let final_after_id = (MAX_CONSECUTIVE_REPOLLS as i64 * 100).to_string();
    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", &final_after_id))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(3)
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&delivery_server)
        .await;

    let mut config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:cap-key",
    );
    config.edge_poll_interval = 60;

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    // With MAX_CONSECUTIVE_REPOLLS=10, the loop processes 10 full batches then sleeps
    // for poll_interval (60s). We stop after 15s — plenty for 1000 deliveries but
    // well before the 60s sleep expires, so the second round never starts.
    tokio::time::sleep(Duration::from_secs(15)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    let delivered = probe.items_delivered.load(Ordering::Relaxed);
    assert_eq!(
        delivered,
        (MAX_CONSECUTIVE_REPOLLS as u64) * 100,
        "should stop re-polling after {} consecutive full batches (delivered {})",
        MAX_CONSECUTIVE_REPOLLS,
        delivered,
    );
}

#[tokio::test]
async fn edge_connector_cursor_scoped_per_integration_key() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&delivery_server)
        .await;

    // First connector with key-A polls and advances to item 20
    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .and(header("Authorization", "iec1:key-A"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                20,
                "alert-created",
                "1",
                "item A",
            )])),
        )
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "20"))
        .and(header("Authorization", "iec1:key-A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let db_path = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();

    let mut config_a = ILConfig::new();
    config_a.edge_connector_key = Some("iec1:key-A".to_string());
    config_a.edge_poll_interval = 1;
    config_a.edge_http_url = Some(format!("{}/webhook", delivery_server.uri()));
    config_a.edge_api_host = Some(poll_server.uri());
    config_a.db_file = db_path.clone();

    let db_a = ILDatabase::new(&db_path);
    db_a.prepare_database();
    let ctx_a = Arc::new(DaemonContext {
        config: config_a,
        db: Mutex::new(db_a),
        ilert_client: ilert::ilert::ILert::new().expect("ilert client"),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
        edge_connector_probe: Some(EdgeConnectorProbeState::new()),
    });

    let job_ctx = ctx_a.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx_a.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    // Verify key-A's cursor is at 20
    {
        let db = ctx_a.db.lock().await;
        assert_eq!(db.get_il_value(&cursor_db_key("iec1:key-A")).unwrap(), "20");
        // key-B should have no cursor yet
        assert!(db.get_il_value(&cursor_db_key("iec1:key-B")).is_none());
    }

    // Second connector with key-B on the same DB must start at afterId=0
    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .and(header("Authorization", "iec1:key-B"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                5,
                "alert-created",
                "2",
                "item B",
            )])),
        )
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "5"))
        .and(header("Authorization", "iec1:key-B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let mut config_b = ILConfig::new();
    config_b.edge_connector_key = Some("iec1:key-B".to_string());
    config_b.edge_poll_interval = 1;
    config_b.edge_http_url = Some(format!("{}/webhook", delivery_server.uri()));
    config_b.edge_api_host = Some(poll_server.uri());
    config_b.db_file = db_path.clone();

    let db_b = ILDatabase::new(&db_path);
    let ctx_b = Arc::new(DaemonContext {
        config: config_b,
        db: Mutex::new(db_b),
        ilert_client: ilert::ilert::ILert::new().expect("ilert client"),
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe: None,
        edge_connector_probe: Some(EdgeConnectorProbeState::new()),
    });

    let job_ctx = ctx_b.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx_b.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let db = ctx_b.db.lock().await;
    assert_eq!(db.get_il_value(&cursor_db_key("iec1:key-B")).unwrap(), "5");
    assert_eq!(
        db.get_il_value(&cursor_db_key("iec1:key-A")).unwrap(),
        "20",
        "key-A cursor must remain unchanged"
    );
}

#[tokio::test]
async fn edge_connector_readiness_recovers_after_error() {
    let poll_server = MockServer::start().await;
    let delivery_server = MockServer::start().await;

    // First poll returns 401 (error), then subsequent polls return success
    let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let call_count_clone = call_count.clone();
    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .respond_with(move |_: &wiremock::Request| {
            let n = call_count_clone.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                ResponseTemplate::new(401)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"items": [], "role": "leader"}))
            }
        })
        .mount(&poll_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&delivery_server)
        .await;

    let config = build_edge_config(
        &poll_server.uri(),
        &format!("{}/webhook", delivery_server.uri()),
        "iec1:recovery-key",
    );
    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    // Wait for first poll (error) + backoff (5s) + second poll (success)
    tokio::time::sleep(Duration::from_secs(8)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert!(
        probe.is_ready(),
        "readiness should recover after successful poll"
    );
    assert!(probe.last_error().is_none(), "error should be cleared");
}

#[tokio::test]
async fn edge_connector_ha_standby_keeps_readiness_healthy() {
    let poll_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "role": "standby"
        })))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let mut config = build_edge_config(
        &poll_server.uri(),
        "http://localhost:1/unused",
        "iec1:standby-ready-key",
    );
    config.edge_cluster_id = Some("cluster-1".to_string());
    config.edge_instance_id = Some("node-2".to_string());
    config.edge_standby_interval = 1;

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert!(
        probe.is_ready(),
        "standby response should keep readiness healthy"
    );
}

// --- Kafka edge delivery tests ---

fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn start_kafka_broker(topic: &str) -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    let host_port = find_free_port();

    let container = GenericImage::new("apache/kafka", "3.8.1")
        .with_exposed_port(9092.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Kafka Server started"))
        .with_mapped_port(host_port, 9092.tcp())
        .with_env_var("KAFKA_NODE_ID", "1")
        .with_env_var("KAFKA_PROCESS_ROLES", "broker,controller")
        .with_env_var(
            "KAFKA_LISTENERS",
            "PLAINTEXT://0.0.0.0:9092,CONTROLLER://0.0.0.0:9093",
        )
        .with_env_var(
            "KAFKA_ADVERTISED_LISTENERS",
            format!("PLAINTEXT://127.0.0.1:{}", host_port),
        )
        .with_env_var(
            "KAFKA_LISTENER_SECURITY_PROTOCOL_MAP",
            "CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT",
        )
        .with_env_var("KAFKA_CONTROLLER_LISTENER_NAMES", "CONTROLLER")
        .with_env_var("KAFKA_CONTROLLER_QUORUM_VOTERS", "1@localhost:9093")
        .with_env_var("KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR", "1")
        .with_env_var("KAFKA_GROUP_INITIAL_REBALANCE_DELAY_MS", "0")
        .with_startup_timeout(Duration::from_secs(60))
        .start()
        .await
        .expect("Failed to start Kafka container");

    container
        .exec(ExecCommand::new([
            "/opt/kafka/bin/kafka-topics.sh",
            "--bootstrap-server",
            "localhost:9092",
            "--create",
            "--topic",
            topic,
            "--partitions",
            "1",
            "--replication-factor",
            "1",
        ]))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(1)).await;
    (container, host_port)
}

#[tokio::test]
async fn edge_connector_kafka_delivery() {
    let edge_topic = "edge-delivery-test";
    let (_container, kafka_port) = start_kafka_broker(edge_topic).await;
    let broker = format!("127.0.0.1:{}", kafka_port);

    let poll_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![
            poll_item(1, "alert-created", "42", "test kafka delivery"),
            poll_item(2, "alert-resolved", "42", "resolved"),
        ])))
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let mut config = ILConfig::new();
    config.edge_connector_key = Some("iec1:kafka-test".to_string());
    config.edge_mode = Some("kafka".to_string());
    config.edge_topic = Some(edge_topic.to_string());
    config.kafka_brokers = Some(broker.clone());
    config.edge_poll_interval = 1;
    config.edge_api_host = Some(poll_server.uri());
    config.db_file = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(4)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 2);

    // Verify messages in Kafka
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &broker)
        .set("group.id", format!("verify-{}", uuid::Uuid::new_v4()))
        .set("auto.offset.reset", "earliest")
        .create()
        .expect("consumer creation failed");

    consumer.subscribe(&[edge_topic]).expect("subscribe failed");

    let mut stream = consumer.stream();

    let msg1 = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("timeout waiting for kafka message 1")
        .expect("stream ended")
        .expect("kafka error");
    let key1 = std::str::from_utf8(msg1.key().unwrap()).unwrap();
    let payload1: serde_json::Value = serde_json::from_slice(msg1.payload().unwrap()).unwrap();
    assert_eq!(key1, "42");
    assert_eq!(payload1["eventType"], "alert-created");
    assert_eq!(payload1["summary"], "test kafka delivery");

    let msg2 = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("timeout waiting for kafka message 2")
        .expect("stream ended")
        .expect("kafka error");
    let key2 = std::str::from_utf8(msg2.key().unwrap()).unwrap();
    let payload2: serde_json::Value = serde_json::from_slice(msg2.payload().unwrap()).unwrap();
    assert_eq!(key2, "42");
    assert_eq!(payload2["eventType"], "alert-resolved");

    // Verify cursor persisted
    let db = ctx.db.lock().await;
    assert_eq!(
        db.get_il_value(&cursor_db_key("iec1:kafka-test")).unwrap(),
        "2"
    );
}

// --- MQTT edge delivery tests ---

async fn start_mqtt_broker() -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    let mosquitto_conf = b"listener 1883 0.0.0.0\nallow_anonymous true\n".to_vec();

    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(1883.tcp())
        .with_wait_for(WaitFor::message_on_stderr("mosquitto version"))
        .with_copy_to("/mosquitto/config/mosquitto.conf", mosquitto_conf)
        .start()
        .await
        .expect("Failed to start mosquitto container");

    let port = container.get_host_port_ipv4(1883).await.unwrap();
    (container, port)
}

#[tokio::test]
async fn edge_connector_mqtt_delivery() {
    let edge_topic = "edge/delivery/test";
    let (_container, mqtt_port) = start_mqtt_broker().await;

    let poll_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(poll_response(vec![poll_item(
                1,
                "alert-created",
                "99",
                "test mqtt delivery",
            )])),
        )
        .up_to_n_times(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    // Subscribe to the edge topic BEFORE the edge connector publishes
    let sub_opts = rumqttc::MqttOptions::new(
        format!("verify-{}", uuid::Uuid::new_v4()),
        "127.0.0.1",
        mqtt_port,
    );
    let (sub_client, mut sub_eventloop) = rumqttc::AsyncClient::new(sub_opts, 100);
    sub_client
        .subscribe(edge_topic, rumqttc::QoS::AtLeastOnce)
        .await
        .unwrap();

    let received = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received_clone = received.clone();
    let sub_handle = tokio::spawn(async move {
        loop {
            match sub_eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(p))) => {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&p.payload) {
                        received_clone.lock().await.push(val);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // Give the subscriber time to connect and subscribe
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut config = ILConfig::new();
    config.edge_connector_key = Some("iec1:mqtt-test".to_string());
    config.edge_mode = Some("mqtt".to_string());
    config.edge_topic = Some(edge_topic.to_string());
    config.mqtt_host = Some("127.0.0.1".to_string());
    config.mqtt_port = Some(mqtt_port);
    config.mqtt_name = Some(format!("edge-{}", uuid::Uuid::new_v4()));
    config.mqtt_qos = 1;
    config.edge_poll_interval = 1;
    config.edge_api_host = Some(poll_server.uri());
    config.db_file = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(4)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    // Stop subscriber
    sub_client.disconnect().await.ok();
    sub_handle.abort();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 1);

    let msgs = received.lock().await;
    assert_eq!(msgs.len(), 1, "expected 1 MQTT message");
    assert_eq!(msgs[0]["eventType"], "alert-created");
    assert_eq!(msgs[0]["id"], "99");
    assert_eq!(msgs[0]["summary"], "test mqtt delivery");

    // Verify cursor persisted
    let db = ctx.db.lock().await;
    assert_eq!(
        db.get_il_value(&cursor_db_key("iec1:mqtt-test")).unwrap(),
        "1"
    );
}

#[tokio::test]
async fn edge_connector_delivers_via_stdout() {
    let poll_server = MockServer::start().await;

    let items = vec![
        poll_item(1, "alert-created", "200", "Disk full"),
        poll_item(2, "alert-resolved", "200", "Disk cleared"),
    ];

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(items)))
        .expect(1)
        .mount(&poll_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/edge-connections/events"))
        .and(query_param("after-id", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(poll_response(vec![])))
        .up_to_n_times(5)
        .mount(&poll_server)
        .await;

    let mut config = ILConfig::new();
    config.edge_connector_key = Some("iec1:stdout-test".to_string());
    config.edge_mode = Some("stdout".to_string());
    config.edge_poll_interval = 1;
    config.edge_api_host = Some(poll_server.uri());
    config.db_file = tempfile::NamedTempFile::new()
        .unwrap()
        .path()
        .to_string_lossy()
        .to_string();

    let ctx = build_ctx(config);

    let job_ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        run_edge_connector_job(job_ctx).await;
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    ctx.running.store(false, Ordering::Relaxed);
    handle.await.unwrap();

    let probe = ctx.edge_connector_probe.as_ref().unwrap();
    assert!(probe.is_ready());
    assert_eq!(probe.items_delivered.load(Ordering::Relaxed), 2);

    let db = ctx.db.lock().await;
    let cursor = db.get_il_value(&cursor_db_key("iec1:stdout-test")).unwrap();
    assert_eq!(cursor, "2");
}

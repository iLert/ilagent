use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ilert::ilert::ILert;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use tempfile::NamedTempFile;
use testcontainers::core::{ExecCommand, IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use tokio::sync::Mutex;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use ilagent::config::ILConfig;
use ilagent::consumers::kafka::run_kafka_job;
use ilagent::db::ILDatabase;
use ilagent::{DaemonContext, KafkaProbeState};

fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn start_kafka() -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    start_kafka_with_topics(&["ilert-events", "ilert-heartbeats"]).await
}

async fn start_kafka_with_topics(
    topics: &[&str],
) -> (testcontainers::ContainerAsync<GenericImage>, u16) {
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
        .expect("Failed to start Kafka container — is Docker running?");

    for topic in topics {
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
    }

    // brief pause for topic metadata to propagate
    tokio::time::sleep(Duration::from_secs(1)).await;

    (container, host_port)
}

fn kafka_daemon_ctx(
    broker: &str,
    db_path: &str,
    ilert_client: ILert,
    kafka_probe: Option<KafkaProbeState>,
) -> Arc<DaemonContext> {
    let mut config = ILConfig::new();
    config.kafka_brokers = Some(broker.to_string());
    config.kafka_group_id = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert-events".to_string());
    config.heartbeat_topic = Some("ilert-heartbeats".to_string());
    config.db_file = db_path.to_string();

    let db = ILDatabase::new(db_path);
    db.prepare_database();

    Arc::new(DaemonContext {
        config,
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
        mqtt_probe: None,
        kafka_probe,
    })
}

async fn kafka_produce(broker: &str, topic: &str, key: &str, payload: &str) {
    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", broker)
        .set("message.timeout.ms", "5000")
        .create()
        .expect("Producer creation failed");

    producer
        .send(
            FutureRecord::to(topic).key(key).payload(payload),
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to produce message");
}

async fn wait_for_attempts(attempts: &AtomicUsize, expected: usize, timeout: Duration) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if attempts.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        attempts.load(Ordering::SeqCst) >= expected,
        "expected at least {} attempts, got {}",
        expected,
        attempts.load(Ordering::SeqCst)
    );
}

async fn produce_until_attempts(
    broker: &str,
    topic: &str,
    key: &str,
    payload: &str,
    attempts: &AtomicUsize,
    expected: usize,
    timeout: Duration,
) {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        kafka_produce(broker, topic, key, payload).await;

        let produced_at = std::time::Instant::now();
        while produced_at.elapsed() < Duration::from_secs(2) {
            if attempts.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    assert!(
        attempts.load(Ordering::SeqCst) >= expected,
        "expected at least {} attempts, got {}",
        expected,
        attempts.load(Ordering::SeqCst)
    );
}

// --- Tests ---

#[tokio::test]
async fn kafka_event_delivered_to_ilert() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/kafka/.*"))
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
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, None);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    produce_until_attempts(
        &broker,
        "ilert-events",
        "test-key-1",
        r#"{"apiKey": "kafka-test-key", "eventType": "ALERT", "summary": "Kafka e2e test", "alertKey": "kafka-host-1"}"#,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    let db = ILDatabase::new(&db_path);
    let events = db.get_il_events(10).unwrap();
    assert!(
        events.is_empty(),
        "Kafka delivers directly to ilert, no DB queue"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_heartbeat_delivered() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    // il1hbt-prefixed keys use legacy GET /heartbeats/{key} on the main host
    Mock::given(method("GET"))
        .and(path_regex("/heartbeats/il1hbt.*"))
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
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, None);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    produce_until_attempts(
        &broker,
        "ilert-heartbeats",
        "hbt-key",
        r#"{"apiKey": "il1hbt-kafka-test"}"#,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_invalid_json_committed_without_crash() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/kafka/.*"))
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
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, None);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    // first, warm up the consumer with garbage + valid messages
    // the retry-produce loop handles the consumer group rebalance delay
    kafka_produce(&broker, "ilert-events", "bad", "not json at all").await;

    produce_until_attempts(
        &broker,
        "ilert-events",
        "good-key",
        r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "still alive"}"#,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    // give a moment to ensure no extra messages sneak through
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "only the valid event should reach ilert"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_multiple_events_all_delivered() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/kafka/.*"))
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
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, None);
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    // warm up the consumer with the first event
    produce_until_attempts(
        &broker,
        "ilert-events",
        "key-0",
        r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "event-0"}"#,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    for i in 1..3 {
        kafka_produce(
            &broker,
            "ilert-events",
            &format!("key-{}", i),
            &format!(
                r#"{{"apiKey": "k1", "eventType": "ALERT", "summary": "event-{}"}}"#,
                i
            ),
        )
        .await;
    }

    wait_for_attempts(&attempts, 3, Duration::from_secs(15)).await;
    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_event_with_config_mappings() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let capture_bodies = bodies.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/kafka/.*"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            capture_bodies.lock().unwrap().push(body);
            response_attempts.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(202)
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.kafka_brokers = Some(broker.clone());
    config.kafka_group_id = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert-events".to_string());
    config.db_file = db_path.clone();
    config.event_key = Some("static-api-key".to_string());
    config.map_key_summary = Some("msg".to_string());
    config.map_key_alert_key = Some("mCode".to_string());
    config.map_key_etype = Some("state".to_string());
    config.map_val_etype_alert = Some("SET".to_string());
    config.map_val_etype_resolve = Some("CLR".to_string());

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

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    produce_until_attempts(
        &broker,
        "ilert-events",
        "machine-1",
        r#"{"state": "SET", "mCode": "M-100", "msg": "Pump failure detected"}"#,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    let captured = bodies.lock().unwrap();
    let last = captured.last().expect("should have at least one body");

    let body: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(body["integrationKey"], "static-api-key");
    assert_eq!(body["eventType"], "ALERT");
    assert_eq!(body["summary"], "Pump failure detected");
    assert_eq!(body["alertKey"], "M-100");

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_event_forward_payload() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = attempts.clone();
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let capture_bodies = bodies.clone();

    Mock::given(method("POST"))
        .and(path_regex("/v1/events/kafka/.*"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            capture_bodies.lock().unwrap().push(body);
            response_attempts.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(202)
        })
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.kafka_brokers = Some(broker.clone());
    config.kafka_group_id = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert-events".to_string());
    config.db_file = db_path.clone();
    config.event_key = Some("fwd-key".to_string());
    config.map_key_etype = Some("eventType".to_string());
    config.map_val_etype_alert = Some("alertCreated".to_string());
    config.map_key_summary = Some("data.message".to_string());
    config.forward_message_payload = true;

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

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    let payload = r#"{
        "uuid": "a1b2c3",
        "location": "powerplant",
        "eventType": "alertCreated",
        "data": {
            "message": "Pressure anomaly detected",
            "priority": 2
        }
    }"#;

    produce_until_attempts(
        &broker,
        "ilert-events",
        "sensor-1",
        payload,
        &attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    let captured = bodies.lock().unwrap();
    let last = captured.last().unwrap();

    let body: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(body["integrationKey"], "fwd-key");
    assert_eq!(body["eventType"], "ALERT");
    assert_eq!(body["summary"], "Pressure anomaly detected");

    let cd = &body["customDetails"];
    assert_eq!(cd["uuid"], "a1b2c3");
    assert_eq!(cd["location"], "powerplant");
    assert_eq!(cd["data"]["priority"], 2);
    assert!(
        cd.get("topic").is_none(),
        "topic should not pollute forwarded payload"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn kafka_probe_becomes_ready() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client = ILert::new().unwrap();
    let probe = KafkaProbeState::new();
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, Some(probe));
    let ctx_clone = daemon_ctx.clone();

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    while started.elapsed() < timeout {
        if let Some(ref probe) = daemon_ctx.kafka_probe {
            if probe.is_ready() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let probe = daemon_ctx.kafka_probe.as_ref().unwrap();
    assert!(
        probe.consumer_started.load(Ordering::Relaxed),
        "consumer should have started"
    );
    assert!(
        probe.subscribed.load(Ordering::Relaxed),
        "consumer should be subscribed"
    );
    assert!(probe.is_ready(), "probe should report ready");
    assert!(
        !probe.worker_exited.load(Ordering::Relaxed),
        "worker should still be running"
    );

    daemon_ctx.running.store(false, Ordering::Relaxed);

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        probe.worker_exited.load(Ordering::Relaxed),
        "worker should have exited after shutdown"
    );
}

#[tokio::test]
async fn kafka_graceful_shutdown() {
    let (_container, port) = start_kafka().await;
    let broker = format!("127.0.0.1:{}", port);

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let ilert_client = ILert::new().unwrap();
    let daemon_ctx = kafka_daemon_ctx(&broker, &db_path, ilert_client, None);
    let ctx_clone = daemon_ctx.clone();

    let consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    tokio::time::sleep(Duration::from_secs(5)).await;

    daemon_ctx.running.store(false, Ordering::Relaxed);

    let result = tokio::time::timeout(Duration::from_secs(10), consumer).await;
    assert!(
        result.is_ok(),
        "consumer should exit within timeout after shutdown signal"
    );
    assert!(
        result.unwrap().is_ok(),
        "consumer should exit without panic"
    );
}

#[tokio::test]
async fn kafka_policy_delivered() {
    let (_container, port) =
        start_kafka_with_topics(&["ilert-events", "ilert-heartbeats", "ilert-policies"]).await;
    let broker = format!("127.0.0.1:{}", port);
    let mock_server = MockServer::start().await;

    let resolve_attempts = Arc::new(AtomicUsize::new(0));
    let response_resolve = resolve_attempts.clone();

    Mock::given(method("GET"))
        .and(path_regex("/api/escalation-policies/resolve.*"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": 42, "name": "Test Policy"})),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex("/api/users/search-email"))
        .respond_with(move |_req: &Request| {
            response_resolve.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 99,
                "email": "support@ilert.com",
                "username": "support"
            }))
        })
        .mount(&mock_server)
        .await;

    Mock::given(method("PUT"))
        .and(path_regex("/api/escalation-policies/42/levels/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let tmp = NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_str().unwrap().to_string();

    let mut config = ILConfig::new();
    config.kafka_brokers = Some(broker.clone());
    config.kafka_group_id = Some(format!("ilagent-test-{}", uuid::Uuid::new_v4()));
    config.event_topic = Some("ilert-events".to_string());
    config.heartbeat_topic = Some("ilert-heartbeats".to_string());
    config.policy_topic = Some("ilert-policies".to_string());
    config.policy_routing_keys = Some("location".to_string());
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

    let _consumer = tokio::spawn(async move {
        run_kafka_job(ctx_clone).await;
    });

    produce_until_attempts(
        &broker,
        "ilert-policies",
        "policy-1",
        r#"{
            "uuid": "550x8400-x29b-11d4-x716-446655440000",
            "location": "powerplant",
            "eventType": "active",
            "data": { "email": "support@ilert.com", "shift": "1" }
        }"#,
        &resolve_attempts,
        1,
        Duration::from_secs(30),
    )
    .await;

    daemon_ctx.running.store(false, Ordering::Relaxed);
}

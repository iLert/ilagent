use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use log::{debug, error, info, warn};
use serde::Deserialize;

use crate::DaemonContext;
const DEFAULT_LIMIT: i64 = 100;
const DEFAULT_API_HOST: &str = "https://api.ilert.com";
pub const MAX_CONSECUTIVE_REPOLLS: u32 = 10;
const BACKOFF_BASE_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 300;

#[derive(Debug)]
pub struct DeliveryError {
    pub message: String,
    pub retryable: bool,
}

impl DeliveryError {
    fn retryable(message: String) -> Self {
        Self {
            message,
            retryable: true,
        }
    }

    fn non_retryable(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PollResponse {
    pub items: Vec<EdgeConnectorItem>,
    pub role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EdgeConnectorItem {
    pub id: i64,
    pub payload: serde_json::Value,
}

pub fn cursor_db_key(integration_key: &str) -> String {
    use ring::digest;
    let hash = digest::digest(&digest::SHA256, integration_key.as_bytes());
    let hex: String = hash.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    format!("edge_cursor:{}", &hex[..16])
}

async fn load_cursor(ctx: &DaemonContext, db_key: &str) -> i64 {
    let db = ctx.db.lock().await;
    db.get_il_value(db_key)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
}

async fn save_cursor(ctx: &DaemonContext, db_key: &str, cursor: i64) {
    let db = ctx.db.lock().await;
    if let Err(e) = db.set_il_val(db_key, &cursor.to_string()) {
        warn!("Failed to persist edge cursor: {}", e);
    }
}

pub async fn poll_items(
    client: &reqwest::Client,
    base_url: &str,
    integration_key: &str,
    after_id: i64,
    limit: i64,
    cluster_id: Option<&str>,
    instance_id: Option<&str>,
    last_processed_id: Option<i64>,
) -> Result<PollResponse, String> {
    let url = format!("{}/api/edge-connections/events", base_url);

    let mut request = client
        .get(&url)
        .header("Authorization", integration_key)
        .header("User-Agent", crate::CALLER_AGENT)
        .query(&[("limit", limit.to_string())]);

    if let Some(cid) = cluster_id {
        request = request.query(&[("cluster-id", cid)]);
        if let Some(iid) = instance_id {
            request = request.query(&[("instance-id", iid)]);
        }
        if let Some(lpid) = last_processed_id {
            if lpid > 0 {
                request = request.query(&[("last-processed-id", lpid.to_string())]);
            }
        }
    } else {
        request = request.query(&[("after-id", after_id.to_string())]);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("poll request failed: {}", e))?;

    let status = response.status();
    if status.as_u16() == 401 {
        return Err("authentication failed — check integration key".to_string());
    }
    if !status.is_success() {
        return Err(format!("poll returned status {}", status));
    }

    response
        .json::<PollResponse>()
        .await
        .map_err(|e| format!("failed to parse poll response: {}", e))
}

async fn deliver_item(
    ctx: &DaemonContext,
    http_client: &reqwest::Client,
    kafka_producer: Option<&rdkafka::producer::FutureProducer>,
    mqtt_client: Option<&rumqttc::AsyncClient>,
    mqtt_ack: Option<&tokio::sync::Notify>,
    item: &EdgeConnectorItem,
) -> Result<(), DeliveryError> {
    let mode = ctx.config.edge_mode.as_deref().unwrap_or("http");
    match mode {
        "http" => deliver_http(ctx, http_client, item).await,
        "kafka" => {
            deliver_kafka(
                ctx,
                kafka_producer
                    .ok_or_else(|| DeliveryError::retryable("kafka producer not initialized".to_string()))?,
                item,
            )
            .await
        }
        "mqtt" => {
            deliver_mqtt(
                ctx,
                mqtt_client
                    .ok_or_else(|| DeliveryError::retryable("mqtt client not initialized".to_string()))?,
                mqtt_ack
                    .ok_or_else(|| DeliveryError::retryable("mqtt ack notify not initialized".to_string()))?,
                item,
            )
            .await
        }
        "script" => deliver_script(ctx, item).await,
        "stdout" => deliver_stdout(item),
        _ => Err(DeliveryError::non_retryable(format!("unknown edge mode: {}", mode))),
    }
}

async fn deliver_http(
    ctx: &DaemonContext,
    client: &reqwest::Client,
    item: &EdgeConnectorItem,
) -> Result<(), DeliveryError> {
    let url = ctx
        .config
        .edge_http_url
        .as_ref()
        .ok_or_else(|| DeliveryError::non_retryable("edge_http_url is required for http mode".to_string()))?;

    let method = ctx.config.edge_http_method.as_deref().unwrap_or("POST");
    let event_type = item
        .payload
        .get("eventType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let alert_id = item
        .payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let builder = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        _ => return Err(DeliveryError::non_retryable(format!("unsupported HTTP method: {}", method))),
    };

    let response = builder
        .header("Content-Type", "application/json")
        .header("X-ilert-Event-Type", event_type)
        .header("X-ilert-Alert-Id", alert_id)
        .header("X-ilert-Edge-Item-Id", item.id.to_string())
        .json(&item.payload)
        .send()
        .await
        .map_err(|e| DeliveryError::retryable(format!("http delivery failed: {}", e)))?;

    let status = response.status();
    if status.is_success() {
        Ok(())
    } else if status.as_u16() == 429 {
        Err(DeliveryError::retryable(format!(
            "http delivery returned status 429 (rate limited)"
        )))
    } else if status.is_client_error() {
        Err(DeliveryError::non_retryable(format!(
            "http delivery returned status {} — skipping item",
            status
        )))
    } else {
        Err(DeliveryError::retryable(format!(
            "http delivery returned status {}",
            status
        )))
    }
}

async fn deliver_kafka(
    ctx: &DaemonContext,
    producer: &rdkafka::producer::FutureProducer,
    item: &EdgeConnectorItem,
) -> Result<(), DeliveryError> {
    use rdkafka::producer::FutureRecord;

    let topic = ctx
        .config
        .edge_topic
        .as_deref()
        .ok_or_else(|| DeliveryError::non_retryable("edge_topic required for kafka mode".to_string()))?;

    let key = item
        .payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payload =
        serde_json::to_string(&item.payload).map_err(|e| DeliveryError::non_retryable(format!("serialize failed: {}", e)))?;

    let record = FutureRecord::to(topic).key(&key).payload(&payload);

    producer
        .send(record, Duration::from_secs(5))
        .await
        .map_err(|(e, _)| DeliveryError::retryable(format!("kafka delivery failed: {}", e)))?;

    Ok(())
}

async fn deliver_mqtt(
    ctx: &DaemonContext,
    client: &rumqttc::AsyncClient,
    ack_notify: &tokio::sync::Notify,
    item: &EdgeConnectorItem,
) -> Result<(), DeliveryError> {
    let topic = ctx
        .config
        .edge_topic
        .as_deref()
        .ok_or_else(|| DeliveryError::non_retryable("edge_topic required for mqtt mode".to_string()))?;

    let qos = match ctx.config.mqtt_qos {
        0 => rumqttc::QoS::AtMostOnce,
        1 => rumqttc::QoS::AtLeastOnce,
        _ => rumqttc::QoS::ExactlyOnce,
    };

    let payload =
        serde_json::to_vec(&item.payload).map_err(|e| DeliveryError::non_retryable(format!("serialize failed: {}", e)))?;

    client
        .publish(topic, qos, false, payload)
        .await
        .map_err(|e| DeliveryError::retryable(format!("mqtt delivery failed: {}", e)))?;

    if ctx.config.mqtt_qos > 0 {
        tokio::time::timeout(Duration::from_secs(10), ack_notify.notified())
            .await
            .map_err(|_| DeliveryError::retryable("mqtt broker did not acknowledge publish within 10s".to_string()))?;
    }

    Ok(())
}

fn deliver_stdout(item: &EdgeConnectorItem) -> Result<(), DeliveryError> {
    let payload =
        serde_json::to_string(&item.payload).map_err(|e| DeliveryError::non_retryable(format!("serialize failed: {}", e)))?;
    println!("{}", payload);
    Ok(())
}

async fn deliver_script(ctx: &DaemonContext, item: &EdgeConnectorItem) -> Result<(), DeliveryError> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let script = ctx
        .config
        .edge_script
        .as_ref()
        .ok_or_else(|| DeliveryError::non_retryable("edge_script is required for script mode".to_string()))?;

    let event_type = item
        .payload
        .get("eventType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let alert_id = item
        .payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let timestamp = item
        .payload
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let payload =
        serde_json::to_string(&item.payload).map_err(|e| DeliveryError::non_retryable(format!("serialize failed: {}", e)))?;

    let mut child = Command::new(script)
        .stdin(std::process::Stdio::piped())
        .env("ILERT_EVENT_TYPE", event_type)
        .env("ILERT_ALERT_ID", alert_id)
        .env("ILERT_EDGE_ITEM_ID", item.id.to_string())
        .env("ILERT_TIMESTAMP", timestamp)
        .spawn()
        .map_err(|e| DeliveryError::retryable(format!("failed to spawn script: {}", e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| DeliveryError::retryable(format!("failed to write to script stdin: {}", e)))?;
    }

    let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(result) => result.map_err(|e| DeliveryError::retryable(format!("script wait failed: {}", e)))?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(DeliveryError::retryable("script execution timed out (30s)".to_string()));
        }
    };

    if status.success() {
        Ok(())
    } else {
        Err(DeliveryError::retryable(format!("script exited with code {:?}", status.code())))
    }
}

fn create_kafka_producer(
    config: &crate::config::ILConfig,
) -> Result<rdkafka::producer::FutureProducer, String> {
    use rdkafka::config::ClientConfig;

    let brokers = config
        .kafka_brokers
        .as_ref()
        .ok_or("kafka_brokers required for kafka delivery mode")?;

    ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()
        .map_err(|e| format!("failed to create kafka producer: {}", e))
}

fn build_mqtt_options(config: &crate::config::ILConfig) -> Result<rumqttc::MqttOptions, String> {
    let host = config
        .mqtt_host
        .as_ref()
        .ok_or("mqtt_host required for mqtt delivery mode")?;
    let port = config.mqtt_port.unwrap_or(1883);
    let name = config.mqtt_name.as_deref().unwrap_or("ilagent");

    let mut options = rumqttc::MqttOptions::new(format!("{}-edge", name), host, port);
    options.set_keep_alive(Duration::from_secs(30));

    if let (Some(username), Some(password)) = (&config.mqtt_username, &config.mqtt_password) {
        options.set_credentials(username, password);
    }

    if config.mqtt_tls {
        let tls_material = crate::consumers::mqtt::TlsMaterial::try_load(config)?;
        if let Some(material) = tls_material {
            let transport = material.try_into_transport()?;
            options.set_transport(transport);
        } else {
            return Err("mqtt_tls requires --mqtt_ca for certificate authority".to_string());
        }
    }

    Ok(options)
}

pub fn validate_edge_config(config: &crate::config::ILConfig) {
    let mode = config.edge_mode.as_deref().unwrap_or("http");
    match mode {
        "http" => {
            if config.edge_http_url.is_none() {
                panic!("--edge_http_url is required when edge_mode is 'http'");
            }
        }
        "kafka" => {
            if config.kafka_brokers.is_none() {
                panic!("--kafka_brokers is required when edge_mode is 'kafka'");
            }
            if config.edge_topic.is_none() {
                panic!("--edge_topic is required when edge_mode is 'kafka'");
            }
        }
        "mqtt" => {
            if config.mqtt_host.is_none() {
                panic!("--mqtt_host is required when edge_mode is 'mqtt'");
            }
            if config.edge_topic.is_none() {
                panic!("--edge_topic is required when edge_mode is 'mqtt'");
            }
        }
        "script" => {
            if config.edge_script.is_none() {
                panic!("--edge_script is required when edge_mode is 'script'");
            }
        }
        "stdout" => {}
        _ => panic!(
            "Unknown edge_mode '{}', expected: http, kafka, mqtt, script, stdout",
            mode
        ),
    }
}

async fn interruptible_sleep(ctx: &Arc<DaemonContext>, secs: u64) {
    for _ in 0..secs {
        if !ctx.running.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn run_edge_connector_job(ctx: Arc<DaemonContext>) {
    let integration_key = ctx.config.edge_connector_key.as_ref().unwrap().clone();
    let poll_interval = ctx.config.edge_poll_interval;
    let standby_interval = ctx.config.edge_standby_interval;
    let limit = DEFAULT_LIMIT;
    let mut consecutive_repolls: u32 = 0;

    let base_url = ctx
        .config
        .edge_api_host
        .clone()
        .or_else(|| std::env::var("ILERT_API_HOST").ok())
        .unwrap_or_else(|| DEFAULT_API_HOST.to_string());

    let ha_mode = ctx.config.edge_cluster_id.is_some();
    let cluster_id = ctx.config.edge_cluster_id.clone();
    let instance_id = if ha_mode {
        Some(
            ctx.config
                .edge_instance_id
                .clone()
                .unwrap_or_else(|| format!("ilagent-{}", uuid::Uuid::new_v4())),
        )
    } else {
        None
    };

    let cursor_key = cursor_db_key(&integration_key);
    let mut cursor_id = if !ha_mode {
        load_cursor(&ctx, &cursor_key).await
    } else {
        0
    };

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to create HTTP client for edge connector");

    let mode = ctx.config.edge_mode.as_deref().unwrap_or("http");

    let kafka_producer: Option<rdkafka::producer::FutureProducer> = if mode == "kafka" {
        Some(create_kafka_producer(&ctx.config).expect("failed to create kafka producer"))
    } else {
        None
    };

    let mqtt_ack_notify = Arc::new(tokio::sync::Notify::new());
    let mut mqtt_eventloop_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mqtt_client: Option<rumqttc::AsyncClient> = if mode == "mqtt" {
        if ctx.config.mqtt_qos == 0 {
            warn!(
                "Edge MQTT delivery is using QoS 0; broker acknowledgements are unavailable and the cursor advances after local publish queueing"
            );
        }
        let options = build_mqtt_options(&ctx.config).expect("failed to build mqtt options");
        let (client, mut eventloop) = rumqttc::AsyncClient::new(options, 10);

        let mqtt_ctx = ctx.clone();
        let ack = mqtt_ack_notify.clone();
        mqtt_eventloop_handle = Some(tokio::spawn(async move {
            loop {
                if !mqtt_ctx.running.load(Ordering::Relaxed) {
                    break;
                }
                match eventloop.poll().await {
                    Ok(rumqttc::Event::Incoming(rumqttc::Packet::PubAck(_)))
                    | Ok(rumqttc::Event::Incoming(rumqttc::Packet::PubComp(_))) => {
                        ack.notify_one();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if !mqtt_ctx.running.load(Ordering::Relaxed) {
                            break;
                        }
                        warn!("Edge MQTT eventloop error: {}", e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }));

        Some(client)
    } else {
        None
    };

    let mut last_processed_id: Option<i64> = None;
    let mut consecutive_poll_failures: u32 = 0;

    if let Some(ref probe) = ctx.edge_connector_probe {
        probe.polling.store(true, Ordering::Relaxed);
    }

    info!(
        "Edge connector polling {} every {}s (mode: {}){}",
        base_url,
        poll_interval,
        mode,
        if ha_mode {
            format!(", HA cluster: {}", cluster_id.as_deref().unwrap_or("?"))
        } else {
            String::new()
        }
    );

    loop {
        if !ctx.running.load(Ordering::Relaxed) {
            break;
        }

        if let Some(ref handle) = mqtt_eventloop_handle {
            if handle.is_finished() {
                error!("MQTT event loop exited unexpectedly, shutting down edge connector");
                if let Some(ref probe) = ctx.edge_connector_probe {
                    probe.record_error("MQTT event loop exited unexpectedly".to_string());
                }
                break;
            }
        }

        let result = poll_items(
            &http_client,
            &base_url,
            &integration_key,
            cursor_id,
            limit,
            cluster_id.as_deref(),
            instance_id.as_deref(),
            last_processed_id,
        )
        .await;

        match result {
            Ok(response) => {
                consecutive_poll_failures = 0;

                if let Some(ref probe) = ctx.edge_connector_probe {
                    probe.clear_error();
                }

                if let Some(ref role) = response.role {
                    if role == "standby" {
                        debug!("Edge connector standby, sleeping {}s", standby_interval);
                        interruptible_sleep(&ctx, standby_interval).await;
                        continue;
                    }
                }

                let batch_size = response.items.len();
                let mut delivery_failed = false;

                for item in &response.items {
                    match deliver_item(
                        &ctx,
                        &http_client,
                        kafka_producer.as_ref(),
                        mqtt_client.as_ref(),
                        Some(&mqtt_ack_notify),
                        item,
                    )
                    .await
                    {
                        Ok(()) => {
                            cursor_id = item.id;
                            last_processed_id = Some(item.id);

                            if !ha_mode {
                                save_cursor(&ctx, &cursor_key, cursor_id).await;
                            }

                            if let Some(ref probe) = ctx.edge_connector_probe {
                                probe.items_delivered.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) if !e.retryable => {
                            warn!(
                                "Edge connector delivery non-retryable for item {}: {}",
                                item.id, e.message
                            );
                            cursor_id = item.id;
                            last_processed_id = Some(item.id);

                            if !ha_mode {
                                save_cursor(&ctx, &cursor_key, cursor_id).await;
                            }
                        }
                        Err(e) => {
                            error!("Edge connector delivery failed (retryable): {}", e.message);
                            if let Some(ref probe) = ctx.edge_connector_probe {
                                probe.record_error(e.message);
                            }
                            delivery_failed = true;
                            break;
                        }
                    }
                }

                if batch_size as i64 >= limit && !delivery_failed {
                    consecutive_repolls += 1;
                    if consecutive_repolls < MAX_CONSECUTIVE_REPOLLS {
                        continue;
                    }
                    debug!(
                        "Edge connector reached max consecutive re-polls ({}), pausing",
                        MAX_CONSECUTIVE_REPOLLS
                    );
                }

                consecutive_repolls = 0;
            }
            Err(e) => {
                consecutive_poll_failures += 1;
                let backoff = std::cmp::min(
                    BACKOFF_BASE_SECS * 2u64.saturating_pow(consecutive_poll_failures - 1),
                    BACKOFF_MAX_SECS,
                );
                error!(
                    "Edge connector poll failed (attempt {}, backoff {}s): {}",
                    consecutive_poll_failures, backoff, e
                );
                if let Some(ref probe) = ctx.edge_connector_probe {
                    probe.record_error(e);
                }
                interruptible_sleep(&ctx, backoff).await;
                continue;
            }
        }

        interruptible_sleep(&ctx, poll_interval).await;
    }

    info!("Edge connector stopped");
}

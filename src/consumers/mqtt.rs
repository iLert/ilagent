use crate::config::ILConfig;
use crate::db::ILDatabase;
use crate::models::event::EventQueueItemJson;
use crate::{DaemonContext, hbt};
use log::{error, info, warn};
use rumqttc::{
    Client, Event, Incoming, MqttOptions, Publish, QoS, RecvTimeoutError, SubscribeReasonCode,
    TlsConfiguration, Transport,
};
use rustls::RootCertStore;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{str, thread};

#[derive(Debug, PartialEq)]
pub enum MessageType {
    Heartbeat,
    Event,
    Policy,
    Ignored,
}

fn topic_filter_matches(filter: &str, topic: &str) -> bool {
    let filter_parts: Vec<&str> = filter.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    for (i, f) in filter_parts.iter().enumerate() {
        if *f == "#" {
            return true;
        }
        if i >= topic_parts.len() {
            return false;
        }
        if *f != "+" && *f != topic_parts[i] {
            return false;
        }
    }
    filter_parts.len() == topic_parts.len()
}

pub(crate) fn classify_configured_message(
    message_topic: &str,
    event_topic: Option<&str>,
    heartbeat_topic: Option<&str>,
    policy_topic: Option<&str>,
) -> MessageType {
    if let Some(ht) = heartbeat_topic {
        if message_topic == ht {
            return MessageType::Heartbeat;
        }
    }
    if let Some(pt) = policy_topic {
        if topic_filter_matches(pt, message_topic) {
            return MessageType::Policy;
        }
    }
    if let Some(et) = event_topic {
        if message_topic == et {
            return MessageType::Event;
        }
        if et.contains('#') || et.contains('+') {
            return MessageType::Event;
        }
    }
    MessageType::Ignored
}

pub fn classify_message(
    message_topic: &str,
    event_topic: &str,
    heartbeat_topic: &str,
    policy_topic: Option<&str>,
) -> MessageType {
    classify_configured_message(
        message_topic,
        Some(event_topic),
        Some(heartbeat_topic),
        policy_topic,
    )
}

fn configured_mqtt_topics(config: &ILConfig) -> Vec<&str> {
    let mut topics = Vec::new();
    if let Some(topic) = config.event_topic.as_deref() {
        topics.push(topic);
    }
    if let Some(topic) = config.heartbeat_topic.as_deref() {
        topics.push(topic);
    }
    if let Some(topic) = config.policy_topic.as_deref() {
        topics.push(topic);
    }
    topics
}

fn describe_mqtt_topics(config: &ILConfig) -> String {
    configured_mqtt_topics(config).join(", ")
}

pub fn configured_topic_count(config: &ILConfig) -> u32 {
    configured_mqtt_topics(config).len() as u32
}

fn has_configured_mqtt_topics(config: &ILConfig) -> bool {
    config.event_topic.is_some()
        || config.heartbeat_topic.is_some()
        || config.policy_topic.is_some()
}

pub fn validate_mqtt_topics(config: &ILConfig) {
    if config.mqtt_host.is_some() && !has_configured_mqtt_topics(config) {
        panic!(
            "At least one MQTT topic must be configured: --event_topic, --heartbeat_topic, or --policy_topic"
        );
    }
}

pub fn validate_mqtt_config(config: &ILConfig) {
    validate_mqtt_topics(config);
    if config.mqtt_host.is_some() && !config.mqtt_buffer && config.mqtt_qos == 0 {
        panic!(
            "MQTT non-buffered mode requires --mqtt_qos 1 or --mqtt_qos 2 because QoS 0 has no broker acknowledgement to delay when ilert delivery fails. Use --mqtt_buffer if you need to accept QoS 0 messages."
        );
    }
}

pub fn prepare_mqtt_event(
    config: &ILConfig,
    payload: &str,
    topic: &str,
) -> Option<EventQueueItemJson> {
    super::prepare_consumer_event(config, payload, topic, json!({"topic": topic}))
}

pub fn build_event_api_path(integration_key: &str) -> String {
    super::build_event_api_path("mqtt", integration_key)
}

struct TlsMaterial {
    ca: Vec<u8>,
    client_auth: Option<(Vec<u8>, Vec<u8>)>,
    fingerprint: u64,
}

impl TlsMaterial {
    fn try_load(config: &ILConfig) -> Result<Option<TlsMaterial>, String> {
        let ca = match &config.mqtt_ca_path {
            Some(path) => std::fs::read(path)
                .map_err(|e| format!("Failed to read MQTT CA certificate file {}: {}", path, e))?,
            None => return Ok(None),
        };

        let client_auth = match (&config.mqtt_client_cert_path, &config.mqtt_client_key_path) {
            (Some(cert_path), Some(key_path)) => {
                let cert = std::fs::read(cert_path).map_err(|e| {
                    format!(
                        "Failed to read MQTT client certificate file {}: {}",
                        cert_path, e
                    )
                })?;
                let key = std::fs::read(key_path).map_err(|e| {
                    format!("Failed to read MQTT client key file {}: {}", key_path, e)
                })?;
                Some((cert, key))
            }
            _ => None,
        };

        let mut hasher = DefaultHasher::new();
        ca.hash(&mut hasher);
        if let Some((ref cert, ref key)) = client_auth {
            cert.hash(&mut hasher);
            key.hash(&mut hasher);
        }

        Ok(Some(TlsMaterial {
            ca,
            client_auth,
            fingerprint: hasher.finish(),
        }))
    }

    fn try_into_transport(self) -> Result<Transport, String> {
        let mut root_store = RootCertStore::empty();
        let ca_certs: Vec<_> = rustls_pemfile::certs(&mut &self.ca[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to parse CA certificate PEM: {}", e))?;
        if ca_certs.is_empty() {
            return Err("CA certificate file contains no certificates".to_string());
        }
        for cert in &ca_certs {
            root_store
                .add(cert.clone())
                .map_err(|e| format!("Invalid CA certificate: {}", e))?;
        }

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config_builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("Failed to configure TLS protocol versions: {}", e))?
            .with_root_certificates(root_store);

        let client_config = if let Some((cert_bytes, key_bytes)) = self.client_auth {
            let certs: Vec<_> = rustls_pemfile::certs(&mut &cert_bytes[..])
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to parse client certificate PEM: {}", e))?;
            if certs.is_empty() {
                return Err("Client certificate file contains no certificates".to_string());
            }
            let key = rustls_pemfile::private_key(&mut &key_bytes[..])
                .map_err(|e| format!("Failed to parse client key PEM: {}", e))?
                .ok_or_else(|| "Client key file contains no private key".to_string())?;
            config_builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| format!("Client certificate and key are incompatible: {}", e))?
        } else {
            config_builder.with_no_client_auth()
        };

        Ok(Transport::tls_with_config(TlsConfiguration::Rustls(
            Arc::new(client_config),
        )))
    }
}

pub fn run_mqtt_job(daemon_ctx: Arc<DaemonContext>) -> () {
    let mut connected = false;
    let mut recon_attempts: u32 = 0;

    let db = ILDatabase::new(daemon_ctx.config.db_file.as_str());

    let mqtt_host = daemon_ctx
        .config
        .mqtt_host
        .clone()
        .expect("Missing mqtt host");
    let mqtt_port = daemon_ctx
        .config
        .mqtt_port
        .clone()
        .expect("Missing mqtt port");
    let mqtt_name = daemon_ctx
        .config
        .mqtt_name
        .clone()
        .expect("Missing mqtt name");

    validate_mqtt_config(&daemon_ctx.config);
    let event_topic = daemon_ctx.config.event_topic.clone();
    let heartbeat_topic = daemon_ctx.config.heartbeat_topic.clone();
    let policy_topic = daemon_ctx.config.policy_topic.clone();

    let qos = match daemon_ctx.config.mqtt_qos {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    };

    let shared_prefix = daemon_ctx
        .config
        .mqtt_shared_group
        .as_ref()
        .map(|g| format!("$share/{}/", g));

    let sub_topic = |topic: &str| -> String {
        match &shared_prefix {
            Some(prefix) => format!("{}{}", prefix, topic),
            None => topic.to_string(),
        }
    };

    let mut active_tls_fp: Option<u64> = None;
    let mut staged_transport: Option<(Transport, u64)> = None;

    if daemon_ctx.config.mqtt_tls {
        match TlsMaterial::try_load(&daemon_ctx.config) {
            Ok(Some(material)) => {
                let fp = material.fingerprint;
                let transport = material
                    .try_into_transport()
                    .expect("Invalid TLS certificate material");
                active_tls_fp = Some(fp);
                staged_transport = Some((transport, fp));
            }
            Ok(None) => {
                warn!("No CA certificate provided, using system default certificates");
            }
            Err(e) => panic!("{}", e),
        }
    }

    loop {
        if let Some(ref probe) = daemon_ctx.mqtt_probe {
            probe.reset();
        }

        let mut mqtt_options = MqttOptions::new(mqtt_name.as_str(), mqtt_host.as_str(), mqtt_port);

        mqtt_options
            .set_keep_alive(Duration::from_secs(5))
            .set_pending_throttle(Duration::from_secs(1))
            .set_clean_session(false)
            .set_manual_acks(true);

        if let Some(mqtt_username) = daemon_ctx.config.mqtt_username.clone() {
            mqtt_options.set_credentials(
                mqtt_username.as_str(),
                daemon_ctx
                    .config
                    .mqtt_password
                    .clone()
                    .expect("mqtt_username is set, expecting mqtt_password to be set as well")
                    .as_str(),
            );
        }

        let mut attempt_tls_fp: Option<u64> = None;

        if daemon_ctx.config.mqtt_tls {
            let transport = if let Some((transport, fp)) = staged_transport.take() {
                attempt_tls_fp = Some(fp);
                transport
            } else if active_tls_fp.is_some() {
                match TlsMaterial::try_load(&daemon_ctx.config) {
                    Ok(Some(material)) => {
                        let fp = material.fingerprint;
                        match material.try_into_transport() {
                            Ok(transport) => {
                                attempt_tls_fp = Some(fp);
                                transport
                            }
                            Err(e) => {
                                error!("Failed to build TLS transport: {}", e);
                                let delay_ms =
                                    std::cmp::min(100 * 2u64.pow(recon_attempts.min(10)), 30_000);
                                recon_attempts += 1;
                                thread::sleep(Duration::from_millis(delay_ms));
                                continue;
                            }
                        }
                    }
                    Ok(None) => Transport::tls_with_default_config(),
                    Err(e) => {
                        error!("Failed to load TLS certificates for reconnect: {}", e);
                        let delay_ms =
                            std::cmp::min(100 * 2u64.pow(recon_attempts.min(10)), 30_000);
                        recon_attempts += 1;
                        thread::sleep(Duration::from_millis(delay_ms));
                        continue;
                    }
                }
            } else {
                Transport::tls_with_default_config()
            };
            mqtt_options.set_transport(transport);
            info!("MQTT TLS enabled");
        }

        let (client, mut connection) = Client::new(mqtt_options, 10);

        if let Some(ref topic) = event_topic {
            client
                .subscribe(sub_topic(topic).as_str(), qos)
                .expect("Failed to subscribe to mqtt event topic");
        }

        if let Some(ref topic) = heartbeat_topic {
            client
                .subscribe(sub_topic(topic).as_str(), qos)
                .expect("Failed to subscribe to mqtt heartbeat topic");
        }

        if let Some(ref topic) = policy_topic {
            client
                .subscribe(sub_topic(topic).as_str(), qos)
                .expect("Failed to subscribe to mqtt policy topic");
        }

        info!(
            "Subscribing to mqtt topics {} (QoS {:?}{})",
            describe_mqtt_topics(&daemon_ctx.config),
            qos,
            shared_prefix
                .as_ref()
                .map(|p| format!(", shared: {}", p))
                .unwrap_or_default()
        );

        let mut tls_reload_requested = false;
        let mut last_tls_check = std::time::Instant::now();
        let tls_check_interval = Duration::from_secs(30);

        info!("Connecting to Mqtt server..");
        loop {
            if !daemon_ctx.running.load(Ordering::Relaxed) {
                break;
            }

            if active_tls_fp.is_some() && last_tls_check.elapsed() >= tls_check_interval {
                last_tls_check = std::time::Instant::now();
                match TlsMaterial::try_load(&daemon_ctx.config) {
                    Ok(Some(material)) => {
                        if Some(material.fingerprint) != active_tls_fp {
                            let fp = material.fingerprint;
                            match material.try_into_transport() {
                                Ok(transport) => {
                                    info!(
                                        "TLS certificate change detected, reconnecting with new certificates.."
                                    );
                                    staged_transport = Some((transport, fp));
                                    tls_reload_requested = true;
                                    let _ = client.disconnect();
                                    break;
                                }
                                Err(e) => {
                                    warn!(
                                        "TLS certificates changed but are invalid, keeping current connection: {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(
                            "Failed to read TLS certificates during reload check, keeping current connection: {}",
                            e
                        );
                    }
                }
            }

            let invoke = match connection.recv_timeout(Duration::from_millis(250)) {
                Ok(invoke) => invoke,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };

            let event = match invoke {
                Ok(event) => event,
                Err(e) => {
                    error!("mqtt error {:?}", e);
                    if let Some(ref probe) = daemon_ctx.mqtt_probe {
                        probe.record_error(format!("{:?}", e));
                    }
                    break;
                }
            };

            match event {
                Event::Incoming(Incoming::ConnAck(_)) => {
                    if !connected {
                        connected = true;
                        if let Some(fp) = attempt_tls_fp {
                            active_tls_fp = Some(fp);
                        }
                        info!(
                            "Connected to mqtt server {}:{}",
                            mqtt_host.as_str(),
                            mqtt_port
                        );
                    }
                    if let Some(ref probe) = daemon_ctx.mqtt_probe {
                        probe.set_connected();
                    }
                }
                Event::Incoming(Incoming::SubAck(suback)) => {
                    if let Some(ref probe) = daemon_ctx.mqtt_probe {
                        let has_failure = suback
                            .return_codes
                            .iter()
                            .any(|rc| matches!(rc, SubscribeReasonCode::Failure));
                        if has_failure {
                            let err =
                                format!("Subscription rejected: {:?}", suback.return_codes);
                            error!("MQTT {}", err);
                            probe.record_error(err);
                        } else {
                            probe.record_suback_success();
                        }
                    }
                }
                Event::Incoming(Incoming::Publish(message)) => {
                    recon_attempts = 0;

                    let payload = str::from_utf8(&message.payload);
                    if payload.is_err() {
                        error!("Failed to decode mqtt payload {:?}", payload);
                        acknowledge_mqtt_publish(&client, &message);
                        continue;
                    }
                    let payload = payload.expect("payload from utf8");

                    info!("Received mqtt message {}", message.topic);
                    let should_retry = match classify_configured_message(
                        &message.topic,
                        event_topic.as_deref(),
                        heartbeat_topic.as_deref(),
                        policy_topic.as_deref(),
                    ) {
                        MessageType::Heartbeat => process_heartbeat_message(&daemon_ctx, payload),
                        MessageType::Event => {
                            process_event_message(&daemon_ctx, &db, payload, &message.topic)
                        }
                        MessageType::Policy => {
                            process_policy_message(&daemon_ctx, &db, payload, &message.topic)
                        }
                        MessageType::Ignored => false,
                    };

                    if should_retry {
                        if message.qos == QoS::AtMostOnce {
                            warn!(
                                "MQTT message from topic {} failed but cannot be retried with QoS 0",
                                message.topic
                            );
                            continue;
                        }
                        warn!(
                            "MQTT message from topic {} failed, reconnecting without acknowledgement",
                            message.topic
                        );
                        let _ = client.disconnect();
                        break;
                    }

                    acknowledge_mqtt_publish(&client, &message);
                }
                _ => continue,
            }
        }

        if !daemon_ctx.running.load(Ordering::Relaxed) {
            break;
        }

        connected = false;

        if tls_reload_requested {
            recon_attempts = 0;
            continue;
        }

        // exponential backoff, capped at 30 seconds
        let delay_ms = std::cmp::min(100 * 2u64.pow(recon_attempts.min(10)), 30_000);
        recon_attempts += 1;

        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn acknowledge_mqtt_publish(client: &Client, message: &Publish) {
    if let Err(e) = client.ack(message) {
        error!("Failed to acknowledge MQTT message {:?}", e);
    }
}

fn process_heartbeat_message(daemon_ctx: &Arc<DaemonContext>, payload: &str) -> bool {
    let parsed = crate::models::heartbeat::HeartbeatJson::parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        let ok = tokio::runtime::Handle::current().block_on(hbt::ping_heartbeat(
            &daemon_ctx.ilert_client,
            heartbeat.integrationKey.as_str(),
        ));
        if ok {
            info!(
                "Heartbeat {} pinged, triggered by mqtt message",
                heartbeat.integrationKey.as_str()
            );
        }
        return !ok;
    }
    false
}

fn process_policy_message(
    daemon_ctx: &Arc<DaemonContext>,
    db: &ILDatabase,
    payload: &str,
    topic: &str,
) -> bool {
    if daemon_ctx.config.mqtt_buffer {
        match db.create_mqtt_queue_item(topic, payload) {
            Ok(id) => {
                info!("Policy message queued for retry processing: {}", id);
                false
            }
            Err(e) => {
                error!("Failed to queue policy message: {}", e);
                true
            }
        }
    } else {
        tokio::runtime::Handle::current().block_on(super::policy::handle_policy_update(
            &daemon_ctx.ilert_client,
            &daemon_ctx.config,
            payload,
        ))
    }
}

fn process_event_message(
    daemon_ctx: &Arc<DaemonContext>,
    db: &ILDatabase,
    payload: &str,
    topic: &str,
) -> bool {
    if daemon_ctx.config.mqtt_buffer {
        match db.create_mqtt_queue_item(topic, payload) {
            Ok(id) => {
                info!("Event message queued for retry processing: {}", id);
                false
            }
            Err(e) => {
                error!("Failed to queue event message: {}", e);
                true
            }
        }
    } else {
        if let Some(event) = prepare_mqtt_event(&daemon_ctx.config, payload, topic) {
            let event_api_path = build_event_api_path(&event.integrationKey);
            let db_event = EventQueueItemJson::to_db(event, Some(event_api_path));
            tokio::runtime::Handle::current().block_on(crate::poll::send_queued_event(
                &daemon_ctx.ilert_client,
                &db_event,
            ))
        } else {
            false
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum EnqueueResult {
    Inserted,
    Filtered,
    DbError,
}

pub fn enqueue_event(
    config: &ILConfig,
    db: &ILDatabase,
    payload: &str,
    topic: &str,
) -> EnqueueResult {
    let event = match prepare_mqtt_event(config, payload, topic) {
        Some(e) => e,
        None => return EnqueueResult::Filtered,
    };
    let event_api_path = build_event_api_path(&event.integrationKey);
    let db_event = EventQueueItemJson::to_db(event, Some(event_api_path));
    match db.create_il_event(&db_event) {
        Ok(Some(val)) => {
            let event_id = val.id.clone().unwrap_or("".to_string());
            info!(
                "Event {} successfully created and added to queue.",
                event_id
            );
            EnqueueResult::Inserted
        }
        Ok(None) => {
            error!("Failed to create event, result is empty");
            EnqueueResult::DbError
        }
        Err(e) => {
            error!("Failed to create event {:?}.", e);
            EnqueueResult::DbError
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ILConfig;

    // --- classify_message ---

    #[test]
    fn classify_exact_heartbeat_topic() {
        assert_eq!(
            classify_message("ilert/heartbeats", "ilert/events", "ilert/heartbeats", None),
            MessageType::Heartbeat
        );
    }

    #[test]
    fn classify_exact_event_topic() {
        assert_eq!(
            classify_message("ilert/events", "ilert/events", "ilert/heartbeats", None),
            MessageType::Event
        );
    }

    #[test]
    fn classify_unmatched_topic_without_wildcard() {
        assert_eq!(
            classify_message("other/topic", "ilert/events", "ilert/heartbeats", None),
            MessageType::Ignored
        );
    }

    #[test]
    fn classify_wildcard_hash_matches_as_event() {
        assert_eq!(
            classify_message("factory/sensors/temp", "#", "ilert/heartbeats", None),
            MessageType::Event
        );
    }

    #[test]
    fn classify_wildcard_plus_matches_as_event() {
        assert_eq!(
            classify_message(
                "ilert/zone1/events",
                "ilert/+/events",
                "ilert/heartbeats",
                None
            ),
            MessageType::Event
        );
    }

    #[test]
    fn classify_heartbeat_takes_priority_over_wildcard() {
        // even if event_topic is '#', heartbeat exact match should win
        assert_eq!(
            classify_message("ilert/heartbeats", "#", "ilert/heartbeats", None),
            MessageType::Heartbeat
        );
    }

    #[test]
    fn classify_wildcard_does_not_match_heartbeat_topic() {
        // heartbeat topic is checked first, so a non-heartbeat message with wildcard event topic is Event
        assert_eq!(
            classify_message(
                "some/random/topic",
                "devices/+/alerts",
                "ilert/heartbeats",
                None
            ),
            MessageType::Event
        );
    }

    #[test]
    fn classify_policy_topic() {
        assert_eq!(
            classify_message(
                "ilert/policies",
                "ilert/events",
                "ilert/heartbeats",
                Some("ilert/policies")
            ),
            MessageType::Policy
        );
    }

    #[test]
    fn classify_heartbeat_takes_priority_over_policy() {
        assert_eq!(
            classify_message(
                "ilert/heartbeats",
                "ilert/events",
                "ilert/heartbeats",
                Some("ilert/heartbeats")
            ),
            MessageType::Heartbeat
        );
    }

    #[test]
    fn classify_event_when_policy_configured_but_not_matching() {
        assert_eq!(
            classify_message(
                "ilert/events",
                "ilert/events",
                "ilert/heartbeats",
                Some("ilert/policies")
            ),
            MessageType::Event
        );
    }

    #[test]
    fn classify_no_policy_topic_ignores_policy() {
        assert_eq!(
            classify_message("ilert/policies", "ilert/events", "ilert/heartbeats", None),
            MessageType::Ignored
        );
    }

    #[test]
    fn classify_policy_wildcard_hash() {
        assert_eq!(
            classify_message(
                "ilert/policies/abc",
                "ilert/events",
                "ilert/heartbeats",
                Some("ilert/policies/#")
            ),
            MessageType::Policy
        );
    }

    #[test]
    fn classify_policy_wildcard_plus() {
        assert_eq!(
            classify_message(
                "ilert/zone1/policies",
                "ilert/events",
                "ilert/heartbeats",
                Some("ilert/+/policies")
            ),
            MessageType::Policy
        );
    }

    #[test]
    fn classify_policy_wildcard_does_not_match_unrelated_topic() {
        // proper MQTT filter matching: ilert/+/policies should NOT match events/foo
        assert_eq!(
            classify_message(
                "events/foo",
                "events/foo",
                "ilert/heartbeats",
                Some("ilert/+/policies")
            ),
            MessageType::Event
        );
    }

    #[test]
    fn classify_policy_hash_matches_parent_level() {
        // per MQTT spec, "ilert/#" matches "ilert"
        assert_eq!(
            classify_message("ilert", "ilert/events", "ilert/heartbeats", Some("ilert/#")),
            MessageType::Policy
        );
    }

    #[test]
    fn classify_policy_plus_requires_exact_level_count() {
        // "ilert/+" should NOT match "ilert/a/b" (+ is single-level)
        assert_eq!(
            classify_message(
                "ilert/a/b",
                "ilert/a/b",
                "ilert/heartbeats",
                Some("ilert/+")
            ),
            MessageType::Event
        );
    }

    #[test]
    fn classify_heartbeat_takes_priority_over_policy_wildcard() {
        assert_eq!(
            classify_message(
                "ilert/heartbeats",
                "ilert/events",
                "ilert/heartbeats",
                Some("#")
            ),
            MessageType::Heartbeat
        );
    }

    // --- prepare_mqtt_event ---

    #[test]
    fn prepare_event_adds_topic_to_custom_details() {
        let config = ILConfig::new();
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test"}"#;
        let event = prepare_mqtt_event(&config, payload, "factory/sensors").unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["topic"], "factory/sensors");
    }

    #[test]
    fn prepare_event_preserves_existing_custom_details() {
        let config = ILConfig::new();
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test", "customDetails": {"env": "prod"}}"#;
        let event = prepare_mqtt_event(&config, payload, "ilert/events").unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["env"], "prod");
        // topic should NOT be injected when customDetails already exists
        assert!(cd.get("topic").is_none());
    }

    #[test]
    fn prepare_event_returns_none_for_invalid_json() {
        let config = ILConfig::new();
        let result = prepare_mqtt_event(&config, "not json", "ilert/events");
        assert!(result.is_none());
    }

    #[test]
    fn prepare_event_applies_config_mappings() {
        let mut config = ILConfig::new();
        config.event_key = Some("overwritten-key".to_string());
        config.map_key_summary = Some("msg".to_string());
        let payload = r#"{"msg": "Disk full"}"#;
        let event = prepare_mqtt_event(&config, payload, "monitoring/disk").unwrap();
        assert_eq!(event.integrationKey, "overwritten-key");
        assert_eq!(event.summary, "Disk full");
    }

    #[test]
    fn prepare_event_filtered_returns_none() {
        let mut config = ILConfig::new();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload =
            r#"{"apiKey": "k1", "type": "INFO", "eventType": "ALERT", "summary": "test"}"#;
        let result = prepare_mqtt_event(&config, payload, "ilert/events");
        assert!(result.is_none());
    }

    // --- build_event_api_path ---

    #[test]
    fn event_api_path_empty_key() {
        assert_eq!(build_event_api_path(""), "/v1/events/mqtt/");
    }

    // --- end-to-end: prepare + to_db ---

    #[test]
    fn prepare_and_convert_to_db() {
        let mut config = ILConfig::new();
        config.event_key = Some("static-key".to_string());
        config.map_key_etype = Some("state".to_string());
        config.map_val_etype_alert = Some("SET".to_string());
        config.map_key_summary = Some("comment".to_string());
        config.map_key_alert_key = Some("mCode".to_string());

        let payload = r#"{"state": "SET", "mCode": "M-100", "comment": "Pump failure"}"#;
        let event = prepare_mqtt_event(&config, payload, "factory/alarms").unwrap();

        assert_eq!(event.integrationKey, "static-key");
        assert_eq!(event.eventType, "ALERT");
        assert_eq!(event.summary, "Pump failure");
        assert_eq!(event.alertKey.as_ref().unwrap(), "M-100");

        let api_path = build_event_api_path(&event.integrationKey);
        let db_item = EventQueueItemJson::to_db(event, Some(api_path));
        assert_eq!(db_item.integration_key, "static-key");
        assert_eq!(
            db_item.event_api_path.unwrap(),
            "/v1/events/mqtt/static-key"
        );
        assert_eq!(db_item.event_type, "ALERT");

        // custom details should have topic injected
        let cd: serde_json::Value = serde_json::from_str(&db_item.custom_details.unwrap()).unwrap();
        assert_eq!(cd["topic"], "factory/alarms");
    }

    // --- TlsMaterial ---

    #[test]
    fn tls_no_ca_path_returns_none() {
        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        assert!(TlsMaterial::try_load(&config).unwrap().is_none());
    }

    #[test]
    fn tls_missing_ca_file_returns_error() {
        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some("/nonexistent/ca.pem".to_string());
        assert!(TlsMaterial::try_load(&config).is_err());
    }

    #[test]
    fn tls_missing_client_cert_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, b"ca-cert").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());
        config.mqtt_client_cert_path = Some("/nonexistent/client.pem".to_string());
        config.mqtt_client_key_path = Some("/nonexistent/client.key".to_string());
        assert!(TlsMaterial::try_load(&config).is_err());
    }

    #[test]
    fn tls_same_bytes_produce_same_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, b"ca-cert").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());

        let m1 = TlsMaterial::try_load(&config).unwrap().unwrap();
        let m2 = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert_eq!(m1.fingerprint, m2.fingerprint);
    }

    #[test]
    fn tls_fingerprint_changes_when_ca_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());

        std::fs::write(&ca_path, b"ca-cert-v1").unwrap();
        let m1 = TlsMaterial::try_load(&config).unwrap().unwrap();

        std::fs::write(&ca_path, b"ca-cert-v2").unwrap();
        let m2 = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert_ne!(m1.fingerprint, m2.fingerprint);
    }

    #[test]
    fn tls_fingerprint_changes_when_client_cert_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        let cert_path = dir.path().join("client.pem");
        let key_path = dir.path().join("client.key");
        std::fs::write(&ca_path, b"ca-cert").unwrap();
        std::fs::write(&cert_path, b"client-cert-v1").unwrap();
        std::fs::write(&key_path, b"client-key").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());
        config.mqtt_client_cert_path = Some(cert_path.to_str().unwrap().to_string());
        config.mqtt_client_key_path = Some(key_path.to_str().unwrap().to_string());

        let m1 = TlsMaterial::try_load(&config).unwrap().unwrap();

        std::fs::write(&cert_path, b"client-cert-v2").unwrap();
        let m2 = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert_ne!(m1.fingerprint, m2.fingerprint);
    }

    #[test]
    fn tls_fingerprint_changes_when_client_key_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        let cert_path = dir.path().join("client.pem");
        let key_path = dir.path().join("client.key");
        std::fs::write(&ca_path, b"ca-cert").unwrap();
        std::fs::write(&cert_path, b"client-cert").unwrap();
        std::fs::write(&key_path, b"client-key-v1").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());
        config.mqtt_client_cert_path = Some(cert_path.to_str().unwrap().to_string());
        config.mqtt_client_key_path = Some(key_path.to_str().unwrap().to_string());

        let m1 = TlsMaterial::try_load(&config).unwrap().unwrap();

        std::fs::write(&key_path, b"client-key-v2").unwrap();
        let m2 = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert_ne!(m1.fingerprint, m2.fingerprint);
    }

    #[test]
    fn tls_load_failure_preserves_previous_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, b"ca-cert").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());

        let original = TlsMaterial::try_load(&config).unwrap().unwrap();
        let original_fp = original.fingerprint;

        std::fs::remove_file(&ca_path).unwrap();
        assert!(TlsMaterial::try_load(&config).is_err());

        std::fs::write(&ca_path, b"ca-cert").unwrap();
        let restored = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert_eq!(original_fp, restored.fingerprint);
    }

    // --- TlsMaterial::try_into_transport validation ---

    #[test]
    fn tls_invalid_ca_pem_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, b"not valid pem data").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());

        let material = TlsMaterial::try_load(&config).unwrap().unwrap();
        match material.try_into_transport() {
            Err(e) => assert!(
                e.contains("no certificates"),
                "expected 'no certificates' error, got: {}",
                e
            ),
            Ok(_) => panic!("expected error for invalid CA PEM"),
        }
    }

    #[test]
    fn tls_invalid_client_cert_pem_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        let cert_path = dir.path().join("client.pem");
        let key_path = dir.path().join("client.key");

        std::fs::write(&ca_path, include_str!("../../tests/fixtures/ca.pem")).unwrap();
        std::fs::write(&cert_path, b"not a certificate").unwrap();
        std::fs::write(&key_path, b"not a key").unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());
        config.mqtt_client_cert_path = Some(cert_path.to_str().unwrap().to_string());
        config.mqtt_client_key_path = Some(key_path.to_str().unwrap().to_string());

        let material = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert!(material.try_into_transport().is_err());
    }

    #[test]
    fn tls_valid_ca_pem_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, include_str!("../../tests/fixtures/ca.pem")).unwrap();

        let mut config = ILConfig::new();
        config.mqtt_tls = true;
        config.mqtt_ca_path = Some(ca_path.to_str().unwrap().to_string());

        let material = TlsMaterial::try_load(&config).unwrap().unwrap();
        assert!(material.try_into_transport().is_ok());
    }
}

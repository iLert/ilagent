use log::{info, error, warn};
use std::time::Duration;
use rumqttc::{MqttOptions, Client, QoS, Incoming, Event, Transport, TlsConfiguration};
use std::{str, thread};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::db::ILDatabase;
use crate::config::ILConfig;
use crate::{hbt, DaemonContext};
use crate::models::event::EventQueueItemJson;
use serde_json::json;

#[derive(Debug, PartialEq)]
pub enum MessageType {
    Heartbeat,
    Event,
    Ignored,
}

pub fn classify_message(message_topic: &str, event_topic: &str, heartbeat_topic: &str) -> MessageType {
    if message_topic == heartbeat_topic {
        MessageType::Heartbeat
    } else if message_topic == event_topic {
        MessageType::Event
    } else if event_topic.contains('#') || event_topic.contains('+') {
        MessageType::Event
    } else {
        MessageType::Ignored
    }
}

pub fn prepare_mqtt_event(config: &ILConfig, payload: &str, topic: &str) -> Option<EventQueueItemJson> {
    super::prepare_consumer_event(config, payload, topic, json!({"topic": topic}))
}

pub fn build_event_api_path(api_key: &str) -> String {
    super::build_event_api_path("mqtt", api_key)
}

fn build_tls_transport(config: &ILConfig) -> Transport {
    let ca = match &config.mqtt_ca_path {
        Some(path) => std::fs::read(path).expect("Failed to read MQTT CA certificate file"),
        None => {
            warn!("No CA certificate provided, using system default certificates");
            return Transport::tls_with_default_config();
        }
    };

    let client_auth = match (&config.mqtt_client_cert_path, &config.mqtt_client_key_path) {
        (Some(cert_path), Some(key_path)) => {
            let cert = std::fs::read(cert_path).expect("Failed to read MQTT client certificate file");
            let key = std::fs::read(key_path).expect("Failed to read MQTT client key file");
            Some((cert, key))
        },
        _ => None
    };

    let tls_config = TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth,
    };
    Transport::tls_with_config(tls_config)
}

pub fn run_mqtt_job(daemon_ctx: Arc<DaemonContext>) -> () {

    let mut connected = false;
    let mut recon_attempts: u32 = 0;

    let db = ILDatabase::new(daemon_ctx.config.db_file.as_str());

    let mqtt_host = daemon_ctx.config.mqtt_host.clone().expect("Missing mqtt host");
    let mqtt_port = daemon_ctx.config.mqtt_port.clone().expect("Missing mqtt port");
    let mut mqtt_options = MqttOptions::new(
        daemon_ctx.config.mqtt_name.clone().expect("Missing mqtt name"),
        mqtt_host.as_str(),
        mqtt_port,
    );

    mqtt_options
        .set_keep_alive(Duration::from_secs(5))
        .set_pending_throttle(Duration::from_secs(1))
        .set_clean_session(false);

    if let Some(mqtt_username) = daemon_ctx.config.mqtt_username.clone() {
        mqtt_options.set_credentials(mqtt_username.as_str(),
                                     daemon_ctx.config.mqtt_password.clone()
                                         .expect("mqtt_username is set, expecting mqtt_password to be set as well").as_str());
    }

    if daemon_ctx.config.mqtt_tls {
        let transport = build_tls_transport(&daemon_ctx.config);
        mqtt_options.set_transport(transport);
        info!("MQTT TLS enabled");
    }

    let (client, mut connection) = Client::new(mqtt_options, 10);

    let event_topic = daemon_ctx.config.event_topic.clone().expect("Missing mqtt event topic");
    let heartbeat_topic = daemon_ctx.config.heartbeat_topic.clone().expect("Missing mqtt heartbeat topic");

    client.subscribe(event_topic.as_str(), QoS::AtMostOnce)
        .expect("Failed to subscribe to mqtt event topic");

    client.subscribe(heartbeat_topic.as_str(), QoS::AtMostOnce)
        .expect("Failed to subscribe to mqtt heartbeat topic");

    info!("Subscribing to mqtt topics {} and {}", event_topic.as_str(), heartbeat_topic.as_str());

    loop {

        info!("Connecting to Mqtt server..");
        for (_i, invoke) in connection.iter().enumerate() {

            if !daemon_ctx.running.load(Ordering::Relaxed) {
                break;
            }

            match invoke {
                Err(e) => {
                    error!("mqtt error {:?}", e);
                    connected = false;
                    // this will likely kill the mqtt stream, parent loop will reconnect
                    continue;
                },
                _ => ()
            };

            if !connected {
                connected = true;
                recon_attempts = 0;
                info!("Connected to mqtt server {}:{}", mqtt_host.as_str(), mqtt_port);
            }

            let event: Event = invoke.unwrap();
            match event {
                Event::Incoming(Incoming::Publish(message)) => {

                    let payload = str::from_utf8(&message.payload);
                    if payload.is_err() {
                        error!("Failed to decode mqtt payload {:?}", payload);
                        continue;
                    }
                    let payload = payload.expect("payload from utf8");

                    info!("Received mqtt message {}", message.topic);
                    match classify_message(&message.topic, &event_topic, &heartbeat_topic) {
                        MessageType::Heartbeat => handle_heartbeat_message(&daemon_ctx, payload),
                        MessageType::Event => handle_event_message(&daemon_ctx.config, &db, payload, &message.topic),
                        MessageType::Ignored => (),
                    }
                },
                _ => continue
            }
        }

        // faster exits
        if !daemon_ctx.running.load(Ordering::Relaxed) {
            break;
        }

        // exponential backoff, capped at 30 seconds
        let delay_ms = std::cmp::min(100 * 2u64.pow(recon_attempts.min(10)), 30_000);
        recon_attempts += 1;

        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn handle_heartbeat_message(daemon_ctx: &Arc<DaemonContext>, payload: &str) {
    let parsed = crate::models::heartbeat::HeartbeatJson::parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        let ctx = daemon_ctx.clone();
        let _ = tokio::spawn(async move {
            if hbt::ping_heartbeat(&ctx.ilert_client, heartbeat.apiKey.as_str()).await {
                info!("Heartbeat {} pinged, triggered by mqtt message", heartbeat.apiKey.as_str());
            }
        });
    }
}

fn handle_event_message(config: &ILConfig, db: &ILDatabase, payload: &str, topic: &str) -> () {
    if let Some(event) = prepare_mqtt_event(config, payload, topic) {
        let event_api_path = build_event_api_path(&event.apiKey);
        let db_event = EventQueueItemJson::to_db(event, Some(event_api_path));
        let insert_result = db.create_il_event(&db_event);
        match insert_result {
            Ok(res) => match res {
                Some(val) => {
                    let event_id = val.id.clone().unwrap_or("".to_string());
                    info!("Event {} successfully created and added to queue.", event_id);
                },
                None => {
                    error!("Failed to create event, result is empty");
                }
            },
            Err(e) => {
                error!("Failed to create event {:?}.", e);
            }
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
            classify_message("ilert/heartbeats", "ilert/events", "ilert/heartbeats"),
            MessageType::Heartbeat
        );
    }

    #[test]
    fn classify_exact_event_topic() {
        assert_eq!(
            classify_message("ilert/events", "ilert/events", "ilert/heartbeats"),
            MessageType::Event
        );
    }

    #[test]
    fn classify_unmatched_topic_without_wildcard() {
        assert_eq!(
            classify_message("other/topic", "ilert/events", "ilert/heartbeats"),
            MessageType::Ignored
        );
    }

    #[test]
    fn classify_wildcard_hash_matches_as_event() {
        assert_eq!(
            classify_message("factory/sensors/temp", "#", "ilert/heartbeats"),
            MessageType::Event
        );
    }

    #[test]
    fn classify_wildcard_plus_matches_as_event() {
        assert_eq!(
            classify_message("ilert/zone1/events", "ilert/+/events", "ilert/heartbeats"),
            MessageType::Event
        );
    }

    #[test]
    fn classify_heartbeat_takes_priority_over_wildcard() {
        // even if event_topic is '#', heartbeat exact match should win
        assert_eq!(
            classify_message("ilert/heartbeats", "#", "ilert/heartbeats"),
            MessageType::Heartbeat
        );
    }

    #[test]
    fn classify_wildcard_does_not_match_heartbeat_topic() {
        // heartbeat topic is checked first, so a non-heartbeat message with wildcard event topic is Event
        assert_eq!(
            classify_message("some/random/topic", "devices/+/alerts", "ilert/heartbeats"),
            MessageType::Event
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
        assert_eq!(event.apiKey, "overwritten-key");
        assert_eq!(event.summary, "Disk full");
    }

    #[test]
    fn prepare_event_filtered_returns_none() {
        let mut config = ILConfig::new();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload = r#"{"apiKey": "k1", "type": "INFO", "eventType": "ALERT", "summary": "test"}"#;
        let result = prepare_mqtt_event(&config, payload, "ilert/events");
        assert!(result.is_none());
    }

    // --- build_event_api_path ---

    #[test]
    fn event_api_path_format() {
        assert_eq!(build_event_api_path("il1api123"), "/v1/events/mqtt/il1api123");
    }

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

        assert_eq!(event.apiKey, "static-key");
        assert_eq!(event.eventType, "ALERT");
        assert_eq!(event.summary, "Pump failure");
        assert_eq!(event.alertKey.as_ref().unwrap(), "M-100");

        let api_path = build_event_api_path(&event.apiKey);
        let db_item = EventQueueItemJson::to_db(event, Some(api_path));
        assert_eq!(db_item.api_key, "static-key");
        assert_eq!(db_item.event_api_path.unwrap(), "/v1/events/mqtt/static-key");
        assert_eq!(db_item.event_type, "ALERT");

        // custom details should have topic injected
        let cd: serde_json::Value = serde_json::from_str(&db_item.custom_details.unwrap()).unwrap();
        assert_eq!(cd["topic"], "factory/alarms");
    }
}

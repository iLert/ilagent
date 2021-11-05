use std::thread;
use std::thread::JoinHandle;
use log::{debug, info, warn, error};
use std::time::{Duration};
use std::sync::atomic::{AtomicBool, Ordering};
use serde_derive::{Deserialize, Serialize};
use std::sync::Arc;
use rumqttc::{MqttOptions, Client, QoS, Incoming};
use std::str;

use ilert::ilert::ILert;
use ilert::ilert_builders::ILertEventType;

use crate::il_db::ILDatabase;
use crate::il_config::ILConfig;
use crate::il_server::{EventQueueItemJson, EventQueueTransitionItemJson};
use crate::il_hbt;

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeartbeatJson {
    pub apiKey: String
}

pub fn run_mqtt_job(config: &ILConfig, are_we_running: &Arc<AtomicBool>) -> JoinHandle<()> {

    let config = config.clone();
    let are_we_running = are_we_running.clone();
    let mqtt_thread = thread::spawn(move || {

        let mut connected = false;
        let mut recon_attempts = 0;
        let ilert_client = ILert::new().expect("Failed to create iLert client");
        let db = ILDatabase::new(config.db_file.as_str());

        let mqtt_host = config.mqtt_host.clone().expect("Missing mqtt host");
        let mqtt_port = config.mqtt_port.clone().expect("Missing mqtt port");
        let mut mqtt_options = MqttOptions::new(
            config.mqtt_name.clone().expect("Missing mqtt name"),
            mqtt_host.as_str(),
            mqtt_port,
        );

        mqtt_options
            .set_keep_alive(5)
            .set_throttle(Duration::from_secs(1))
            .set_clean_session(false);

        if let Some(mqtt_username) = config.mqtt_username.clone() {
            mqtt_options.set_credentials(mqtt_username.as_str(),
                             config.mqtt_password.clone()
                                 .expect("mqtt_username is set, expecting mqtt_password to be set as well").as_str());
        }

        let (mut client, mut connection) = Client::new(mqtt_options, 10);

        let event_topic = config.mqtt_event_topic.clone().expect("Missing mqtt event topic");
        let heartbeat_topic = config.mqtt_heartbeat_topic.clone().expect("Missing mqtt heartbeat topic");

        client.subscribe(event_topic.as_str(), QoS::AtMostOnce)
            .expect("Failed to subscribe to mqtt event topic");

        client.subscribe(heartbeat_topic.as_str(), QoS::AtMostOnce)
            .expect("Failed to subscribe to mqtt heartbeat topic");

        info!("Subscribing to mqtt topics {} and {}", event_topic.as_str(), heartbeat_topic.as_str());

        loop {

            info!("Connecting to Mqtt server..");
            for (_i, invoke) in connection.iter().enumerate() {

                // will end thread
                if !are_we_running.load(Ordering::SeqCst) {
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

                let (inc, _out) = invoke.unwrap();
                if inc.is_none() {
                    continue;
                }

                match inc.unwrap() {
                    Incoming::Publish(message) => {

                        let payload = str::from_utf8(&message.payload);
                        if payload.is_err() {
                            error!("Failed to decode mqtt payload {:?}", payload);
                            continue;
                        }
                        let payload = payload.unwrap();

                        info!("Received mqtt message {}", message.topic);
                        if heartbeat_topic == message.topic {
                            handle_heartbeat_message(&ilert_client, payload);
                        } else if event_topic == message.topic {
                            handle_event_message(&config, &db, payload, message.topic.as_str());
                        } else {

                            // with filters event processing might subscribe to wildcards
                            if event_topic.contains("#") || event_topic.contains("+") {
                                handle_event_message(&config, &db, payload, message.topic.as_str());
                            }
                        }
                    },
                    _ => continue
                }
            }

            // instant quit
            if !are_we_running.load(Ordering::SeqCst) {
                break;
            }

            // fallback, in case mqtt connection drops all the time
            if recon_attempts < 300 {
                recon_attempts = recon_attempts + 1;
            }

            thread::sleep(Duration::from_millis(100 * recon_attempts));
        }
    });

    mqtt_thread
}

fn parse_heartbeat_json(payload: &str) -> Option<HeartbeatJson> {
    let parsed = serde_json::from_str(payload);
    match parsed {
        Ok(v) => Some(v),
        Err(e) => {
            error!("Failed to parse heartbeat mqtt payload {}", e);
            None
        }
    }
}

fn handle_heartbeat_message(ilert_client: &ILert, payload: &str) -> () {
    let parsed = parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        if il_hbt::ping_heartbeat(ilert_client, heartbeat.apiKey.as_str()) {
            info!("Heartbeat {} pinged, triggered by mqtt message", heartbeat.apiKey.as_str());
        }
    }
}

fn parse_event_json(config: &ILConfig, payload: &str, topic: &str) -> Option<EventQueueItemJson> {

    // raw parse
    let json : serde_json::Result<serde_json::Value> = serde_json::from_str(payload);
    if json.is_err() {
        warn!("Invalid mqtt event payload json {}", json.unwrap_err());
        return None;
    }
    let json = json.unwrap();

    // helper container with default fields (all optional)
    let parsed: serde_json::Result<EventQueueTransitionItemJson> = serde_json::from_str(payload);
    if parsed.is_err() {
        error!("Failed to parse event mqtt payload {}", parsed.unwrap_err());
        return None;
    }
    let mut parsed = parsed.unwrap();

    let config = config.clone();

    // event filter check
    if let Some(mqtt_filter_key) = config.mqtt_filter_key {
        let val_opt = json.get(mqtt_filter_key);

        if val_opt.is_none() {
            debug!("Dropping event because filter key is missing");
            return None;
        }

        if let Some(mqtt_filter_val) = config.mqtt_filter_val {
            let val = val_opt.expect("failed to unwrap event filter val opt");
            if let Some(val) = val.as_str() {
                if !mqtt_filter_val.eq(val) {
                    debug!("Dropping event because filter key value is not matching: {:?}", val);
                    return None;
                }
            }
        }
    }

    // overwrite api key

    if let Some(mqtt_event_key) = config.mqtt_event_key {
        parsed.apiKey = Some(mqtt_event_key.clone());
    }

    // mappings

    if let Some(mqtt_map_key_alert_key) = config.mqtt_map_key_alert_key {
        let val_opt = json.get(mqtt_map_key_alert_key);
        if let Some(val) = val_opt {
            if let Some(val) = val.as_str() {
                parsed.alertKey = Some(val.to_string());
            }
        }
    }

    if let Some(mqtt_map_key_summary) = config.mqtt_map_key_summary {
        let val_opt = json.get(mqtt_map_key_summary);
        if let Some(val) = val_opt {
            if let Some(val) = val.as_str() {
                parsed.summary = Some(val.to_string());
            }
        }
    }

    let mut event_type = "".to_string();
    if let Some(mqtt_map_key_etype) = config.mqtt_map_key_etype {
        let val_opt = json.get(mqtt_map_key_etype);
        if let Some(val) = val_opt {
            if let Some(val) = val.as_str() {
                event_type = val.to_string();
                parsed.eventType = Some(event_type.clone());
            }
        }
    }

    if let Some(mqtt_map_val_etype_alert) = config.mqtt_map_val_etype_alert {
        if mqtt_map_val_etype_alert.eq(event_type.as_str()) {
            parsed.eventType = Some(ILertEventType::ALERT.as_str().to_string());
        }
    }

    if let Some(mqtt_map_val_etype_accept) = config.mqtt_map_val_etype_accept {
        if mqtt_map_val_etype_accept.eq(event_type.as_str()) {
            parsed.eventType = Some(ILertEventType::ACCEPT.as_str().to_string());
        }
    }

    if let Some(mqtt_map_val_etype_resolve) = config.mqtt_map_val_etype_resolve {
        if mqtt_map_val_etype_resolve.eq(event_type.as_str()) {
            parsed.eventType = Some(ILertEventType::RESOLVE.as_str().to_string());
        }
    }

    // try to save empty summary on alert events
    if parsed.summary.is_none()
        && parsed.eventType.clone().unwrap_or(ILertEventType::ALERT.as_str().to_string())
            .eq(ILertEventType::ALERT.as_str()) {
        parsed.summary = Some(format!("New alert from {}", topic).to_string());
    }

    debug!("Mapped event transition object: {:?}", parsed);
    Some(EventQueueItemJson::from_transition(parsed))
}

fn handle_event_message(config: &ILConfig, db: &ILDatabase, payload: &str, topic: &str) -> () {
    let parsed = parse_event_json(&config, payload, topic);
    if let Some(event) = parsed {
        let db_event = EventQueueItemJson::to_db(event);
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
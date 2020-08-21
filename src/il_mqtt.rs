use std::thread;
use std::thread::JoinHandle;
use log::{info, error};
use std::time::{Duration};
use std::sync::atomic::{AtomicBool, Ordering};
use serde_derive::{Deserialize, Serialize};
use std::sync::Arc;
use rumqttc::{MqttOptions, Client, QoS, Incoming};
use std::str;

use ilert::ilert::ILert;

use crate::il_db::ILDatabase;
use crate::il_config::ILConfig;
use crate::il_server::EventQueueItemJson;
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

        let mqtt_host = config.mqtt_host.expect("Missing mqtt host");
        let mqtt_port = config.mqtt_port.expect("Missing mqtt port");
        let mut mqtt_options = MqttOptions::new(
            config.mqtt_name.expect("Missing mqtt name"),
            mqtt_host.as_str(),
            mqtt_port,
        );

        mqtt_options
            .set_keep_alive(5)
            .set_throttle(Duration::from_secs(1))
            .set_clean_session(false);

        let (mut client, mut connection) = Client::new(mqtt_options, 10);

        let event_topic = config.mqtt_event_topic.expect("Missing mqtt event topic");
        let heartbeat_topic = config.mqtt_heartbeat_topic.expect("Missing mqtt heartbeat topic");

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
                        error!("mqtt error {}", e);
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
                            handle_event_message(&db, payload);
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

fn parse_event_json(payload: &str) -> Option<EventQueueItemJson> {
    let parsed = serde_json::from_str(payload);
    match parsed {
        Ok(v) => Some(v),
        Err(e) => {
            error!("Failed to parse event mqtt payload {}", e);
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

fn handle_event_message(db: &ILDatabase, payload: &str) -> () {
    let parsed = parse_event_json(payload);
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
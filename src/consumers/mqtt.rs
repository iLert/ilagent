use log::{info, error};
use std::time::{Duration};
use rumqttc::{MqttOptions, Client, QoS, Incoming, Event};
use std::{str, thread};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use ilert::ilert::ILert;

use crate::db::ILDatabase;
use crate::config::ILConfig;
use crate::{hbt, DaemonContext};
use crate::models::event::EventQueueItemJson;

pub fn run_mqtt_job(daemon_ctx: Arc<DaemonContext>) -> () {

    let mut connected = false;
    let mut recon_attempts = 0;

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
                    if heartbeat_topic == message.topic {
                        handle_heartbeat_message(payload);
                    } else if event_topic == message.topic {
                        handle_event_message(&daemon_ctx.config, &db, payload, message.topic.as_str());
                    } else {

                        // with filters event processing might subscribe to wildcards
                        if event_topic.contains("#") || event_topic.contains("+") {
                            handle_event_message(&daemon_ctx.config, &db, payload, message.topic.as_str());
                        }
                    }
                },
                _ => continue
            }
        }

        // faster exits
        if !daemon_ctx.running.load(Ordering::Relaxed) {
            break;
        }

        // fallback, in case mqtt connection drops all the time
        if recon_attempts < 300 {
            recon_attempts = recon_attempts + 1;
        }

        thread::sleep(Duration::from_millis(100 * recon_attempts));
    }
}

fn handle_heartbeat_message(payload: &str) -> () {
    let parsed = crate::models::heartbeat::HeartbeatJson::parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        let _ = tokio::spawn(async move {
            // this is a hack to allow the .await call in this context, it is not really smart to create a new client with each call
            let ilert_client = ILert::new().expect("Failed to create ilert client");
            if hbt::ping_heartbeat(&ilert_client, heartbeat.apiKey.as_str()).await {
                info!("Heartbeat {} pinged, triggered by mqtt message", heartbeat.apiKey.as_str());
            }
        });
    }
}

fn handle_event_message(config: &ILConfig, db: &ILDatabase, payload: &str, topic: &str) -> () {
    let parsed = crate::models::event::EventQueueItemJson::parse_event_json(&config, payload, topic);
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
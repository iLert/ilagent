use log::{debug, info, error};
use env_logger::Env;
use std::sync::Arc;
use clap::{Arg, App, ArgMatches};
use std::panic;
use std::process;

use ilert::ilert::ILert;
use ilert::ilert_builders::{EventImage, EventLink};
use tokio::sync::{Mutex};

use config::ILConfig;
use db::ILDatabase;
use crate::models::event_db::EventQueueItem;

mod config;
mod db;
mod models;
mod hbt;
mod consumers;
mod poll;
mod server;
mod cleanup;

pub struct DaemonContext {
    pub config: ILConfig,
    pub db: Mutex<ILDatabase>,
    pub ilert_client: ILert
}

#[tokio::main]
async fn main() -> () {

    let matches = App::new("ilert Agent")

        .version("0.5.0")
        .author("ilert GmbH. <support@ilert.com>")
        .about("ilert Agent 🦀 📦 The swiss army knife.")

        .arg(Arg::with_name("COMMAND")
            .help("The actual command that should be executed.")
            .max_values(1)
            .possible_values(&["daemon", "event", "heartbeat", "cleanup"])
            .required(true)
            .index(1))

        .arg(Arg::with_name("port")
            .short("p")
            .long("port")
            .value_name("PORT")
            .help("Sets a port for the daemon's http server, the server is not started, unless a port is provided")
            .takes_value(true)
            )

        .arg(Arg::with_name("heartbeat")
            .short("b")
            .long("heartbeat")
            .value_name("HEARTBEAT")
            .help("Sets the API key of the heartbeat (pings every minute), daemon mode only")
             .takes_value(true)
             )

        .arg(Arg::with_name("api_key")
            .short("k")
            .long("api_key")
            .value_name("APIKEY")
            .help("Sets the API key for the commands 'event' and 'heartbeat'")
             .takes_value(true)
             )

        .arg(Arg::with_name("summary")
            .short("s")
            .long("summary")
            .value_name("SUMMARY")
            .help("Sets the summary value for the 'event' command e.g. 'my summary'")
            .takes_value(true)
            )

        .arg(Arg::with_name("event_type")
            .short("t")
            .long("event_type")
            .value_name("EVENT_TYPE")
            .help("Sets the event type value for the 'event' command")
            .possible_values(&["ALERT", "ACCEPT", "RESOLVE"])
             .takes_value(true)
            )

        .arg(Arg::with_name("priority")
            .short("o")
            .long("priority")
            .value_name("PRIORITY")
            .help("Sets the event priority for the 'event' command")
            .possible_values(&["LOW", "HIGH"])
            .takes_value(true)
        )

        .arg(Arg::with_name("image")
            .short("g")
            .long("image")
            .value_name("IMAGE")
            .help("Sets the event image url for the 'event' command")
            .takes_value(true)
        )

        .arg(Arg::with_name("details")
            .short("d")
            .long("details")
            .value_name("DETAIL")
            .help("Sets the event detail for the 'event' command")
            .takes_value(true)
        )

        .arg(Arg::with_name("link")
            .short("l")
            .long("link")
            .value_name("LINK")
            .help("Sets the event link for the 'event' command")
            .takes_value(true)
        )

        .arg(Arg::with_name("file")
            .short("f")
            .long("file")
            .value_name("FILE")
            .help("Provides the file path for the SQLite database (default: ./ilagent.db3)")
            .takes_value(true)
        )

        .arg(Arg::with_name("alert_key")
            .short("i")
            .long("alert_key")
            .value_name("ALERT_KEY")
            .help("Sets the event alert key for the 'event' command")
            .takes_value(true)
        )

        .arg(Arg::with_name("resource")
            .long("resource")
            .value_name("RESOURCE")
            .help("Sets a resource target for the command, not specifically bound to a single command")
            .takes_value(true)
        )

        /* ### mqtt ### */

        .arg(Arg::with_name("mqtt_host")
            .short("m")
            .long("mqtt_host")
            .value_name("MQTT_HOST")
            .help("If provided under daemon command, sets mqtt server to connect to")
            .takes_value(true)
        )

        .arg(Arg::with_name("mqtt_port")
            .short("q")
            .long("mqtt_port")
            .value_name("MQTT_PORT")
            .help("If provided under daemon command, sets mqtt port (default: '1883')")
            .takes_value(true)
        )

        .arg(Arg::with_name("mqtt_name")
            .short("n")
            .long("mqtt_name")
            .value_name("MQTT_NAME")
            .help("If provided under daemon command, sets mqtt client name (default: 'ilagent')")
            .takes_value(true)
        )

        .arg(Arg::with_name("mqtt_username")
            .long("mqtt_username")
            .value_name("MQTT_USERNAME")
            .help("If provided under daemon command, sets mqtt client credential username (expects mqtt_password to be set as well)")
            .takes_value(true)
        )

        .arg(Arg::with_name("mqtt_password")
            .long("mqtt_password")
            .value_name("MQTT_PASSWORD")
            .help("If provided under daemon command, sets mqtt client credential password (only works with mqtt_username set)")
            .takes_value(true)
        )

        /* ### kafka ### */

        .arg(
            Arg::with_name("kafka_brokers")
                .long("kafka_brokers")
                .help("Broker list in kafka format")
                .takes_value(true)
        )

        .arg(
            Arg::with_name("kafka_group_id")
                .long("kafka_group_id")
                .help("Kafka consumer group id")
                .takes_value(true)
                .default_value("ilagent"),
        )

        /* ### consumer overwrites ### */

        .arg(Arg::with_name("event_topic")
            .short("e")
            .long("event_topic")
            .value_name("EVENT_TOPIC")
            .help("Consumer topic to listen to (default: 'ilert/events')")
            .takes_value(true)
        )

        .arg(Arg::with_name("heartbeat_topic")
            .short("r")
            .long("heartbeat_topic")
            .value_name("HEARTBEAT_TOPIC")
            .help("Consumer topic to listen to (default: 'ilert/heartbeats')")
            .takes_value(true)
        )

        .arg(Arg::with_name("event_key")
            .long("event_key")
            .value_name("event_key")
            .help("If provided under daemon command, overwrites all events with apiKey")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_key_etype")
            .long("map_key_etype")
            .value_name("map_key_etype")
            .help("If provided under daemon command, overwrites JSON payload key. Default is 'eventType'")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_key_alert_key")
            .long("map_key_alert_key")
            .value_name("MAP_KEY_ALERT_KEY")
            .help("If provided under daemon command, overwrites JSON payload key. Default is 'alertKey'")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_key_summary")
            .long("map_key_summary")
            .value_name("map_key_summary")
            .help("If provided under daemon command, overwrites JSON payload key. Default is 'summary'")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_val_etype_alert")
            .long("map_val_etype_alert")
            .value_name("map_val_etype_alert")
            .help("If provided under daemon command, overwrites JSON payload value, of key 'eventType' with origin value 'ALERT'")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_val_etype_accept")
            .long("map_val_etype_accept")
            .value_name("map_val_etype_accept")
            .help("If provided under daemon command, overwrites JSON payload value, of key 'eventType' with origin value 'ACCEPT'")
            .takes_value(true)
        )

        .arg(Arg::with_name("map_val_etype_resolve")
            .long("map_val_etype_resolve")
            .value_name("map_val_etype_resolve")
            .help("If provided under daemon command, overwrites JSON payload value, of key 'eventType' with origin value 'RESOLVE'")
            .takes_value(true)
        )

        .arg(Arg::with_name("filter_key")
            .long("filter_key")
            .value_name("filter_key")
            .help("If provided under daemon command, requires given key in JSON payload")
            .takes_value(true)
        )

        .arg(Arg::with_name("filter_val")
            .long("filter_val")
            .value_name("filter_val")
            .help("If provided under daemon command, along with 'filter_key' requires certain value of JSON payload property")
            .takes_value(true)
        )

        .arg(Arg::with_name("v")
            .short("v")
            .long("verbose")
            .multiple(true)
            .help("Sets the level of verbosity")
        )

        .get_matches();

    let mut config = ILConfig::new();

    let default_port = config.get_port_as_string().clone();
    // http server is only started if the port is provided
    config.start_http = matches.is_present("port");
    let port = matches.value_of("port").unwrap_or(default_port.as_str());
    config.set_port_from_str(port);

    let command = matches.value_of("COMMAND").expect("Failed to parse provided command");

    let log_level = match matches.occurrences_of("v") {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => {
            error!("Maximum log level reached (4)");
            "debug"
        }
    };

    env_logger::Builder::from_env(Env::default()
        .default_filter_or(log_level))
        .init();

    if matches.is_present("heartbeat") {
        let heartbeat_key = matches.value_of("heartbeat").expect("Failed to parse heartbeat api key");
        config.heartbeat_key = Some(heartbeat_key.to_string());
    }

    if matches.is_present("mqtt_host") {

        let mqtt_host = matches.value_of("mqtt_host").unwrap_or("127.0.0.1");
        let mqtt_port_str = matches.value_of("mqtt_port").unwrap_or("1883");
        let mqtt_name = matches.value_of("mqtt_name").unwrap_or("ilagent");

        config.mqtt_host = Some(mqtt_host.to_string());
        config.set_mqtt_port_from_str(mqtt_port_str);
        config.mqtt_name = Some(mqtt_name.to_string());

        let event_topic = matches.value_of("event_topic").unwrap_or("ilert/events");
        let heartbeat_topic = matches.value_of("heartbeat_topic").unwrap_or("ilert/heartbeats");
        config.event_topic = Some(event_topic.to_string());
        config.heartbeat_topic = Some(heartbeat_topic.to_string());

        config = parse_consumer_arguments(&matches, config);
    }

    if matches.is_present("kafka_brokers") {

        let kafka_brokers = matches.value_of("kafka_brokers").unwrap_or("localhost:9092");
        let kafka_group_id = matches.value_of("kafka_group_id").unwrap_or("ilagent");

        config.kafka_brokers = Some(kafka_brokers.to_string());
        config.kafka_group_id = Some(kafka_group_id.to_string());

        if let Some(topic) = matches.value_of("event_topic") {
            config.event_topic = Some(topic.to_string());
        }

        if let Some(topic) = matches.value_of("heartbeat_topic") {
            config.heartbeat_topic = Some(topic.to_string());
        }

        config = parse_consumer_arguments(&matches, config);
    }

    let db_file = matches.value_of("file");
    if let Some(file) = db_file {
        config.db_file = file.to_string();
    }

    debug!("Running command: {}.", command);
    match command {
        "daemon" => run_daemon(&config).await,
        "event" => run_event(matches).await,
        "heartbeat" => run_heartbeat(matches).await,
        "cleanup" => run_cleanup(matches).await,
        _ => panic!("Unsupported command provided.") // unreachable
    }
}

fn parse_consumer_arguments(matches: &ArgMatches, mut config: ILConfig) -> ILConfig {

    if matches.is_present("mqtt_username") {
        config.mqtt_username = Some(matches.value_of("mqtt_username").expect("failed to parse mqtt_username").to_string());

        if matches.is_present("mqtt_password") {
            config.mqtt_password = Some(matches.value_of("mqtt_password").expect("failed to parse mqtt_password").to_string());
            info!("mqtt credentials set");
        }
    }

    if matches.is_present("filter_key") {
        config.filter_key = Some(matches.value_of("filter_key").expect("failed to parse mqtt mapping key: filter_key").to_string());
        info!("Filter key is present: {:?} will be required in event payloads", config.filter_key);
    }

    if matches.is_present("filter_val") {
        config.filter_val = Some(matches.value_of("filter_val").expect("failed to parse mqtt mapping key: filter_val").to_string());
        info!("Filter key value is present, will be required in event payloads");
    }

    // mappings
    if matches.is_present("event_key") {
        config.event_key = Some(matches.value_of("event_key").expect("failed to parse consumer mapping key: event_key").to_string());
        info!("eventKey has been configured, overwriting events with static key");
    }

    if matches.is_present("map_key_etype") {
        config.map_key_etype = Some(matches.value_of("map_key_etype").expect("failed to parse consumer mapping key: map_key_etype").to_string());
        info!("Overwrite for payload key 'eventType' has been configured: '{:?}'", config.map_key_etype);
    }

    if matches.is_present("map_key_alert_key") {
        config.map_key_alert_key = Some(matches.value_of("map_key_alert_key").expect("failed to parse consumer mapping key: map_key_alert_key").to_string());
        info!("Overwrite for payload key 'alertKey' has been configured: '{:?}'", config.map_key_alert_key);
    }

    if matches.is_present("map_key_summary") {
        config.map_key_summary = Some(matches.value_of("map_key_summary").expect("failed to parse consumer mapping key: map_key_summary").to_string());
        info!("Overwrite for payload key 'summary' has been configured: '{:?}'", config.map_key_summary);
    }

    if matches.is_present("map_val_etype_alert") {
        config.map_val_etype_alert = Some(matches.value_of("map_val_etype_alert").expect("failed to parse consumer mapping key: map_val_etype_alert").to_string());
        info!("Overwrite for payload val of key 'eventType' and default: 'ALERT' has been configured: '{:?}'", config.map_val_etype_alert);
    }

    if matches.is_present("map_val_etype_accept") {
        config.map_val_etype_accept = Some(matches.value_of("map_val_etype_accept").expect("failed to parse consumer mapping key: map_val_etype_accept").to_string());
        info!("Overwrite for payload val of key 'eventType' and default: 'ACCEPT' has been configured: '{:?}'", config.map_val_etype_accept);
    }

    if matches.is_present("map_val_etype_resolve") {
        config.map_val_etype_resolve = Some(matches.value_of("map_val_etype_resolve").expect("failed to parse consumer mapping key: map_val_etype_resolve").to_string());
        info!("Overwrite for payload val of key 'eventType' and default: 'RESOLVE' has been configured: '{:?}'", config.map_val_etype_resolve);
    }

    config
}

/**
    If port is provided starts a http server with proxy functionality /api/events and /api/heartbeats
    Where events are queued in a local SQLite table to ensure delivery
    If provided, pings a heartbeat api key regularly
    If provided, connects to MQTT or Kafka broker and proxies events (through queue) and heartbeats
    If http server or mqtt client is started will also spawn a poll thread to poll the db
    Kafka will use the consumer offset to ensure at least once delivery, no db polling needed
*/
async fn run_daemon(config: &ILConfig) -> () {

    // in case a thread (like mqtt or poll) dies of a panic
    // we want to make sure the whole program exits
    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        orig_hook(panic_info);
        process::exit(1);
    }));

    info!("Starting..");

    let ilert_client = ILert::new().expect("Failed to create ilert client");
    let db = ILDatabase::new(config.db_file.as_str());
    info!("Migrating DB..");
    db.prepare_database();

    let daemon_ctx = Arc::new(DaemonContext {
        config: config.clone(),
        db: Mutex::new(db),
        ilert_client
    });

    // poll is only needed if mqtt or web server are running
    let is_poll_needed = config.mqtt_host.is_some() || config.start_http;
    let mut poll_job = None;
    if is_poll_needed {
        info!("Starting poll job..");
        let cloned_ctx = daemon_ctx.clone();
        poll_job = Some(tokio::spawn(async move {
            poll::run_poll_job(cloned_ctx).await;
        }));
    }

    let mut hbt_job = None;
    if config.heartbeat_key.is_some() {
        info!("Running regular heartbeats..");
        let cloned_ctx = daemon_ctx.clone();
        hbt_job = Some(tokio::spawn(async move {
            hbt::run_hbt_job(cloned_ctx).await;
        }));
    }

    let mut mqtt_job = None;
    if config.mqtt_host.is_some() {
        info!("Running MQTT thread..");
        let cloned_ctx = daemon_ctx.clone();
        // rumqttc spawns its own tokio runtime
        mqtt_job = Some(tokio::task::spawn_blocking(move || {
            consumers::mqtt::run_mqtt_job(&cloned_ctx.config);
        }));
    }

    let mut kafka_job = None;
    if config.kafka_brokers.is_some() {
        info!("Running Kafka thread..");
        let cloned_ctx = daemon_ctx.clone();
        kafka_job = Some(tokio::spawn(async move {
            consumers::kafka::run_kafka_job(cloned_ctx).await;
        }));
    }

    if config.start_http {
        info!("Starting server..");
        server::run_server(daemon_ctx.clone());
        // blocking..
    }

    if let Some(handle) = poll_job {
        handle.await.expect("Failed to join poll thread");
    }

    if let Some(handle) = hbt_job {
        handle.await.expect("Failed to join heartbeat thread");
    }

    if let Some(handle) = mqtt_job {
        handle.await.expect("Failed to join mqtt thread");
    }

    if let Some(handle) = kafka_job {
        handle.await.expect("Failed to join kafka thread");
    }

    ()
}

/**
    Attempts to create an event, one time - skips queue
*/
async fn run_event(matches: ArgMatches<'_>) -> () {

    if !matches.is_present("api_key") {
        return error!("Missing api_key arg (-k, --api_key)");
    }

    if !matches.is_present("event_type") {
        return error!("Missing alert_type arg (-t, --event_type)");
    }

    if !matches.is_present("summary") {
        return error!("Missing summary arg (-s, --summary)");
    }

    let ilert_client = ILert::new().expect("Failed to create ilert client");
    let api_key = matches.value_of("api_key").unwrap();
    let event_type = matches.value_of("event_type").unwrap();
    let summary = matches.value_of("summary").unwrap();

    let alert_key = matches.value_of("alert_key");
    let alert_key = match alert_key {
        Some(k) => Some(k.to_string()),
        None => None
    };

    let priority = matches.value_of("priority");
    let priority = match priority {
        Some(k) => Some(k.to_string()),
        None => None
    };

    let details = matches.value_of("details");
    let details = match details {
        Some(k) => Some(k.to_string()),
        None => None
    };

    let mut images = None;
    let image = matches.value_of("image");
    if let Some(val) = image {
        let vec = vec!(EventImage::new(val));
        let j = serde_json::to_string(&vec);
        images = match j {
            Ok(v) => Some(v),
            Err(e) => {
                error!("Failed to parse image {}", e);
                None
            }
        };
    }

    let mut links = None;
    let link = matches.value_of("link");
    if let Some(val) = link {
        let mut e_link = EventLink::new(val);
        e_link.text = Some("Provided Url".to_string());
        let vec = vec!(e_link);
        let j = serde_json::to_string(&vec);
        links = match j {
            Ok(v) => Some(v),
            Err(e) => {
                error!("Failed to parse link {}", e);
                None
            }
        };
    }

    let mut event = EventQueueItem::new_with_required(
        api_key, event_type, summary, alert_key);

    event.id = Some("provided".to_string()); // prettier logs
    event.priority = priority;
    event.details = details;
    event.images = images;
    event.links = links;

    poll::process_queued_event(&ilert_client, &event).await;
}

/**
    Attempts to ping a heartbeat
*/
async fn run_heartbeat(matches: ArgMatches<'_>) -> () {

    if !matches.is_present("api_key") {
        return error!("Missing api_key arg (-k, --api_key)");
    }

    let ilert_client = ILert::new().expect("Failed to create ilert client");
    let api_key = matches.value_of("api_key").unwrap();

    if hbt::ping_heartbeat(&ilert_client, api_key).await {
        info!("Heartbeat ping successful");
    }
}

/**
    Attempts to clean-up resources
*/
async fn run_cleanup(matches: ArgMatches<'_>) -> () {

    if !matches.is_present("api_key") {
        return error!("Missing api_key arg (-k, --api_key)");
    }

    if !matches.is_present("resource") {
        return error!("Missing resource arg (--resource)");
    }

    let api_key = matches.value_of("api_key").unwrap();
    let mut ilert_client = ILert::new().expect("Failed to create ilert client");
    ilert_client.auth_via_token(api_key).expect("Failed to set api key");

    let resource = matches.value_of("resource").unwrap();
    match resource {
        "alerts" => cleanup::cleanup_alerts(&ilert_client).await,
        _ => panic!("Unsupported 'resource' provided.")
    }
}
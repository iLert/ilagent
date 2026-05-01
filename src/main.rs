use clap::{Arg, ArgAction, ArgMatches, Command};
use env_logger::Env;
use ilert::ilert::ILert;
use ilert::ilert_builders::{EventImage, EventLink};
use log::{debug, error, info, warn};
use std::panic;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

use ilagent::config::ILConfig;
use ilagent::db::ILDatabase;
use ilagent::models::event_db::EventQueueItem;
use ilagent::{DaemonContext, cleanup, consumers, hbt, http_server, poll, version_check};

fn strip_bearer_prefix(key: String) -> String {
    if let Some(stripped) = key.strip_prefix("Bearer ") {
        warn!("Stripping unnecessary 'Bearer ' prefix from API key");
        stripped.to_string()
    } else if let Some(stripped) = key.strip_prefix("bearer ") {
        warn!("Stripping unnecessary 'bearer ' prefix from API key");
        stripped.to_string()
    } else {
        key
    }
}

pub fn consumer_args() -> Vec<Arg> {
    vec![
        Arg::new("event_topic")
            .short('e')
            .long("event_topic")
            .value_name("EVENT_TOPIC")
            .help("Consumer topic to listen to for events"),
        Arg::new("heartbeat_topic")
            .short('r')
            .long("heartbeat_topic")
            .value_name("HEARTBEAT_TOPIC")
            .help("Consumer topic to listen to for heartbeats"),
        Arg::new("event_key")
            .long("event_key")
            .value_name("EVENT_KEY")
            .help("Overwrites all events with the given apiKey"),
        Arg::new("map_key_etype")
            .long("map_key_etype")
            .value_name("MAP_KEY_ETYPE")
            .help("Overwrites JSON payload key for eventType"),
        Arg::new("map_key_alert_key")
            .long("map_key_alert_key")
            .value_name("MAP_KEY_ALERT_KEY")
            .help("Overwrites JSON payload key for alertKey"),
        Arg::new("map_key_summary")
            .long("map_key_summary")
            .value_name("MAP_KEY_SUMMARY")
            .help("Overwrites JSON payload key for summary"),
        Arg::new("map_val_etype_alert")
            .long("map_val_etype_alert")
            .value_name("MAP_VAL_ETYPE_ALERT")
            .help("Maps the given value to eventType 'ALERT'"),
        Arg::new("map_val_etype_accept")
            .long("map_val_etype_accept")
            .value_name("MAP_VAL_ETYPE_ACCEPT")
            .help("Maps the given value to eventType 'ACCEPT'"),
        Arg::new("map_val_etype_resolve")
            .long("map_val_etype_resolve")
            .value_name("MAP_VAL_ETYPE_RESOLVE")
            .help("Maps the given value to eventType 'RESOLVE'"),
        Arg::new("filter_key")
            .long("filter_key")
            .value_name("FILTER_KEY")
            .help("Requires the given key in JSON payload"),
        Arg::new("filter_val")
            .long("filter_val")
            .value_name("FILTER_VAL")
            .help("Along with 'filter_key', requires certain value of JSON payload property"),
        Arg::new("forward_message_payload")
            .long("forward_message_payload")
            .action(ArgAction::SetTrue)
            .help("Forward the full original JSON payload as customDetails in events"),
        Arg::new("policy_topic")
            .long("policy_topic")
            .value_name("POLICY_TOPIC")
            .help("Consumer topic to listen to for escalation policy updates"),
        Arg::new("policy_routing_keys")
            .long("policy_routing_keys")
            .value_name("POLICY_ROUTING_KEYS")
            .help("Comma-separated JSON field names to extract routing key values from"),
        Arg::new("map_key_email")
            .long("map_key_email")
            .value_name("MAP_KEY_EMAIL")
            .help("JSON path for email field, dot-notation for nested fields (default: 'data.email')"),
        Arg::new("map_key_shift")
            .long("map_key_shift")
            .value_name("MAP_KEY_SHIFT")
            .help("JSON path for shift field, dot-notation for nested fields (default: 'data.shift')"),
        Arg::new("shift_offset")
            .long("shift_offset")
            .value_name("SHIFT_OFFSET")
            .help("Offset to apply to the shift value (e.g. -1 to convert 1-indexed to 0-indexed, default: 0)"),
        Arg::new("max_retries")
            .long("max_retries")
            .value_name("MAX_RETRIES")
            .help("Maximum number of retries for failed queue items (0 = unlimited, default: 100)"),
    ]
}

pub fn build_cli() -> Command {
    let mut daemon_cmd = Command::new("daemon")
        .about("Run ilagent as a daemon with optional HTTP server, MQTT, and Kafka consumers")
        .arg(Arg::new("port")
            .short('p')
            .long("port")
            .value_name("PORT")
            .help("Sets a port for the HTTP server (server is not started unless a port is provided)"))
        .arg(Arg::new("heartbeat")
            .short('b')
            .long("heartbeat")
            .value_name("HEARTBEAT")
            .help("Sets the API key of the heartbeat (pings regularly)"))
        // mqtt
        .arg(Arg::new("mqtt_host")
            .short('m')
            .long("mqtt_host")
            .value_name("MQTT_HOST")
            .help("Sets the MQTT server to connect to"))
        .arg(Arg::new("mqtt_port")
            .short('q')
            .long("mqtt_port")
            .value_name("MQTT_PORT")
            .help("Sets the MQTT port (default: 1883)"))
        .arg(Arg::new("mqtt_name")
            .short('n')
            .long("mqtt_name")
            .value_name("MQTT_NAME")
            .help("Sets the MQTT client name (default: 'ilagent')"))
        .arg(Arg::new("mqtt_username")
            .long("mqtt_username")
            .value_name("MQTT_USERNAME")
            .help("Sets the MQTT credential username"))
        .arg(Arg::new("mqtt_password")
            .long("mqtt_password")
            .value_name("MQTT_PASSWORD")
            .help("Sets the MQTT credential password (requires mqtt_username)"))
        .arg(Arg::new("mqtt_tls")
            .long("mqtt_tls")
            .action(ArgAction::SetTrue)
            .help("Enables TLS for the MQTT connection"))
        .arg(Arg::new("mqtt_ca")
            .long("mqtt_ca")
            .value_name("MQTT_CA")
            .help("CA certificate file path for MQTT TLS (PEM format)"))
        .arg(Arg::new("mqtt_client_cert")
            .long("mqtt_client_cert")
            .value_name("MQTT_CLIENT_CERT")
            .help("Client certificate file path for MQTT mTLS (PEM format)"))
        .arg(Arg::new("mqtt_client_key")
            .long("mqtt_client_key")
            .value_name("MQTT_CLIENT_KEY")
            .help("Client private key file path for MQTT mTLS (PEM format)"))
        .arg(Arg::new("mqtt_qos")
            .long("mqtt_qos")
            .value_name("MQTT_QOS")
            .help("Sets the MQTT QoS level: 0 (at most once), 1 (at least once), 2 (exactly once) (default: 0)"))
        .arg(Arg::new("mqtt_buffer")
            .long("mqtt_buffer")
            .action(ArgAction::SetTrue)
            .help("Buffer MQTT messages in SQLite for retry instead of processing directly"))
        .arg(Arg::new("mqtt_shared_group")
            .long("mqtt_shared_group")
            .value_name("MQTT_SHARED_GROUP")
            .help("MQTT v5 shared subscription group name (topics are prefixed with $share/<group>/ for load balancing)"))
        // kafka
        .arg(Arg::new("kafka_brokers")
            .long("kafka_brokers")
            .help("Broker list in kafka format"))
        .arg(Arg::new("kafka_group_id")
            .long("kafka_group_id")
            .help("Kafka consumer group id")
            .default_value("ilagent"));

    for arg in consumer_args() {
        daemon_cmd = daemon_cmd.arg(arg);
    }

    let event_cmd = Command::new("event")
        .about("Send a single event to ilert")
        .arg(
            Arg::new("integration_key")
                .short('k')
                .long("integration_key")
                .value_name("INTEGRATION_KEY")
                .help("Sets the integration key (falls back to ILERT_INTEGRATION_KEY env var)"),
        )
        .arg(
            Arg::new("event_type")
                .short('t')
                .long("event_type")
                .value_name("EVENT_TYPE")
                .required(true)
                .value_parser(["ALERT", "ACCEPT", "RESOLVE"])
                .help("Sets the event type"),
        )
        .arg(
            Arg::new("summary")
                .short('s')
                .long("summary")
                .value_name("SUMMARY")
                .required(true)
                .help("Sets the summary"),
        )
        .arg(
            Arg::new("alert_key")
                .short('i')
                .long("alert_key")
                .value_name("ALERT_KEY")
                .help("Sets the event alert key"),
        )
        .arg(
            Arg::new("priority")
                .short('o')
                .long("priority")
                .value_name("PRIORITY")
                .value_parser(["LOW", "HIGH"])
                .help("Sets the event priority"),
        )
        .arg(
            Arg::new("details")
                .short('d')
                .long("details")
                .value_name("DETAIL")
                .help("Sets the event detail"),
        )
        .arg(
            Arg::new("image")
                .short('g')
                .long("image")
                .value_name("IMAGE")
                .help("Sets the event image url"),
        )
        .arg(
            Arg::new("link")
                .short('l')
                .long("link")
                .value_name("LINK")
                .help("Sets the event link url"),
        );

    let heartbeat_cmd = Command::new("heartbeat")
        .about("Send a single heartbeat ping to ilert")
        .arg(
            Arg::new("integration_key")
                .short('k')
                .long("integration_key")
                .value_name("INTEGRATION_KEY")
                .help("Sets the integration key (falls back to ILERT_INTEGRATION_KEY env var)"),
        );

    let cleanup_cmd = Command::new("cleanup")
        .about("Clean up ilert resources (requires ILERT_API_KEY env var)")
        .arg(
            Arg::new("resource")
                .long("resource")
                .value_name("RESOURCE")
                .required(true)
                .help("Sets the resource target to clean up"),
        )
        .arg(
            Arg::new("responder")
                .long("responder")
                .value_name("USER_ID")
                .action(ArgAction::Append)
                .help("Filter alerts by responder user ID (can be specified multiple times)"),
        );

    Command::new("ilagent")
        .version(env!("CARGO_PKG_VERSION"))
        .author("ilert GmbH. <support@ilert.com>")
        .about(format!(
            "ilagent {} — Bridge your infrastructure to ilert.",
            env!("CARGO_PKG_VERSION")
        ))
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("v")
                .short('v')
                .long("verbose")
                .action(ArgAction::Count)
                .global(true)
                .help("Sets the level of verbosity"),
        )
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .value_name("FILE")
                .global(true)
                .help("File path for the SQLite database (default: ./ilagent.db3)"),
        )
        .subcommand(daemon_cmd)
        .subcommand(event_cmd)
        .subcommand(heartbeat_cmd)
        .subcommand(cleanup_cmd)
}

#[tokio::main]
async fn main() {
    let matches = build_cli().get_matches();

    let log_level = match matches.get_count("v") {
        0 => "warn",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => {
            error!("Maximum log level reached (4)");
            "debug"
        }
    };

    env_logger::Builder::from_env(Env::default().default_filter_or(log_level)).init();

    version_check::spawn_version_check();

    match matches.subcommand() {
        Some(("daemon", sub_m)) => {
            let config = build_daemon_config(sub_m, &matches);
            run_daemon(&config).await;
        }
        Some(("event", sub_m)) => run_event(sub_m).await,
        Some(("heartbeat", sub_m)) => run_heartbeat(sub_m).await,
        Some(("cleanup", sub_m)) => run_cleanup(sub_m).await,
        _ => unreachable!("subcommand_required prevents this"),
    }
}

pub fn build_daemon_config(matches: &ArgMatches, global_matches: &ArgMatches) -> ILConfig {
    let mut config = ILConfig::new();

    let default_port = config.get_port_as_string().clone();
    config.start_http = matches.get_one::<String>("port").is_some();
    let port = matches
        .get_one::<String>("port")
        .map(|s| s.as_str())
        .unwrap_or(default_port.as_str());
    config.set_port_from_str(port);

    if let Some(heartbeat_key) = matches.get_one::<String>("heartbeat") {
        config.heartbeat_key = Some(heartbeat_key.to_string());
    }

    if let Some(mqtt_host) = matches.get_one::<String>("mqtt_host") {
        let mqtt_port_str = matches
            .get_one::<String>("mqtt_port")
            .map(|s| s.as_str())
            .unwrap_or("1883");
        let mqtt_name = matches
            .get_one::<String>("mqtt_name")
            .map(|s| s.as_str())
            .unwrap_or("ilagent");

        config.mqtt_host = Some(mqtt_host.to_string());
        config.set_mqtt_port_from_str(mqtt_port_str);
        config.mqtt_name = Some(mqtt_name.to_string());

        if let Some(event_topic) = matches.get_one::<String>("event_topic") {
            config.event_topic = Some(event_topic.to_string());
        }

        if let Some(heartbeat_topic) = matches.get_one::<String>("heartbeat_topic") {
            config.heartbeat_topic = Some(heartbeat_topic.to_string());
        }

        config.mqtt_tls = matches.get_flag("mqtt_tls");
        config.mqtt_ca_path = matches.get_one::<String>("mqtt_ca").map(|s| s.to_string());
        config.mqtt_client_cert_path = matches
            .get_one::<String>("mqtt_client_cert")
            .map(|s| s.to_string());
        config.mqtt_client_key_path = matches
            .get_one::<String>("mqtt_client_key")
            .map(|s| s.to_string());

        if let Some(mqtt_qos) = matches.get_one::<String>("mqtt_qos") {
            let qos = mqtt_qos
                .parse::<u8>()
                .expect("Failed to parse mqtt_qos as integer");
            if qos > 2 {
                panic!("mqtt_qos must be 0, 1, or 2");
            }
            config.mqtt_qos = qos;
            info!("MQTT QoS level set to {}", qos);
        }

        config.mqtt_buffer = matches.get_flag("mqtt_buffer");
        if config.mqtt_buffer {
            info!("MQTT buffering enabled — messages will be queued in SQLite for retry");
        }

        if let Some(shared_group) = matches.get_one::<String>("mqtt_shared_group") {
            config.mqtt_shared_group = Some(shared_group.to_string());
            info!("MQTT shared subscription group: '{}'", shared_group);
        }

        config = parse_consumer_arguments(matches, config);
        consumers::mqtt::validate_mqtt_config(&config);
    }

    if let Some(kafka_brokers) = matches.get_one::<String>("kafka_brokers") {
        let kafka_group_id = matches
            .get_one::<String>("kafka_group_id")
            .map(|s| s.as_str())
            .unwrap_or("ilagent");

        config.kafka_brokers = Some(kafka_brokers.to_string());
        config.kafka_group_id = Some(kafka_group_id.to_string());

        if let Some(topic) = matches.get_one::<String>("event_topic") {
            config.event_topic = Some(topic.to_string());
        }

        if let Some(topic) = matches.get_one::<String>("heartbeat_topic") {
            config.heartbeat_topic = Some(topic.to_string());
        }

        config = parse_consumer_arguments(matches, config);
        if !config.start_http {
            config.start_http = true;
            warn!("The current version of ilagent, enforces the http server when kafka is used");
        }
    }

    if let Some(file) = global_matches.get_one::<String>("file") {
        config.db_file = file.to_string();
    } else if let Some(file) = matches.get_one::<String>("file") {
        config.db_file = file.to_string();
    }

    config
}

pub fn parse_consumer_arguments(matches: &ArgMatches, mut config: ILConfig) -> ILConfig {
    if let Some(username) = matches.get_one::<String>("mqtt_username") {
        config.mqtt_username = Some(username.to_string());

        if let Some(password) = matches.get_one::<String>("mqtt_password") {
            config.mqtt_password = Some(password.to_string());
            info!("mqtt credentials set");
        }
    }

    if let Some(filter_key) = matches.get_one::<String>("filter_key") {
        config.filter_key = Some(filter_key.to_string());
        info!(
            "Filter key is present: {:?} will be required in event payloads",
            config.filter_key
        );
    }

    if let Some(filter_val) = matches.get_one::<String>("filter_val") {
        config.filter_val = Some(filter_val.to_string());
        info!("Filter key value is present, will be required in event payloads");
    }

    config.forward_message_payload = matches.get_flag("forward_message_payload");
    if config.forward_message_payload {
        info!("Forward message payload enabled — full original JSON will be set as customDetails");
    }

    // mappings
    if let Some(event_key) = matches.get_one::<String>("event_key") {
        config.event_key = Some(event_key.to_string());
        info!("eventKey has been configured, overwriting events with static key");
    }

    if let Some(map_key_etype) = matches.get_one::<String>("map_key_etype") {
        config.map_key_etype = Some(map_key_etype.to_string());
        info!(
            "Overwrite for payload key 'eventType' has been configured: '{:?}'",
            config.map_key_etype
        );
    }

    if let Some(map_key_alert_key) = matches.get_one::<String>("map_key_alert_key") {
        config.map_key_alert_key = Some(map_key_alert_key.to_string());
        info!(
            "Overwrite for payload key 'alertKey' has been configured: '{:?}'",
            config.map_key_alert_key
        );
    }

    if let Some(map_key_summary) = matches.get_one::<String>("map_key_summary") {
        config.map_key_summary = Some(map_key_summary.to_string());
        info!(
            "Overwrite for payload key 'summary' has been configured: '{:?}'",
            config.map_key_summary
        );
    }

    if let Some(map_val_etype_alert) = matches.get_one::<String>("map_val_etype_alert") {
        config.map_val_etype_alert = Some(map_val_etype_alert.to_string());
        info!(
            "Overwrite for payload val of key 'eventType' and default: 'ALERT' has been configured: '{:?}'",
            config.map_val_etype_alert
        );
    }

    if let Some(map_val_etype_accept) = matches.get_one::<String>("map_val_etype_accept") {
        config.map_val_etype_accept = Some(map_val_etype_accept.to_string());
        info!(
            "Overwrite for payload val of key 'eventType' and default: 'ACCEPT' has been configured: '{:?}'",
            config.map_val_etype_accept
        );
    }

    if let Some(map_val_etype_resolve) = matches.get_one::<String>("map_val_etype_resolve") {
        config.map_val_etype_resolve = Some(map_val_etype_resolve.to_string());
        info!(
            "Overwrite for payload val of key 'eventType' and default: 'RESOLVE' has been configured: '{:?}'",
            config.map_val_etype_resolve
        );
    }

    if let Ok(api_key) = std::env::var("ILERT_API_KEY") {
        config.api_key = Some(strip_bearer_prefix(api_key));
        info!("API key has been configured from ILERT_API_KEY environment variable");
    }

    if let Some(policy_topic) = matches.get_one::<String>("policy_topic") {
        config.policy_topic = Some(policy_topic.to_string());
        info!("Policy topic has been configured: '{}'", policy_topic);
    }

    if let Some(policy_routing_keys) = matches.get_one::<String>("policy_routing_keys") {
        config.policy_routing_keys = Some(policy_routing_keys.to_string());
        info!(
            "Policy routing keys have been configured: '{}'",
            policy_routing_keys
        );
    }

    if let Some(map_key_email) = matches.get_one::<String>("map_key_email") {
        config.map_key_email = Some(map_key_email.to_string());
        info!("Email field path has been configured: '{}'", map_key_email);
    }

    if let Some(map_key_shift) = matches.get_one::<String>("map_key_shift") {
        config.map_key_shift = Some(map_key_shift.to_string());
        info!("Shift field path has been configured: '{}'", map_key_shift);
    }

    if let Some(shift_offset) = matches.get_one::<String>("shift_offset") {
        config.shift_offset = shift_offset
            .parse::<i64>()
            .expect("Failed to parse shift_offset as integer");
        info!("Shift offset has been configured: {}", config.shift_offset);
    }

    if let Some(max_retries) = matches.get_one::<String>("max_retries") {
        config.max_retries = max_retries
            .parse::<u32>()
            .expect("Failed to parse max_retries as integer");
        if config.max_retries == 0 {
            info!("Max retries set to unlimited");
        } else {
            info!("Max retries has been configured: {}", config.max_retries);
        }
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
async fn run_daemon(config: &ILConfig) {
    info!("Starting daemon..");

    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        orig_hook(panic_info);
        process::exit(1);
    }));

    let mut ilert_client = ILert::new().expect("Failed to create ilert client");
    if let Some(ref api_key) = config.api_key {
        ilert_client
            .auth_via_token(api_key)
            .expect("Failed to set API key");
    }
    let db = ILDatabase::new(config.db_file.as_str());
    info!("Migrating DB..");
    db.prepare_database();

    let daemon_ctx = Arc::new(DaemonContext {
        config: config.clone(),
        db: Mutex::new(db),
        ilert_client,
        running: AtomicBool::new(true),
    });

    // kafka stream will not exit when ctrlc hook is present
    if config.kafka_brokers.is_none() {
        let ctrlc_ctx = daemon_ctx.clone();
        ctrlc::set_handler(move || {
            info!("Received Ctrl+C. Shutting down threads...");
            ctrlc_ctx.running.store(false, Ordering::Relaxed);
        })
        .expect("Error setting Ctrl-C handler");
    }

    // poll is only needed if mqtt or web server are running
    let is_poll_needed = config.start_http || config.mqtt_buffer;
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
    let mut mqtt_poll_job = None;
    if config.mqtt_host.is_some() {
        info!("Running MQTT thread..");
        let cloned_ctx = daemon_ctx.clone();
        // rumqttc spawns its own tokio runtime
        mqtt_job = Some(tokio::task::spawn_blocking(move || {
            consumers::mqtt::run_mqtt_job(cloned_ctx);
        }));

        if config.mqtt_buffer {
            info!("Starting MQTT poll job..");
            let cloned_ctx = daemon_ctx.clone();
            mqtt_poll_job = Some(tokio::spawn(async move {
                poll::run_mqtt_poll_job(cloned_ctx).await;
            }));
        }
    }

    if config.kafka_brokers.is_some() {
        info!("Running Kafka thread..");
        let cloned_ctx = daemon_ctx.clone();
        tokio::spawn(async move {
            consumers::kafka::run_kafka_job(cloned_ctx).await;
        });
    }

    if config.start_http {
        http_server::run_server(daemon_ctx.clone()).await;
        // blocking...
        debug!("http ended");
        daemon_ctx.running.store(false, Ordering::Relaxed);
    }

    if let Some(handle) = poll_job {
        handle.await.expect("Failed to join poll thread");
        debug!("poll ended");
    }

    if let Some(handle) = hbt_job {
        handle.await.expect("Failed to join heartbeat thread");
        debug!("hbt ended");
    }

    if let Some(handle) = mqtt_poll_job {
        handle.await.expect("Failed to join mqtt poll thread");
        debug!("mqtt poll ended");
    }

    if let Some(handle) = mqtt_job {
        info!("waiting for mqtt to drop connections...");
        handle.await.expect("Failed to join mqtt thread");
        debug!("mqtt ended");
    }

    /* as soon as actix or ctrcl_handler is used the kafka streaming consumer will not shut down
    if let Some(handle) = kafka_job {
        handle.await.expect("Failed to join kafka thread");
        info!("kafka ended");
    } */
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_cli: basic subcommand parsing ---

    #[test]
    fn cli_requires_subcommand() {
        let result = build_cli().try_get_matches_from(vec!["ilagent"]);
        assert!(result.is_err());
    }

    #[test]
    fn cli_daemon_subcommand_no_args() {
        let m = build_cli().try_get_matches_from(vec!["ilagent", "daemon"]);
        assert!(m.is_ok());
        assert!(m.unwrap().subcommand_matches("daemon").is_some());
    }

    #[test]
    fn cli_event_requires_event_type_and_summary() {
        let result = build_cli().try_get_matches_from(vec!["ilagent", "event", "-k", "key1"]);
        assert!(result.is_err()); // missing event_type and summary
    }

    #[test]
    fn cli_event_valid() {
        let m = build_cli().try_get_matches_from(vec![
            "ilagent", "event", "-k", "key1", "-t", "ALERT", "-s", "test",
        ]);
        assert!(m.is_ok());
        let sub = m.unwrap();
        let event_m = sub.subcommand_matches("event").unwrap();
        assert_eq!(
            event_m.get_one::<String>("integration_key").unwrap(),
            "key1"
        );
        assert_eq!(event_m.get_one::<String>("event_type").unwrap(), "ALERT");
        assert_eq!(event_m.get_one::<String>("summary").unwrap(), "test");
    }

    #[test]
    fn cli_event_rejects_invalid_event_type() {
        let result = build_cli().try_get_matches_from(vec![
            "ilagent", "event", "-k", "key1", "-t", "INVALID", "-s", "test",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn cli_event_rejects_invalid_priority() {
        let result = build_cli().try_get_matches_from(vec![
            "ilagent", "event", "-k", "key1", "-t", "ALERT", "-s", "test", "-o", "MEDIUM",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn cli_heartbeat_accepts_no_args() {
        // integration_key can come from env, so CLI parses fine without it
        let result = build_cli().try_get_matches_from(vec!["ilagent", "heartbeat"]);
        assert!(result.is_ok());
    }

    #[test]
    fn cli_heartbeat_valid() {
        let m = build_cli().try_get_matches_from(vec!["ilagent", "heartbeat", "-k", "hbt123"]);
        assert!(m.is_ok());
    }

    #[test]
    fn cli_cleanup_requires_resource() {
        let result = build_cli().try_get_matches_from(vec!["ilagent", "cleanup"]);
        assert!(result.is_err()); // missing --resource
    }

    #[test]
    fn cli_cleanup_valid() {
        let m =
            build_cli().try_get_matches_from(vec!["ilagent", "cleanup", "--resource", "alerts"]);
        assert!(m.is_ok());
    }

    #[test]
    fn cli_cleanup_with_responders() {
        let m = build_cli().try_get_matches_from(vec![
            "ilagent",
            "cleanup",
            "--resource",
            "alerts",
            "--responder",
            "user1",
            "--responder",
            "user2",
        ]);
        assert!(m.is_ok());
        let sub = m.unwrap().subcommand_matches("cleanup").unwrap().clone();
        let responders: Vec<&str> = sub
            .get_many::<String>("responder")
            .unwrap()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(responders, vec!["user1", "user2"]);
    }

    #[test]
    fn cli_cleanup_incidents_valid() {
        let m =
            build_cli().try_get_matches_from(vec!["ilagent", "cleanup", "--resource", "incidents"]);
        assert!(m.is_ok());
    }

    #[test]
    fn cli_verbose_flag_counts() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "-vvv", "daemon"])
            .unwrap();
        assert_eq!(m.get_count("v"), 3);
    }

    #[test]
    fn cli_global_file_flag() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "-f", "/tmp/test.db3", "daemon"])
            .unwrap();
        assert_eq!(m.get_one::<String>("file").unwrap(), "/tmp/test.db3");
    }

    // --- build_daemon_config: minimal ---

    #[test]
    fn daemon_config_defaults() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "daemon"])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(!config.start_http);
        assert_eq!(config.http_port, 8977);
        assert!(config.heartbeat_key.is_none());
        assert!(config.mqtt_host.is_none());
        assert!(config.kafka_brokers.is_none());
        assert_eq!(config.db_file, "./ilagent.db3");
    }

    // --- build_daemon_config: http ---

    #[test]
    fn daemon_config_with_port_enables_http() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "daemon", "-p", "9000"])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(config.start_http);
        assert_eq!(config.http_port, 9000);
    }

    // --- build_daemon_config: heartbeat ---

    #[test]
    fn daemon_config_with_heartbeat() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "daemon", "-b", "il1hbt123"])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.heartbeat_key.unwrap(), "il1hbt123");
    }

    // --- build_daemon_config: MQTT ---

    #[test]
    #[should_panic(expected = "At least one MQTT topic must be configured")]
    fn daemon_config_mqtt_requires_topic() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "daemon", "-m", "broker.local"])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        build_daemon_config(sub, &m);
    }

    #[test]
    #[should_panic(expected = "QoS 0 has no broker acknowledgement")]
    fn daemon_config_mqtt_non_buffered_rejects_qos_zero() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "-e",
                "ilert/events",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        build_daemon_config(sub, &m);
    }

    #[test]
    fn daemon_config_mqtt_defaults_with_event_topic() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "-e",
                "ilert/events",
                "--mqtt_qos",
                "1",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.mqtt_host.unwrap(), "broker.local");
        assert_eq!(config.mqtt_port.unwrap(), 1883);
        assert_eq!(config.mqtt_name.unwrap(), "ilagent");
        assert_eq!(config.event_topic.unwrap(), "ilert/events");
        assert!(config.heartbeat_topic.is_none());
        assert_eq!(config.mqtt_qos, 1);
        assert!(!config.mqtt_tls);
    }

    #[test]
    fn daemon_config_mqtt_buffer_allows_qos_zero() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "-e",
                "ilert/events",
                "--mqtt_buffer",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(config.mqtt_buffer);
        assert_eq!(config.mqtt_qos, 0);
    }

    #[test]
    fn daemon_config_mqtt_custom() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "-q",
                "8883",
                "-n",
                "myagent",
                "--mqtt_tls",
                "--mqtt_username",
                "user1",
                "--mqtt_password",
                "pass1",
                "-e",
                "custom/events",
                "-r",
                "custom/heartbeats",
                "--mqtt_qos",
                "1",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.mqtt_port.unwrap(), 8883);
        assert_eq!(config.mqtt_name.unwrap(), "myagent");
        assert!(config.mqtt_tls);
        assert_eq!(config.mqtt_username.unwrap(), "user1");
        assert_eq!(config.mqtt_password.unwrap(), "pass1");
        assert_eq!(config.event_topic.unwrap(), "custom/events");
        assert_eq!(config.heartbeat_topic.unwrap(), "custom/heartbeats");
    }

    #[test]
    fn daemon_config_mqtt_tls_certs() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "--mqtt_tls",
                "-e",
                "ilert/events",
                "--mqtt_qos",
                "1",
                "--mqtt_ca",
                "/certs/ca.pem",
                "--mqtt_client_cert",
                "/certs/client.pem",
                "--mqtt_client_key",
                "/certs/client.key",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(config.mqtt_tls);
        assert_eq!(config.mqtt_ca_path.unwrap(), "/certs/ca.pem");
        assert_eq!(config.mqtt_client_cert_path.unwrap(), "/certs/client.pem");
        assert_eq!(config.mqtt_client_key_path.unwrap(), "/certs/client.key");
    }

    // --- build_daemon_config: Kafka ---

    #[test]
    fn daemon_config_kafka_enables_http() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "--kafka_brokers",
                "localhost:9092",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.kafka_brokers.unwrap(), "localhost:9092");
        assert_eq!(config.kafka_group_id.unwrap(), "ilagent");
        assert!(config.start_http, "kafka should force http on");
    }

    #[test]
    fn daemon_config_kafka_custom_group() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "--kafka_brokers",
                "localhost:9092",
                "--kafka_group_id",
                "mygroup",
                "-e",
                "kafka/events",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.kafka_group_id.unwrap(), "mygroup");
        assert_eq!(config.event_topic.unwrap(), "kafka/events");
    }

    // --- build_daemon_config: consumer mappings ---

    #[test]
    fn daemon_config_consumer_mappings_via_mqtt() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "-e",
                "ilert/events",
                "--mqtt_qos",
                "1",
                "--event_key",
                "static-key",
                "--map_key_etype",
                "state",
                "--map_key_alert_key",
                "mCode",
                "--map_key_summary",
                "comment",
                "--map_val_etype_alert",
                "SET",
                "--map_val_etype_accept",
                "ACK",
                "--map_val_etype_resolve",
                "CLR",
                "--filter_key",
                "type",
                "--filter_val",
                "ALARM",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.event_key.unwrap(), "static-key");
        assert_eq!(config.map_key_etype.unwrap(), "state");
        assert_eq!(config.map_key_alert_key.unwrap(), "mCode");
        assert_eq!(config.map_key_summary.unwrap(), "comment");
        assert_eq!(config.map_val_etype_alert.unwrap(), "SET");
        assert_eq!(config.map_val_etype_accept.unwrap(), "ACK");
        assert_eq!(config.map_val_etype_resolve.unwrap(), "CLR");
        assert_eq!(config.filter_key.unwrap(), "type");
        assert_eq!(config.filter_val.unwrap(), "ALARM");
    }

    // --- build_daemon_config: db file ---

    #[test]
    fn daemon_config_global_db_file() {
        let m = build_cli()
            .try_get_matches_from(vec!["ilagent", "-f", "/data/custom.db3", "daemon"])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.db_file, "/data/custom.db3");
    }

    // --- build_daemon_config: combined ---

    #[test]
    fn daemon_config_mqtt_plus_heartbeat_plus_http() {
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-p",
                "3000",
                "-b",
                "il1hbt999",
                "-m",
                "mqtt.example.com",
                "-e",
                "ilert/events",
                "--mqtt_qos",
                "1",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(config.start_http);
        assert_eq!(config.http_port, 3000);
        assert_eq!(config.heartbeat_key.unwrap(), "il1hbt999");
        assert_eq!(config.mqtt_host.unwrap(), "mqtt.example.com");
    }

    // --- build_daemon_config: policy ---

    #[test]
    fn daemon_config_policy_args() {
        // env var tests must be in a single test to avoid races with parallel execution
        unsafe {
            std::env::set_var("ILERT_API_KEY", "il1api-test-token");
        }

        // mqtt with policy
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "-m",
                "broker.local",
                "--policy_topic",
                "ilert/policies",
                "--mqtt_qos",
                "1",
                "--policy_routing_keys",
                "location,slot",
                "--filter_key",
                "eventType",
                "--filter_val",
                "active",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.api_key.as_deref().unwrap(), "il1api-test-token");
        assert_eq!(config.policy_topic.unwrap(), "ilert/policies");
        assert_eq!(config.policy_routing_keys.unwrap(), "location,slot");
        assert_eq!(config.filter_key.unwrap(), "eventType");
        assert_eq!(config.filter_val.unwrap(), "active");

        // kafka with policy
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "--kafka_brokers",
                "localhost:9092",
                "--policy_topic",
                "policy-updates",
                "--policy_routing_keys",
                "location",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert_eq!(config.api_key.as_deref().unwrap(), "il1api-test-token");
        assert_eq!(config.policy_topic.unwrap(), "policy-updates");
        assert_eq!(config.policy_routing_keys.unwrap(), "location");

        unsafe {
            std::env::remove_var("ILERT_API_KEY");
        }
    }

    // --- consumer mappings only apply when mqtt or kafka is present ---

    #[test]
    fn daemon_config_mappings_ignored_without_consumer() {
        // without mqtt_host or kafka_brokers, consumer args are never parsed
        let m = build_cli()
            .try_get_matches_from(vec![
                "ilagent",
                "daemon",
                "--event_key",
                "static-key",
                "--filter_key",
                "type",
            ])
            .unwrap();
        let sub = m.subcommand_matches("daemon").unwrap();
        let config = build_daemon_config(sub, &m);
        assert!(config.event_key.is_none());
        assert!(config.filter_key.is_none());
    }
}

fn resolve_integration_key(matches: &ArgMatches) -> String {
    matches.get_one::<String>("integration_key")
        .cloned()
        .or_else(|| std::env::var("ILERT_INTEGRATION_KEY").ok())
        .expect("Integration key is required: provide --integration_key or set ILERT_INTEGRATION_KEY env var")
}

/**
    Attempts to create an event, one time - skips queue
*/
async fn run_event(matches: &ArgMatches) {
    let ilert_client = ILert::new().expect("Failed to create ilert client");
    let integration_key = resolve_integration_key(matches);
    let event_type = matches.get_one::<String>("event_type").unwrap();
    let summary = matches.get_one::<String>("summary").unwrap();

    let alert_key = matches
        .get_one::<String>("alert_key")
        .map(|k| k.to_string());
    let priority = matches.get_one::<String>("priority").map(|k| k.to_string());
    let details = matches.get_one::<String>("details").map(|k| k.to_string());

    let mut images = None;
    if let Some(val) = matches.get_one::<String>("image") {
        let vec = vec![EventImage::new(val)];
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
    if let Some(val) = matches.get_one::<String>("link") {
        let mut e_link = EventLink::new(val);
        e_link.text = Some("Provided Url".to_string());
        let vec = vec![e_link];
        let j = serde_json::to_string(&vec);
        links = match j {
            Ok(v) => Some(v),
            Err(e) => {
                error!("Failed to parse link {}", e);
                None
            }
        };
    }

    let mut event =
        EventQueueItem::new_with_required(&integration_key, event_type, summary, alert_key);

    event.id = Some("provided".to_string()); // prettier logs
    event.priority = priority;
    event.details = details;
    event.images = images;
    event.links = links;

    poll::send_queued_event(&ilert_client, &event).await;
}

/**
    Attempts to ping a heartbeat
*/
async fn run_heartbeat(matches: &ArgMatches) {
    let ilert_client = ILert::new().expect("Failed to create ilert client");
    let integration_key = resolve_integration_key(matches);

    if hbt::ping_heartbeat(&ilert_client, &integration_key).await {
        info!("Heartbeat ping successful");
    }
}

/**
    Attempts to clean-up resources
*/
async fn run_cleanup(matches: &ArgMatches) {
    let api_key = strip_bearer_prefix(
        std::env::var("ILERT_API_KEY")
            .expect("ILERT_API_KEY environment variable is required for cleanup"),
    );
    let mut ilert_client = ILert::new().expect("Failed to create ilert client");
    ilert_client
        .auth_via_token(&api_key)
        .expect("Failed to set api key");

    let resource = matches.get_one::<String>("resource").unwrap();
    let responders: Vec<&str> = matches
        .get_many::<String>("responder")
        .map(|vals| vals.map(|s| s.as_str()).collect())
        .unwrap_or_default();
    match resource.as_str() {
        "alerts" => cleanup::cleanup_alerts(&ilert_client, &responders).await,
        "incidents" => {
            if !responders.is_empty() {
                warn!("--responder filter is not supported for incidents, ignoring");
            }
            cleanup::cleanup_incidents(&ilert_client).await
        }
        _ => panic!("Unsupported 'resource' provided."),
    }
}

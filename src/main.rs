#![allow(dead_code,unused)]

use log::{debug, info, error};
use env_logger::Env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use clap::{Arg, App, ArgMatches};

use ilert::ilert::ILert;
use ilert::ilert_builders::{EventApiResource};

mod il_config;
use il_config::ILConfig;

mod il_db;
use il_db::ILDatabase;

mod il_hbt;
use il_hbt::ping_heartbeat;

mod il_poll;
use il_poll::process_queued_event;
use crate::il_db::EventQueueItem;

mod il_server;

fn main() -> () {

    let matches = App::new("iLert Agent")

        .version("0.2.0")
        .author("iLert GmbH. <support@ilert.com>")
        .about("The iLert Agent lets you easily integrate your system requirements with iLert.")

        .arg(Arg::with_name("COMMAND")
            .help("The actual command that should be executed.")
            .max_values(1)
            .possible_values(&["daemon", "event", "heartbeat"])
            .required(true)
            .index(1))

        .arg(Arg::with_name("port")
            .short("p")
            .long("port")
            .value_name("PORT")
            .help("Sets a custom port for the daemon's http server")
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

        .arg(Arg::with_name("incident_key")
            .short("i")
            .long("incident_key")
            .value_name("INCIDENT_KEY")
            .help("Sets the event incident key for the 'event' command")
            .takes_value(true)
        )

        .arg(Arg::with_name("v")
            .short("v")
            .long("verbose")
            .multiple(true)
            .help("Sets the level of verbosity")
            )

        // TODO: arg to override sqlite db location

        .get_matches();

    let mut config = ILConfig::new();

    let default_port = config.get_port_as_string().clone();
    let port = matches.value_of("port").unwrap_or(default_port.as_str());
    config.set_port_from_str(port);

    let command = matches.value_of("COMMAND").expect("Failed to parse provided command");

    let log_level = match matches.occurrences_of("v") {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => panic!("Maximum log level reached (4)")
    };

    if matches.is_present("heartbeat") {
        let heartbeat_key = matches.value_of("heartbeat").expect("Failed to parse heartbeat api key");
        config.heartbeat_key = Some(heartbeat_key.to_string());
    }

    env_logger::from_env(Env::default()
        .default_filter_or(log_level))
        .init();

    debug!("Running command: {}.", command);
    match command {
        "daemon" => run_daemon(&config),
        "event" => run_event(&config, &matches),
        "heartbeat" => run_heartbeat(&config, &matches),
        _ => panic!("Unsupported command provided.") // unreachable
    }
}

/**
    Starts http server with proxy functionality /api/v1/events and /api/v1/heartbeats
    Where events are queued in a local SQLite table to ensure delivery
    If provided, pings a heartbeat api key regularly
    If provided, connects to MQTT broker and proxies events and heartbeats
*/
fn run_daemon(config: &ILConfig) -> () {

    let are_we_running = Arc::new(AtomicBool::new(true));
    let are_we_running_a = are_we_running.clone();
    ctrlc::set_handler(move || {
        info!("Received Ctrl+C.");
        are_we_running_a.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    info!("Starting..");
    let db_web_instance = ILDatabase::new(config.db_file.as_str());
    info!("Migrating DB..");
    db_web_instance.prepare_database();

    info!("Starting poll job..");
    let poll_job = il_poll::run_poll_job(&config, &are_we_running);

    let mut hbt_job = None;
    if config.heartbeat_key.is_some() {
        info!("Running regular heartbeats..");
        hbt_job = Some(il_hbt::run_hbt_job(&config, &are_we_running));
    }

    info!("Starting server..");
    il_server::run_server(&config, db_web_instance).expect("Failed to start http server");
    // blocking..

    poll_job.join().expect("Failed to join poll thread");

    if let Some(handle) = hbt_job {
        handle.join().expect("Failed to join heartbeat thread");
    }

    ()
}

/**
    Attempts to create event, one time - skips queue
*/
fn run_event(config: &ILConfig, matches: &ArgMatches) -> () {

    if !matches.is_present("api_key") {
        return error!("Missing api_key arg (-k, --api_key)");
    }

    if !matches.is_present("event_type") {
        return error!("Missing alert_type arg (-t, --event_type)");
    }

    if !matches.is_present("summary") {
        return error!("Missing summary arg (-s, --summary)");
    }

    let ilert_client = ILert::new().expect("Failed to create iLert client");
    let api_key = matches.value_of("api_key").unwrap();
    let event_type = matches.value_of("event_type").unwrap();
    let summary = matches.value_of("summary").unwrap();

    let incident_key = matches.value_of("incident_key");
    let incident_key = match incident_key {
        Some(k) => Some(k.to_string()),
        None => None
    };

    let mut event = EventQueueItem::new_with_required(api_key, event_type, summary, incident_key);
    event.id = Some("provided".to_string()); // prettier logs
    il_poll::process_queued_event(&ilert_client, &event);
}

/**
    Attempts to ping a heartbeat
*/
fn run_heartbeat(config: &ILConfig, matches: &ArgMatches) -> () {

    if !matches.is_present("api_key") {
        return error!("Missing api_key arg (-k, --api_key)");
    }

    let ilert_client = ILert::new().expect("Failed to create iLert client");
    let api_key = matches.value_of("api_key").unwrap();

    if il_hbt::ping_heartbeat(&ilert_client, api_key) {
        info!("Heartbeat ping successful");
    }
}

#![allow(dead_code,unused)]

use log::info;
use env_logger::Env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use clap::{Arg, App, ArgMatches};

mod il_config;
use il_config::ILConfig;

mod il_db;
use il_db::ILDatabase;

mod il_server;
mod il_poll;
mod il_hbt;

fn main() -> () {

    let matches = App::new("iLert Agent")

        .version("0.2.0")
        .author("iLert GmbH. <cff@ilert.de>")
        .about("The iLert Agent lets you easily integrate your system requirements with iLert.")

        .arg(Arg::with_name("command")
            .help("The actual command that should be executed.")
            .max_values(1)
            .value_name("COMMAND")
            .possible_values(&["daemon", "event"])
            .required(true)
            .index(1))

        .arg(Arg::with_name("port")
            .short("p")
            .long("port")
            .value_name("PORT")
            .help("Sets a custom port for the http server")
            .takes_value(true))

        .arg(Arg::with_name("heartbeat")
            .short("b")
            .long("heartbeat")
            .value_name("HEARTBEAT")
            .help("Sets the API key of the heartbeat (pings every minute)")
            .takes_value(true))

        .arg(Arg::with_name("v")
            .short("v")
            .long("verbose")
            .multiple(true)
            .help("Sets the level of verbosity"))

        .get_matches();

    let mut config = ILConfig::new();

    let default_port = config.get_port_as_string().clone();
    let port = matches.value_of("port").unwrap_or(default_port.as_str());
    config.set_port_from_str(port);

    let command = matches.value_of("command").expect("Failed to parse provided command");

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

    info!("Running command: {}.", command);
    match command {
        "daemon" => run_daemon(&config),
        "event" => run_event(&config, &matches),
        _ => panic!("Unsupported command provided.") // unreachable
    }
}

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

fn run_event(config: &ILConfig, matches: &ArgMatches) -> () {
    // TODO: implement
}


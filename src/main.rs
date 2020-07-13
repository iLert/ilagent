#![allow(dead_code,unused)]

use log::info;
use env_logger::Env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use clap::{Arg, App};

mod il_config;
use il_config::ILConfig;

mod il_db;
use il_db::ILDatabase;

mod il_server;
mod il_poll;

fn main() {

    let matches = App::new("iLert Agent")
        .version("0.1.0")
        .author("iLert GmbH. <cff@ilert.de>")
        .about("The iLert Agent is a program that lets you easily integrate your monitoring system with iLert.")
        .arg(Arg::with_name("port")
            .short("p")
            .long("port")
            .value_name("PORT")
            .help("Sets a custom port for the http server")
            .takes_value(true))
        .arg(Arg::with_name("command")
            .help("The actual command that should be executed.")
            .max_values(1)
            .value_name("COMMAND")
            .possible_values(&["daemon", "event", "heartbeat"])
            .required(true)
            .index(1))
        .arg(Arg::with_name("v")
            .short("v")
            .multiple(true)
            .help("Sets the level of verbosity"))
        .get_matches();

    let mut config = ILConfig::new();

    let default_port = config.get_port_as_string().clone();
    let port = matches.value_of("port").unwrap_or(default_port.as_str());
    config.set_port_from_str(port);

    let command = matches.value_of("command").expect("Failed to parse provided command");
    info!("Using command: {}.", command);

    let log_level = match matches.occurrences_of("v") {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => panic!("Maximum log level reached (4)"),
    };

    env_logger::from_env(Env::default()
        .default_filter_or(log_level))
        .init();

    match command {
        "daemon" => run_daemon(&config),
        "event" => run_create_event(&config),
        // TODO: add mqtt mode
        // "heartbeat" => run_heartbeat_daemon(&config), // TODO: run this as optional config that starts a thread in daemon
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

    info!("Starting server..");
    il_server::run_server(&config, db_web_instance).expect("Failed to start http server");

    poll_job.join().expect("Failed to join poll thread");
    ()
}

fn run_create_event(config: &ILConfig) -> () {
    // TODO: implement
}


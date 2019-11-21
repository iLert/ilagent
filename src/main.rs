#![allow(dead_code,unused)]

use log::info;
use env_logger::Env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use clap::{Arg, App};

mod ilconfig;
use ilconfig::ILConfig;

mod ilqueue;
use ilqueue::ILQueue;

mod ilserver;
use ilserver::run_server;

mod ilpoll;
use ilpoll::run_poll_job;

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

    let command = matches.value_of("command").unwrap();
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
        "event" => send_create_event(&config),
        "heartbeat" => send_create_heartbeat(&config),
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
    let queue_web_instance = ILQueue::new(config.db_file.as_str());
    info!("Migrating DB..");
    queue_web_instance.prepare_database();

    info!("Starting poll job..");
    let poll_job = run_poll_job(config.clone(), are_we_running);

    info!("Starting server..");
    run_server(queue_web_instance, config.get_http_bind_str().as_str()).unwrap();

    poll_job.join().unwrap();
    ()
}

fn send_create_event(config: &ILConfig) -> () {
    // TODO: implement
}

fn send_create_heartbeat(config: &ILConfig) -> () {
    // TODO: implement
}
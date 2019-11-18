#![allow(dead_code)]

use log::info;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod ilconfig;
use ilconfig::ILConfig;

mod ilqueue;
use ilqueue::ILQueue;

mod ilserver;
use ilserver::run_server;

mod ilpoll;
use ilpoll::run_poll_job;

fn main() -> () {
    env_logger::init();

    let config = ILConfig::new();

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
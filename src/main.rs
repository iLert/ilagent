#![allow(dead_code)]

use std::thread;
use log::info;
use std::time::Duration;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod ilqueue;
use ilqueue::ILQueue;

mod ilserver;
use ilserver::run_server;

fn main() -> () {
    env_logger::init();

    let are_we_running = Arc::new(AtomicBool::new(true));
    let are_we_running_a = are_we_running.clone();
    ctrlc::set_handler(move || {
        info!("Received Ctrl+C.");
        are_we_running_a.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    info!("Starting server..");
    let queue_web_instance = ILQueue::new("./ilagent.db3");
    queue_web_instance.prepare_database();

    let are_we_running_b = are_we_running.clone();
    let poll_thread = thread::spawn(move || {
        let queue = ILQueue::new("./ilagent.db3");
        loop {
            thread::sleep(Duration::new(3, 0));
            if !are_we_running_b.load(Ordering::SeqCst) {
                break;
            }

            let incidents = queue.get_incidents(50);
            info!("Found incidents: {}.", incidents.len());
        }
    });

    let http_bind_str = format!("0.0.0.0:{}", 8977);
    run_server(queue_web_instance, http_bind_str).unwrap();

    poll_thread.join().unwrap();
    ()
}
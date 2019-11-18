use std::thread;
use std::thread::JoinHandle;
use log::info;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ilqueue::ILQueue;
use crate::ilconfig::ILConfig;

pub fn run_poll_job(config: ILConfig, are_we_running: Arc<AtomicBool>) -> JoinHandle<()> {

    let are_we_running_b = are_we_running.clone();
    let poll_thread = thread::spawn(move || {
        // thread gets its own db instance, no migrations required
        let queue = ILQueue::new(config.db_file.as_str());
        loop {
            thread::sleep(Duration::new(3, 0));
            if !are_we_running_b.load(Ordering::SeqCst) {
                break;
            }

            let incidents = queue.get_incidents(50);
            info!("Found incidents: {}.", incidents.len());
        }
    });

    poll_thread
}
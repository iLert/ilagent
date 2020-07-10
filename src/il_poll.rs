use std::thread;
use std::thread::JoinHandle;
use log::info;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::il_db::ILDatabase;
use crate::il_config::ILConfig;

pub fn run_poll_job(config: &ILConfig, are_we_running: &Arc<AtomicBool>) -> JoinHandle<()> {

    let config = config.clone();
    let are_we_running = are_we_running.clone();
    let poll_thread = thread::spawn(move || {

        let mut last_run = Instant::now();
        // thread gets its own db instance, no migrations required
        let db = ILDatabase::new(config.db_file.as_str());
        loop {
            thread::sleep(Duration::new(1, 0));
            if !are_we_running.load(Ordering::SeqCst) {
                break;
            }

            if last_run.elapsed().as_millis() < 5000 {
                continue;
            } else {
                last_run = Instant::now();
            }

            let items = db.get_il_events(50);
            info!("Found {} queued items.", items.len());
        }
    });

    poll_thread
}
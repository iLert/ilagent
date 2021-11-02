use std::thread;
use std::thread::JoinHandle;
use log::{error};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ilert::ilert::ILert;
use ilert::ilert_builders::{HeartbeatApiResource};

use crate::il_config::ILConfig;

pub fn run_hbt_job(config: &ILConfig, are_we_running: &Arc<AtomicBool>) -> JoinHandle<()> {

    let config = config.clone();
    let are_we_running = are_we_running.clone();
    let hbt_thread = thread::spawn(move || {

        let ilert_client = ILert::new().expect("Failed to create iLert client");
        let mut last_run = Instant::now();

        let api_key = config.heartbeat_key
            .expect("Failed to access heartbeat api key");
        let api_key = api_key.as_str();

        // kick off call
        ping_heartbeat(&ilert_client, api_key);

        loop {
            thread::sleep(Duration::from_millis(300));
            if !are_we_running.load(Ordering::SeqCst) {
                break;
            }

            if last_run.elapsed().as_millis() < 30000 {
                continue;
            } else {
                last_run = Instant::now();
            }

            ping_heartbeat(&ilert_client, api_key);
        }
    });

    hbt_thread
}

pub fn ping_heartbeat(ilert_client: &ILert, api_key: &str) -> bool {

    let hbt_result = ilert_client
        .get()
        .heartbeat(api_key)
        .execute();

    match hbt_result {
        Ok(result) => {
            let status_code = result.status.as_u16();
            match status_code {
                202 => true,
                _ => {
                    error!("Bad heartbeat http response {}", status_code);
                    false
                }
            }
        },
        Err(e) => {
            error!("Heartbeat http request failed {}", e);
            false
        }
    }
}
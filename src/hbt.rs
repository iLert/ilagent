use std::sync::Arc;
use log::{error};
use std::time::{Duration, Instant};

use ilert::ilert::ILert;
use ilert::ilert_builders::{HeartbeatApiResource};
use crate::DaemonContext;

pub async fn run_hbt_job(daemon_context: Arc<DaemonContext>) -> () {

    let mut last_run = Instant::now();

    let api_key = daemon_context.config.clone().heartbeat_key
        .expect("Failed to access heartbeat api key");
    let api_key = api_key.as_str();

    // kick off call
    ping_heartbeat(&daemon_context.ilert_client, api_key).await;

    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;

        if last_run.elapsed().as_millis() < 30000 {
            continue;
        } else {
            last_run = Instant::now();
        }

        ping_heartbeat(&daemon_context.ilert_client, api_key).await;
    }
}

pub async fn ping_heartbeat(ilert_client: &ILert, api_key: &str) -> bool {

    let hbt_result = ilert_client
        .get()
        .heartbeat(api_key)
        .execute()
        .await;

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
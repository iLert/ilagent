use log::error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::DaemonContext;
use ilert::ilert::ILert;
use ilert::ilert_builders::{HeartbeatApiResource, PingApiResource};

pub async fn run_hbt_job(daemon_ctx: Arc<DaemonContext>) -> () {
    let mut last_run = Instant::now();

    let integration_key = daemon_ctx
        .config
        .clone()
        .heartbeat_key
        .expect("Failed to access heartbeat integration key");
    let integration_key = integration_key.as_str();

    // kick off call
    ping_heartbeat(&daemon_ctx.ilert_client, integration_key).await;

    while daemon_ctx.running.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(300)).await;

        if last_run.elapsed().as_millis() < 30000 {
            continue;
        } else {
            last_run = Instant::now();
        }

        ping_heartbeat(&daemon_ctx.ilert_client, integration_key).await;
    }
}

pub async fn ping_heartbeat(ilert_client: &ILert, integration_key: &str) -> bool {
    #[allow(deprecated)]
    // heartbeat() is deprecated in ilert crate but required for legacy il1hbt keys
    let hbt_result = if integration_key.starts_with("il1hbt") {
        ilert_client
            .get()
            .heartbeat(integration_key)
            .execute()
            .await
    } else {
        ilert_client.head().ping(integration_key).execute().await
    };

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
        }
        Err(e) => {
            error!("Heartbeat http request failed {}", e);
            false
        }
    }
}

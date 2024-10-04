use std::sync::Arc;
use log::{info, warn, error};
use std::time::{Duration, Instant};

use ilert::ilert::ILert;
use ilert::ilert_builders::{EventApiResource, ILertEventType, ILertPriority};
use crate::DaemonContext;
use crate::models::event::EventQueueItemJson;
use crate::models::event_db::EventQueueItem;

pub async fn run_poll_job(daemon_ctx: Arc<DaemonContext>) -> () {

    let mut last_run = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;

        if last_run.elapsed().as_millis() < 5000 {
            continue;
        } else {
            last_run = Instant::now();
        }

        let items_result = daemon_ctx.db.lock().await.get_il_events(20);
        match items_result {
            Ok(items) => {
                if !items.is_empty() {
                    info!("Found {} queued events.", items.len());
                    process_queued_events(daemon_ctx.clone(), items).await;
                }
            },
            Err(e) => error!("Failed to fetch queued events {}", e)
        };
    }
}

async fn process_queued_events(daemon_ctx: Arc<DaemonContext>, events: Vec<EventQueueItem>) -> () {

    for event in events.iter() {
        let should_retry = process_queued_event(&daemon_ctx.ilert_client, event).await;
        let event_id = event.id.clone().unwrap_or("".to_string());
        if !should_retry {
            let del_result = daemon_ctx.db.lock().await.delete_il_event(event_id.as_str());
            match del_result {
                Ok(_) => info!("Removed event {} from queue", event_id),
                _ => warn!("Failed to remove event {} from queue",event_id)
            };
        } else {
            warn!("Failed to process event {} will retry", event_id);
        }
    }
}

pub async fn process_queued_event(ilert_client: &ILert, event: &EventQueueItem) -> bool {

    let parsed_event = EventQueueItemJson::from_db(event.clone());

    let event_id = event.id.clone().unwrap_or("".to_string());
    let event_type = ILertEventType::from_str(event.event_type.as_str());
    let event_type = match event_type {
        Ok(et) => et,
        _ => {
            error!("Failed to parse event {} with type {}", event_id, event.event_type);
            return false // broken event type, drop this event
        }
    };

    let priority : Option<ILertPriority> = match event.clone().priority {
        Some(prio_str) => {
            let parsed = ILertPriority::from_str(prio_str.as_str());
            match parsed {
                Ok(val) => Some(val),
                _ => {
                    error!("Failed to parse event {} with priority {}", event_id, prio_str);
                    return false // broken event priority, drop this event
                }
            }
        },
        None => None
    };

    let post_result = ilert_client
        .create()
        .event_with_details(
            event.api_key.as_str(),
            event_type,
            Some(event.summary.clone()),
            event.alert_key.clone(),
            event.details.clone(),
            priority,
            parsed_event.images,
            parsed_event.links,
            parsed_event.customDetails,
            None
            )
        .execute()
        .await;

    let response = match post_result {
        Ok(res) => res,
        _ => {
            error!("Network error during event post {}", event_id);
            return true // network error, retry
        }
    };

    let status = response.status.as_u16();

    if status == 202 {
        info!("Event {} post successfully delivered", event_id);
        return false; // default happy case, no retry
    }

    if status == 429 {
        warn!("Event {} post failed too many requests", event_id);
        return true; // too many requests, retry
    }

    if status > 499 {
        warn!("Event {} post failed server side exception", event_id);
        return true; // 500 exceptions, retry
    }

    warn!("Event {} post failed bad request rejection {}", event_id, status);
    error!("Response body: {}", response.body_raw.unwrap_or("No body provided".to_string()));
    false // any other status code e.g. 400, no retry
}

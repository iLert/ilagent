use ilert::ilert::ILert;
use ilert::ilert_builders::{
    AlertGetApiResource, AlertPutApiResource, BaseRequestResult, IncidentGetApiResource,
    IncidentPutApiResource,
};
use log::{debug, error, info, warn};
use std::time;

const BATCH_SIZE: i32 = 12;
const REQUEST_DELAY_MS: u64 = 200;
const MAX_RETRIES: u32 = 5;
const RETRY_BASE_MS: u64 = 1000;

async fn execute_with_retry<F, Fut>(description: &str, mut action: F) -> Option<BaseRequestResult>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<BaseRequestResult, ilert::ilert_error::ILertError>>,
{
    for attempt in 0..=MAX_RETRIES {
        match action().await {
            Ok(result) => {
                let status = result.status.as_u16();
                if status == 429 {
                    if attempt == MAX_RETRIES {
                        error!(
                            "{} — rate limited after {} retries, giving up",
                            description, MAX_RETRIES
                        );
                        return None;
                    }
                    let backoff = RETRY_BASE_MS * 2u64.pow(attempt);
                    warn!(
                        "{} — rate limited (429), retrying in {}ms",
                        description, backoff
                    );
                    tokio::time::sleep(time::Duration::from_millis(backoff)).await;
                    continue;
                }
                if status > 499 {
                    if attempt == MAX_RETRIES {
                        error!(
                            "{} — server error {} after {} retries, giving up",
                            description, status, MAX_RETRIES
                        );
                        return None;
                    }
                    let backoff = RETRY_BASE_MS * 2u64.pow(attempt);
                    warn!(
                        "{} — server error {}, retrying in {}ms",
                        description, status, backoff
                    );
                    tokio::time::sleep(time::Duration::from_millis(backoff)).await;
                    continue;
                }
                return Some(result);
            }
            Err(e) => {
                if attempt == MAX_RETRIES {
                    error!(
                        "{} — network error after {} retries: {}",
                        description, MAX_RETRIES, e
                    );
                    return None;
                }
                let backoff = RETRY_BASE_MS * 2u64.pow(attempt);
                warn!(
                    "{} — network error: {}, retrying in {}ms",
                    description, e, backoff
                );
                tokio::time::sleep(time::Duration::from_millis(backoff)).await;
            }
        }
    }
    None
}

enum ResponderCheck {
    Confirmed,
    NotFound,
    FieldMissing,
}

fn check_alert_responders(alert: &serde_json::Value, responder_ids: &[&str]) -> ResponderCheck {
    match alert.get("responders").and_then(|r| r.as_array()) {
        Some(responders) => {
            for rid in responder_ids {
                let found = responders.iter().any(|r| {
                    r.get("user")
                        .and_then(|u| u.get("id"))
                        .and_then(|id| id.as_i64())
                        .map(|id| id.to_string() == *rid)
                        .unwrap_or(false)
                });
                if !found {
                    return ResponderCheck::NotFound;
                }
            }
            ResponderCheck::Confirmed
        }
        None => ResponderCheck::FieldMissing,
    }
}

pub async fn cleanup_alerts(ilert_client: &ILert, responders: &[&str]) -> () {
    if responders.is_empty() {
        info!("Resolving all PENDING and ACCEPTED alerts...");
    } else {
        info!("Resolving alerts for responders: {:?}...", responders);
    }

    let mut index = 0;
    let mut resolved_alerts_count = 0;
    let mut skipped_alerts_count = 0;
    loop {
        let fetch_result =
            execute_with_retry(&format!("Fetch alerts (offset {})", index), || async {
                let mut request = ilert_client
                    .get()
                    .skip(index)
                    .limit(BATCH_SIZE)
                    .filter("states", "PENDING")
                    .filter("states", "ACCEPTED");

                for responder in responders {
                    request = request.filter("responders", responder);
                }

                request.alerts().execute().await
            })
            .await;

        let alerts = match fetch_result {
            Some(result) if result.status.as_u16() == 200 => result,
            Some(result) => {
                error!(
                    "Failed to fetch alerts: {}, {:?}",
                    result.status, result.body_raw
                );
                break;
            }
            None => break,
        };

        if alerts.body_json.is_none() {
            error!("Failed to get alert data");
            break;
        }

        let alerts = alerts.body_json.unwrap();
        let alerts = alerts.as_array().expect("Failed to get alert items");

        if alerts.is_empty() {
            break;
        }

        for alert in alerts.iter() {
            let alert_id = alert
                .get("id")
                .expect("Failed to get alert item")
                .as_i64()
                .expect("Failed to get alert item id");

            if !responders.is_empty() {
                match check_alert_responders(alert, responders) {
                    ResponderCheck::Confirmed => {}
                    ResponderCheck::NotFound => {
                        warn!(
                            "Skipping alert {} — responder not found in alert's responders list",
                            alert_id
                        );
                        skipped_alerts_count += 1;
                        continue;
                    }
                    ResponderCheck::FieldMissing => {
                        error!(
                            "Aborting — alert {} has no 'responders' field in API response, cannot safely verify ownership",
                            alert_id
                        );
                        info!(
                            "Resolved a total of {} alerts before aborting",
                            resolved_alerts_count
                        );
                        return;
                    }
                }
            }

            let description = format!("Resolve alert {}", alert_id);
            let resolve_result = execute_with_retry(&description, || async {
                ilert_client
                    .update()
                    .resolve_alert(alert_id)
                    .execute()
                    .await
            })
            .await;

            match resolve_result {
                Some(result) if result.status.as_u16() == 200 => {
                    debug!("Resolved alert {}", alert_id);
                    resolved_alerts_count += 1;
                }
                Some(result) => error!(
                    "Failed to resolve alert {}: status {}",
                    alert_id, result.status
                ),
                None => error!("Failed to resolve alert {} after retries", alert_id),
            }

            tokio::time::sleep(time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        }

        index = index + BATCH_SIZE as i64;
        info!("Resolved {} alerts...", resolved_alerts_count);
    }

    if skipped_alerts_count > 0 {
        warn!(
            "Skipped {} alerts that did not match the responder filter client-side",
            skipped_alerts_count
        );
    }
    warn!("Resolved a total of {} alerts", resolved_alerts_count);
    ()
}

pub async fn cleanup_incidents(ilert_client: &ILert) -> () {
    info!("Resolving all non-resolved incidents...");
    let mut index = 0;
    let mut resolved_incidents_count = 0;
    loop {
        let fetch_result =
            execute_with_retry(&format!("Fetch incidents (offset {})", index), || async {
                ilert_client
                    .get()
                    .skip(index)
                    .limit(BATCH_SIZE)
                    .filter("states", "INVESTIGATING")
                    .filter("states", "IDENTIFIED")
                    .filter("states", "MONITORING")
                    .incidents()
                    .execute()
                    .await
            })
            .await;

        let incidents = match fetch_result {
            Some(result) if result.status.as_u16() == 200 => result,
            Some(result) => {
                error!(
                    "Failed to fetch incidents: {}, {:?}",
                    result.status, result.body_raw
                );
                break;
            }
            None => break,
        };

        if incidents.body_json.is_none() {
            error!("Failed to get incident data");
            break;
        }

        let incidents = incidents.body_json.unwrap();
        let incidents = incidents.as_array().expect("Failed to get incident items");

        if incidents.is_empty() {
            break;
        }

        for incident in incidents.iter() {
            let incident_id = incident
                .get("id")
                .expect("Failed to get incident item")
                .as_i64()
                .expect("Failed to get incident item id");

            let resolve_body = serde_json::json!({
                "status": "RESOLVED"
            });

            let description = format!("Resolve incident {}", incident_id);
            let resolve_result = execute_with_retry(&description, || async {
                ilert_client
                    .update()
                    .incident_raw(incident_id, &resolve_body)
                    .execute()
                    .await
            })
            .await;

            match resolve_result {
                Some(result) if result.status.as_u16() == 200 => {
                    debug!("Resolved incident {}", incident_id);
                    resolved_incidents_count += 1;
                }
                Some(result) => error!(
                    "Failed to resolve incident {}: status {}",
                    incident_id, result.status
                ),
                None => error!("Failed to resolve incident {} after retries", incident_id),
            }

            tokio::time::sleep(time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        }

        index = index + BATCH_SIZE as i64;
        info!("Resolved {} incidents...", resolved_incidents_count);
    }

    warn!("Resolved a total of {} incidents", resolved_incidents_count);
    ()
}

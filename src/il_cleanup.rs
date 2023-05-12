use std::{thread, time};
use log::{debug, info, error};
use ilert::ilert::ILert;
use ilert::ilert_builders::{AlertGetApiResource, AlertPutApiResource};

pub fn cleanup_alerts(ilert_client: &ILert) -> () {

    info!("Resolving alerts...");
    let mut index = 0;
    let mut resolved_alerts_count = 0;
    loop {

        let alerts = ilert_client
            .get()
            .skip(index)
            .limit(12)
            .filter("states", "PENDING")
            .filter("states", "ACCEPTED")
            .alerts()
            .execute()
            .expect("Failed to fetch alerts");

        if alerts.status != 200 {
            error!("Failed to fetch alerts: {}, {:?}", alerts.status, alerts.body_raw);
            break;
        }

        // should not happen
        if alerts.body_json.is_none() {
            error!("Failed to get alert data");
            break;
        }

        let alerts = alerts.body_json.unwrap();
        let alerts = alerts.as_array().expect("Failed to get alert items");

        if alerts.is_empty() {
            break;
        }

        thread::sleep(time::Duration::from_secs(1));
        for alert in alerts.iter() {

            let alert_id = alert
                .get("id")
                .expect("Failed to get alert item")
                .as_i64()
                .expect("Failed to get alert item id");

            let resolve_result = ilert_client
                .update()
                .resolve_alert(alert_id)
                .execute()
                .expect(format!("Failed to resolve alert {}", alert_id).as_str());

            if resolve_result.status != 200 {
                error!("Failed to resolve alert {}", alert_id);
            } else {
                debug!("Resolved alert {}", alert_id);
                resolved_alerts_count = resolved_alerts_count + 1;
            }

            // dont hit API limits
            thread::sleep(time::Duration::from_secs(1));
        }

        index = index + 12;
        info!("Resolved {} alerts...", resolved_alerts_count);
    }

    info!("Resolved a total of {} alerts", resolved_alerts_count);
    ()
}
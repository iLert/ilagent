use crate::config::ILConfig;
use crate::json_util::get_nested_value;
use crate::models::event_db::EventQueueItem;
use ilert::ilert_builders::{EventImage, EventLink, EventServiceRef, ILertEventType};
use log::{debug, error, warn};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueItemJson {
    #[serde(alias = "apiKey")]
    pub integrationKey: String,
    pub eventType: String,
    pub summary: String,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>,
    pub labels: Option<HashMap<String, String>>,
    pub severity: Option<i32>,
    pub routingKey: Option<String>,
    pub services: Option<Vec<EventServiceRef>>,
}

/**
helper to apply additional consumer mappings easier
*/
#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueTransitionItemJson {
    #[serde(alias = "apiKey")]
    pub integrationKey: Option<String>,
    pub eventType: Option<String>,
    pub summary: Option<String>,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>,
    pub labels: Option<HashMap<String, String>>,
    pub severity: Option<i32>,
    pub routingKey: Option<String>,
    pub services: Option<Vec<EventServiceRef>>,
}

impl EventQueueItemJson {
    pub fn from_transition(trans: EventQueueTransitionItemJson) -> EventQueueItemJson {
        EventQueueItemJson {
            integrationKey: trans.integrationKey.unwrap_or("".to_string()),
            eventType: trans.eventType.unwrap_or("ALERT".to_string()),
            summary: trans.summary.unwrap_or("".to_string()), // field is optional for some event types
            details: trans.details,
            alertKey: trans.alertKey,
            priority: trans.priority,
            images: trans.images,
            links: trans.links,
            customDetails: trans.customDetails,
            labels: trans.labels,
            severity: trans.severity,
            routingKey: trans.routingKey,
            services: trans.services,
        }
    }

    pub fn to_db(item: EventQueueItemJson, event_api_path: Option<String>) -> EventQueueItem {
        let images = match item.images {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None,
                }
            }
            None => None,
        };

        let links = match item.links {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None,
                }
            }
            None => None,
        };

        let custom_details = match item.customDetails {
            Some(val) => Some(val.to_string()),
            None => None,
        };

        let labels = match item.labels {
            Some(v) => match serde_json::to_string(&v) {
                Ok(str) => Some(str),
                _ => None,
            },
            None => None,
        };

        let services = match item.services {
            Some(v) => match serde_json::to_string(&v) {
                Ok(str) => Some(str),
                _ => None,
            },
            None => None,
        };

        EventQueueItem {
            id: None,
            integration_key: item.integrationKey,
            event_type: item.eventType,
            alert_key: item.alertKey,
            summary: item.summary,
            details: item.details,
            created_at: None,
            priority: item.priority,
            images,
            links,
            custom_details,
            event_api_path,
            labels,
            severity: item.severity.map(|s| s as i64),
            routing_key: item.routingKey,
            services,
        }
    }

    pub fn from_db(item: EventQueueItem) -> EventQueueItemJson {
        let images: Option<Vec<EventImage>> = match item.images {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None,
                }
            }
            None => None,
        };

        let links: Option<Vec<EventLink>> = match item.links {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None,
                }
            }
            None => None,
        };

        let custom_details: Option<serde_json::Value> = match item.custom_details {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None,
                }
            }
            None => None,
        };

        let labels: Option<HashMap<String, String>> = match item.labels {
            Some(str) => match serde_json::from_str(str.as_str()) {
                Ok(v) => Some(v),
                _ => None,
            },
            None => None,
        };

        let services: Option<Vec<EventServiceRef>> = match item.services {
            Some(str) => match serde_json::from_str(str.as_str()) {
                Ok(v) => Some(v),
                _ => None,
            },
            None => None,
        };

        EventQueueItemJson {
            integrationKey: item.integration_key,
            eventType: item.event_type,
            summary: item.summary,
            details: item.details,
            alertKey: item.alert_key,
            priority: item.priority,
            images,
            links,
            customDetails: custom_details,
            labels,
            severity: item.severity.map(|s| s as i32),
            routingKey: item.routing_key,
            services,
        }
    }

    pub fn parse_event_json(
        config: &ILConfig,
        payload: &str,
        topic: &str,
    ) -> Option<EventQueueItemJson> {
        // raw parse
        let json: serde_json::Result<serde_json::Value> = serde_json::from_str(payload);
        if json.is_err() {
            warn!("Invalid consumer event payload json {}", json.unwrap_err());
            return None;
        }
        let json = json.unwrap();

        // helper container with default fields (all optional)
        let parsed: serde_json::Result<EventQueueTransitionItemJson> =
            serde_json::from_str(payload);
        if parsed.is_err() {
            error!(
                "Failed to parse event consumer payload {}",
                parsed.unwrap_err()
            );
            return None;
        }
        let mut parsed = parsed.unwrap();

        // event filter check
        if let Some(ref filter_key) = config.filter_key {
            let val_opt = json.get(filter_key);

            if val_opt.is_none() {
                debug!("Dropping event because filter key is missing");
                return None;
            }

            if let Some(ref filter_val) = config.filter_val {
                if let Some(val) = val_opt {
                    match val.as_str() {
                        Some(val_str) => {
                            if !filter_val.eq(val_str) {
                                debug!(
                                    "Dropping event because filter key value is not matching: {:?}",
                                    val_str
                                );
                                return None;
                            }
                        }
                        None => {
                            warn!(
                                "Dropping event because filter key value is not a string: {:?}",
                                val
                            );
                            return None;
                        }
                    }
                }
            }
        }

        // overwrite api key

        if let Some(ref event_key) = config.event_key {
            parsed.integrationKey = Some(event_key.clone());
        }

        // mappings

        if let Some(ref map_key_alert_key) = config.map_key_alert_key {
            if let Some(val) = get_nested_value(&json, map_key_alert_key) {
                match val.as_str() {
                    Some(s) => parsed.alertKey = Some(s.to_string()),
                    None => warn!(
                        "map_key_alert_key '{}' matched a non-string value: {:?}",
                        map_key_alert_key, val
                    ),
                }
            }
        }

        if let Some(ref map_key_summary) = config.map_key_summary {
            if let Some(val) = get_nested_value(&json, map_key_summary) {
                match val.as_str() {
                    Some(s) => parsed.summary = Some(s.to_string()),
                    None => warn!(
                        "map_key_summary '{}' matched a non-string value: {:?}",
                        map_key_summary, val
                    ),
                }
            }
        }

        let mut event_type = "".to_string();
        if let Some(ref map_key_etype) = config.map_key_etype {
            if let Some(val) = get_nested_value(&json, map_key_etype) {
                match val.as_str() {
                    Some(s) => {
                        event_type = s.to_string();
                        parsed.eventType = Some(event_type.clone());
                    }
                    None => warn!(
                        "map_key_etype '{}' matched a non-string value: {:?}",
                        map_key_etype, val
                    ),
                }
            }
        }

        if let Some(ref map_val_etype_alert) = config.map_val_etype_alert {
            if map_val_etype_alert.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::ALERT.as_str().to_string());
            }
        }

        if let Some(ref map_val_etype_accept) = config.map_val_etype_accept {
            if map_val_etype_accept.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::ACCEPT.as_str().to_string());
            }
        }

        if let Some(ref map_val_etype_resolve) = config.map_val_etype_resolve {
            if map_val_etype_resolve.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::RESOLVE.as_str().to_string());
            }
        }

        // map_key_label values override payload-native labels on key conflict
        if !config.map_key_labels.is_empty() {
            let mut labels = parsed.labels.take().unwrap_or_default();
            for token in &config.map_key_labels {
                let Some((name, path)) = token.split_once('=') else {
                    warn!(
                        "Ignoring malformed map_key_label '{}', expected name=jsonpath",
                        token
                    );
                    continue;
                };
                let name = name.trim();
                let path = path.trim();
                if name.is_empty() || path.is_empty() {
                    warn!(
                        "Ignoring malformed map_key_label '{}', expected name=jsonpath",
                        token
                    );
                    continue;
                }
                if let Some(val) = get_nested_value(&json, path) {
                    match val.as_str() {
                        Some(s) => {
                            labels.insert(name.to_string(), s.to_string());
                        }
                        None => warn!(
                            "map_key_label '{}' matched a non-string value: {:?}",
                            path, val
                        ),
                    }
                }
            }
            if !labels.is_empty() {
                parsed.labels = Some(labels);
            }
        }

        // a configured mapped source is authoritative: an invalid value at the path clears
        // severity rather than falling back to the payload-native value
        if let Some(ref map_key_severity) = config.map_key_severity {
            if let Some(val) = get_nested_value(&json, map_key_severity) {
                let extracted = match val {
                    serde_json::Value::Number(n) => n.as_i64(),
                    serde_json::Value::String(s) => s.parse::<i64>().ok(),
                    _ => None,
                };
                match extracted {
                    Some(n) if (1..=5).contains(&n) => parsed.severity = Some(n as i32),
                    Some(n) => {
                        warn!(
                            "map_key_severity '{}' value {} is out of range 1..=5, dropping severity",
                            map_key_severity, n
                        );
                        parsed.severity = None;
                    }
                    None => {
                        warn!(
                            "map_key_severity '{}' matched a non-integer value: {:?}, dropping severity",
                            map_key_severity, val
                        );
                        parsed.severity = None;
                    }
                }
            }
        }

        // validate any severity (payload-native or mapped) — drop out-of-range, never fail the event
        if let Some(sev) = parsed.severity {
            if !(1..=5).contains(&sev) {
                warn!("Dropping severity {}, value is out of range 1..=5", sev);
                parsed.severity = None;
            }
        }

        if let Some(ref map_key_routing_key) = config.map_key_routing_key {
            if let Some(val) = get_nested_value(&json, map_key_routing_key) {
                match val.as_str() {
                    Some(s) => parsed.routingKey = Some(s.to_string()),
                    None => warn!(
                        "map_key_routing_key '{}' matched a non-string value: {:?}",
                        map_key_routing_key, val
                    ),
                }
            }
        }

        // try to save empty summary on alert events
        if parsed.summary.is_none()
            && parsed
                .eventType
                .clone()
                .unwrap_or(ILertEventType::ALERT.as_str().to_string())
                .eq(ILertEventType::ALERT.as_str())
        {
            parsed.summary = Some(format!("New alert from {}", topic).to_string());
        }

        debug!("Mapped event transition object: {:?}", parsed);
        Some(EventQueueItemJson::from_transition(parsed))
    }
}

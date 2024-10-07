use ilert::ilert_builders::{EventImage, EventLink, ILertEventType};
use log::{debug, error, warn};
use serde_derive::{Deserialize, Serialize};
use crate::config::ILConfig;
use crate::models::event_db::EventQueueItem;

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueItemJson {
    pub apiKey: String,
    pub eventType: String,
    pub summary: String,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>
}

/**
helper to apply additional consumer mappings easier
*/
#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueTransitionItemJson {
    pub apiKey: Option<String>,
    pub eventType: Option<String>,
    pub summary: Option<String>,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>
}

impl EventQueueItemJson {

    pub fn from_transition(trans: EventQueueTransitionItemJson) -> EventQueueItemJson {
        EventQueueItemJson {
            apiKey: trans.apiKey.unwrap_or("".to_string()),
            eventType: trans.eventType.unwrap_or("ALERT".to_string()),
            summary: trans.summary.unwrap_or("".to_string()), // field is optional for some event types
            details: trans.details,
            alertKey: trans.alertKey,
            priority: trans.priority,
            images: trans.images,
            links: trans.links,
            customDetails: trans.customDetails
        }
    }

    pub fn to_db(item: EventQueueItemJson, event_api_path: Option<String>) -> EventQueueItem {

        let images = match item.images {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None
                }
            },
            None => None
        };

        let links = match item.links {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None
                }
            },
            None => None
        };

        let custom_details = match item.customDetails {
            Some(val) => Some(val.to_string()),
            None => None
        };

        EventQueueItem {
            id: None,
            api_key: item.apiKey,
            event_type: item.eventType,
            alert_key: item.alertKey,
            summary: item.summary,
            details: item.details,
            created_at: None,
            priority: item.priority,
            images,
            links,
            custom_details,
            event_api_path
        }
    }

    pub fn from_db(item: EventQueueItem) -> EventQueueItemJson {

        let images : Option<Vec<EventImage>> = match item.images {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        let links : Option<Vec<EventLink>> = match item.links {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        let custom_details : Option<serde_json::Value> = match item.custom_details {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        EventQueueItemJson {
            apiKey: item.api_key,
            eventType: item.event_type,
            summary: item.summary,
            details: item.details,
            alertKey: item.alert_key,
            priority: item.priority,
            images,
            links,
            customDetails: custom_details
        }
    }

    pub fn parse_event_json(config: &ILConfig, payload: &str, topic: &str) -> Option<EventQueueItemJson> {

        // raw parse
        let json : serde_json::Result<serde_json::Value> = serde_json::from_str(payload);
        if json.is_err() {
            warn!("Invalid consumer event payload json {}", json.unwrap_err());
            return None;
        }
        let json = json.unwrap();

        // helper container with default fields (all optional)
        let parsed: serde_json::Result<EventQueueTransitionItemJson> = serde_json::from_str(payload);
        if parsed.is_err() {
            error!("Failed to parse event consumer payload {}", parsed.unwrap_err());
            return None;
        }
        let mut parsed = parsed.unwrap();

        let config = config.clone();

        // event filter check
        if let Some(filter_key) = config.filter_key {
            let val_opt = json.get(filter_key);

            if val_opt.is_none() {
                debug!("Dropping event because filter key is missing");
                return None;
            }

            if let Some(filter_val) = config.filter_val {
                let val = val_opt.expect("failed to unwrap event filter val opt");
                if let Some(val) = val.as_str() {
                    if !filter_val.eq(val) {
                        debug!("Dropping event because filter key value is not matching: {:?}", val);
                        return None;
                    }
                }
            }
        }

        // overwrite api key

        if let Some(event_key) = config.event_key {
            parsed.apiKey = Some(event_key.clone());
        }

        // mappings

        if let Some(map_key_alert_key) = config.map_key_alert_key {
            let val_opt = json.get(map_key_alert_key);
            if let Some(val) = val_opt {
                if let Some(val) = val.as_str() {
                    parsed.alertKey = Some(val.to_string());
                }
            }
        }

        if let Some(map_key_summary) = config.map_key_summary {
            let val_opt = json.get(map_key_summary);
            if let Some(val) = val_opt {
                if let Some(val) = val.as_str() {
                    parsed.summary = Some(val.to_string());
                }
            }
        }

        let mut event_type = "".to_string();
        if let Some(map_key_etype) = config.map_key_etype {
            let val_opt = json.get(map_key_etype);
            if let Some(val) = val_opt {
                if let Some(val) = val.as_str() {
                    event_type = val.to_string();
                    parsed.eventType = Some(event_type.clone());
                }
            }
        }

        if let Some(map_val_etype_alert) = config.map_val_etype_alert {
            if map_val_etype_alert.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::ALERT.as_str().to_string());
            }
        }

        if let Some(map_val_etype_accept) = config.map_val_etype_accept {
            if map_val_etype_accept.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::ACCEPT.as_str().to_string());
            }
        }

        if let Some(map_val_etype_resolve) = config.map_val_etype_resolve {
            if map_val_etype_resolve.eq(event_type.as_str()) {
                parsed.eventType = Some(ILertEventType::RESOLVE.as_str().to_string());
            }
        }

        // try to save empty summary on alert events
        if parsed.summary.is_none()
            && parsed.eventType.clone().unwrap_or(ILertEventType::ALERT.as_str().to_string())
            .eq(ILertEventType::ALERT.as_str()) {
            parsed.summary = Some(format!("New alert from {}", topic).to_string());
        }

        debug!("Mapped event transition object: {:?}", parsed);
        Some(EventQueueItemJson::from_transition(parsed))
    }
}
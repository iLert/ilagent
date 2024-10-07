use ilert::ilert_builders::ILertEventType;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueItem {
    pub id: Option<String>,
    pub api_key: String,
    pub event_type: String,
    pub alert_key: Option<String>,
    pub summary: String,
    pub details: Option<String>,
    pub created_at: Option<String>,
    pub priority: Option<String>,
    pub images: Option<String>,
    pub links: Option<String>,
    pub custom_details: Option<String>,
    pub event_api_path: Option<String>
}

impl EventQueueItem {

    pub fn new() -> EventQueueItem {
        EventQueueItem {
            id: None,
            api_key: "".to_string(),
            event_type: ILertEventType::ALERT.as_str().to_string(),
            alert_key: None,
            summary: "".to_string(),
            details: None,
            created_at: None,
            priority: None,
            images: None,
            links: None,
            custom_details: None,
            event_api_path: None
        }
    }

    pub fn new_with_required(api_key: &str, event_type: &str, summary: &str,
                             alert_key: Option<String>) -> EventQueueItem {
        EventQueueItem {
            id: None,
            api_key: api_key.to_string(),
            event_type: event_type.to_string(),
            alert_key,
            summary: summary.to_string(),
            details: None,
            created_at: None,
            priority: None,
            images: None,
            links: None,
            custom_details: None,
            event_api_path: None
        }
    }
}
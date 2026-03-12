pub mod kafka;
pub mod mqtt;

use crate::config::ILConfig;
use crate::models::event::EventQueueItemJson;

pub fn prepare_consumer_event(config: &ILConfig, payload: &str, topic: &str, default_details: serde_json::Value) -> Option<EventQueueItemJson> {
    let mut event = EventQueueItemJson::parse_event_json(config, payload, topic)?;
    if event.customDetails.is_none() {
        event.customDetails = Some(default_details);
    }
    Some(event)
}

pub fn build_event_api_path(consumer_type: &str, api_key: &str) -> String {
    format!("/v1/events/{}/{}", consumer_type, api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_path_mqtt() {
        assert_eq!(build_event_api_path("mqtt", "key1"), "/v1/events/mqtt/key1");
    }

    #[test]
    fn build_path_kafka() {
        assert_eq!(build_event_api_path("kafka", "key1"), "/v1/events/kafka/key1");
    }

    #[test]
    fn prepare_event_injects_custom_details_when_missing() {
        let config = ILConfig::new();
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test"}"#;
        let details = serde_json::json!({"topic": "t1", "extra": "val"});
        let event = prepare_consumer_event(&config, payload, "t1", details).unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["topic"], "t1");
        assert_eq!(cd["extra"], "val");
    }

    #[test]
    fn prepare_event_preserves_existing_custom_details() {
        let config = ILConfig::new();
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test", "customDetails": {"env": "prod"}}"#;
        let details = serde_json::json!({"topic": "t1"});
        let event = prepare_consumer_event(&config, payload, "t1", details).unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["env"], "prod");
        assert!(cd.get("topic").is_none());
    }

    #[test]
    fn prepare_event_returns_none_for_invalid_payload() {
        let config = ILConfig::new();
        assert!(prepare_consumer_event(&config, "bad", "t1", serde_json::json!({})).is_none());
    }
}

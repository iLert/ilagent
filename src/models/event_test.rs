#[cfg(test)]
mod tests {
    use crate::config::ILConfig;
    use crate::models::event::{EventQueueItemJson, EventQueueTransitionItemJson};

    fn default_config() -> ILConfig {
        ILConfig::new()
    }

    // --- parse_event_json: basic parsing ---

    #[test]
    fn parse_valid_event() {
        let config = default_config();
        let payload = r#"{"apiKey": "il1api123", "eventType": "ALERT", "summary": "Server down"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.integrationKey, "il1api123");
        assert_eq!(event.eventType, "ALERT");
        assert_eq!(event.summary, "Server down");
    }

    #[test]
    fn parse_event_invalid_json() {
        let config = default_config();
        let result = EventQueueItemJson::parse_event_json(&config, "not json", "ilert/events");
        assert!(result.is_none());
    }

    #[test]
    fn parse_event_empty_payload() {
        let config = default_config();
        let result = EventQueueItemJson::parse_event_json(&config, "", "ilert/events");
        assert!(result.is_none());
    }

    #[test]
    fn parse_event_minimal_payload() {
        let config = default_config();
        // all fields optional in transition struct, should produce defaults
        let payload = r#"{}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.integrationKey, "");
        assert_eq!(event.eventType, "ALERT");
    }

    #[test]
    fn parse_event_with_all_fields() {
        let config = default_config();
        let payload = r#"{
            "apiKey": "key1",
            "eventType": "RESOLVE",
            "summary": "Resolved",
            "details": "Detail text",
            "alertKey": "alert-123",
            "priority": "HIGH",
            "customDetails": {"env": "prod"}
        }"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.integrationKey, "key1");
        assert_eq!(event.eventType, "RESOLVE");
        assert_eq!(event.summary, "Resolved");
        assert_eq!(event.details.unwrap(), "Detail text");
        assert_eq!(event.alertKey.unwrap(), "alert-123");
        assert_eq!(event.priority.unwrap(), "HIGH");
        assert!(event.customDetails.is_some());
    }

    // --- parse_event_json: event_key overwrite ---

    #[test]
    fn parse_event_overwrites_api_key() {
        let mut config = default_config();
        config.event_key = Some("static-key".to_string());
        let payload = r#"{"apiKey": "original-key", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().integrationKey, "static-key");
    }

    // --- parse_event_json: field mappings ---

    #[test]
    fn parse_event_maps_custom_summary_key() {
        let mut config = default_config();
        config.map_key_summary = Some("comment".to_string());
        let payload = r#"{"apiKey": "k1", "comment": "My custom summary"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().summary, "My custom summary");
    }

    #[test]
    fn parse_event_maps_custom_alert_key() {
        let mut config = default_config();
        config.map_key_alert_key = Some("mCode".to_string());
        let payload = r#"{"apiKey": "k1", "mCode": "CODE-42"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().alertKey.unwrap(), "CODE-42");
    }

    #[test]
    fn parse_event_maps_custom_event_type_key() {
        let mut config = default_config();
        config.map_key_etype = Some("state".to_string());
        let payload = r#"{"apiKey": "k1", "state": "RESOLVE"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "RESOLVE");
    }

    // --- parse_event_json: value mappings ---

    #[test]
    fn parse_event_maps_value_to_alert() {
        let mut config = default_config();
        config.map_key_etype = Some("state".to_string());
        config.map_val_etype_alert = Some("SET".to_string());
        let payload = r#"{"apiKey": "k1", "state": "SET"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "ALERT");
    }

    #[test]
    fn parse_event_maps_value_to_accept() {
        let mut config = default_config();
        config.map_key_etype = Some("state".to_string());
        config.map_val_etype_accept = Some("ACK".to_string());
        let payload = r#"{"apiKey": "k1", "state": "ACK"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "ACCEPT");
    }

    #[test]
    fn parse_event_maps_value_to_resolve() {
        let mut config = default_config();
        config.map_key_etype = Some("state".to_string());
        config.map_val_etype_resolve = Some("CLR".to_string());
        let payload = r#"{"apiKey": "k1", "state": "CLR"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "RESOLVE");
    }

    #[test]
    fn parse_event_value_mapping_no_match_keeps_original() {
        let mut config = default_config();
        config.map_key_etype = Some("state".to_string());
        config.map_val_etype_alert = Some("SET".to_string());
        let payload = r#"{"apiKey": "k1", "state": "UNKNOWN"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "UNKNOWN");
    }

    // --- parse_event_json: combined key + value mappings ---

    #[test]
    fn parse_event_full_mapping_pipeline() {
        let mut config = default_config();
        config.event_key = Some("static-api-key".to_string());
        config.map_key_etype = Some("state".to_string());
        config.map_key_alert_key = Some("mCode".to_string());
        config.map_key_summary = Some("comment".to_string());
        config.map_val_etype_alert = Some("SET".to_string());
        config.map_val_etype_accept = Some("ACK".to_string());
        config.map_val_etype_resolve = Some("CLR".to_string());
        let payload = r#"{"state": "SET", "mCode": "M-100", "comment": "Pump failure"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "factory/alarms");
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.integrationKey, "static-api-key");
        assert_eq!(event.eventType, "ALERT");
        assert_eq!(event.alertKey.unwrap(), "M-100");
        assert_eq!(event.summary, "Pump failure");
    }

    // --- parse_event_json: filters ---

    #[test]
    fn parse_event_filter_key_present_passes() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        let payload = r#"{"apiKey": "k1", "type": "ALARM", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
    }

    #[test]
    fn parse_event_filter_key_missing_drops() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_none());
    }

    #[test]
    fn parse_event_filter_key_and_value_match_passes() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload = r#"{"apiKey": "k1", "type": "ALARM", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
    }

    #[test]
    fn parse_event_filter_key_and_value_mismatch_drops() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload = r#"{"apiKey": "k1", "type": "INFO", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_none());
    }

    // --- parse_event_json: default summary fallback ---

    #[test]
    fn parse_event_alert_without_summary_gets_default() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "factory/sensors");
        assert!(result.is_some());
        assert_eq!(result.unwrap().summary, "New alert from factory/sensors");
    }

    #[test]
    fn parse_event_resolve_without_summary_stays_empty() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "eventType": "RESOLVE"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
        assert_eq!(result.unwrap().summary, "");
    }

    // --- from_transition ---

    #[test]
    fn from_transition_defaults() {
        let trans = EventQueueTransitionItemJson {
            integrationKey: None,
            eventType: None,
            summary: None,
            details: None,
            alertKey: None,
            priority: None,
            images: None,
            links: None,
            customDetails: None,
        };
        let event = EventQueueItemJson::from_transition(trans);
        assert_eq!(event.integrationKey, "");
        assert_eq!(event.eventType, "ALERT");
        assert_eq!(event.summary, "");
    }

    // --- to_db / from_db round-trip ---

    #[test]
    fn to_db_and_from_db_round_trip() {
        let original = EventQueueItemJson {
            integrationKey: "key1".to_string(),
            eventType: "ALERT".to_string(),
            summary: "Something broke".to_string(),
            details: Some("details here".to_string()),
            alertKey: Some("alert-42".to_string()),
            priority: Some("HIGH".to_string()),
            images: None,
            links: None,
            customDetails: Some(serde_json::json!({"env": "prod", "host": "srv1"})),
        };

        let db_item = EventQueueItemJson::to_db(original.clone(), Some("/v1/events/mqtt/key1".to_string()));
        assert_eq!(db_item.integration_key, "key1");
        assert_eq!(db_item.event_api_path.as_ref().unwrap(), "/v1/events/mqtt/key1");

        let restored = EventQueueItemJson::from_db(db_item);
        assert_eq!(restored.integrationKey, "key1");
        assert_eq!(restored.eventType, "ALERT");
        assert_eq!(restored.summary, "Something broke");
        assert_eq!(restored.details.unwrap(), "details here");
        assert_eq!(restored.alertKey.unwrap(), "alert-42");
        assert_eq!(restored.priority.unwrap(), "HIGH");
        let cd = restored.customDetails.unwrap();
        assert_eq!(cd["env"], "prod");
        assert_eq!(cd["host"], "srv1");
    }
}

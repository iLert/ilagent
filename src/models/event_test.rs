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

    // --- parse_event_json: dot-notation mapping ---

    #[test]
    fn parse_event_maps_nested_summary_key() {
        let mut config = default_config();
        config.event_key = Some("k1".to_string());
        config.map_key_summary = Some("data.message".to_string());
        let payload = r#"{"data": {"message": "Nested summary"}}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "t1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().summary, "Nested summary");
    }

    #[test]
    fn parse_event_maps_nested_alert_key() {
        let mut config = default_config();
        config.event_key = Some("k1".to_string());
        config.map_key_alert_key = Some("meta.code".to_string());
        let payload = r#"{"meta": {"code": "M-200"}}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "t1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().alertKey.unwrap(), "M-200");
    }

    #[test]
    fn parse_event_maps_nested_event_type_key() {
        let mut config = default_config();
        config.event_key = Some("k1".to_string());
        config.map_key_etype = Some("status.type".to_string());
        let payload = r#"{"status": {"type": "RESOLVE"}}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "t1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().eventType, "RESOLVE");
    }

    #[test]
    fn parse_event_nested_mapping_missing_path_graceful() {
        let mut config = default_config();
        config.event_key = Some("k1".to_string());
        config.map_key_summary = Some("data.nonexistent.deep".to_string());
        let payload = r#"{"data": {"other": "value"}}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "t1");
        assert!(result.is_some());
        // summary falls back to topic-based default
        assert_eq!(result.unwrap().summary, "New alert from t1");
    }

    // --- parse_event_json: filters ---

    #[test]
    fn parse_event_filter_key_present_passes() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        let payload =
            r#"{"apiKey": "k1", "type": "ALARM", "eventType": "ALERT", "summary": "test"}"#;
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
        let payload =
            r#"{"apiKey": "k1", "type": "ALARM", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_some());
    }

    #[test]
    fn parse_event_filter_key_and_value_mismatch_drops() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload =
            r#"{"apiKey": "k1", "type": "INFO", "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(result.is_none());
    }

    // --- parse_event_json: filter with non-string values ---

    #[test]
    fn parse_event_filter_non_string_value_drops() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        config.filter_val = Some("ALARM".to_string());
        let payload = r#"{"apiKey": "k1", "type": 123, "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(
            result.is_none(),
            "non-string filter value should be rejected"
        );
    }

    #[test]
    fn parse_event_filter_key_only_non_string_passes() {
        let mut config = default_config();
        config.filter_key = Some("type".to_string());
        // no filter_val set, just checking key existence
        let payload = r#"{"apiKey": "k1", "type": 123, "eventType": "ALERT", "summary": "test"}"#;
        let result = EventQueueItemJson::parse_event_json(&config, payload, "ilert/events");
        assert!(
            result.is_some(),
            "key-only filter should pass when key exists regardless of type"
        );
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
            labels: None,
            severity: None,
            routingKey: None,
            services: None,
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
            labels: None,
            severity: None,
            routingKey: None,
            services: None,
        };

        let db_item =
            EventQueueItemJson::to_db(original.clone(), Some("/v1/events/mqtt/key1".to_string()));
        assert_eq!(db_item.integration_key, "key1");
        assert_eq!(
            db_item.event_api_path.as_ref().unwrap(),
            "/v1/events/mqtt/key1"
        );

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

    // --- labels ---

    #[test]
    fn parse_event_payload_native_labels() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "summary": "s", "labels": {"env": "prod", "dc": "eu"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        let labels = event.labels.unwrap();
        assert_eq!(labels.get("env").unwrap(), "prod");
        assert_eq!(labels.get("dc").unwrap(), "eu");
    }

    #[test]
    fn parse_event_map_key_label_extracts_and_merges() {
        let mut config = default_config();
        config.map_key_labels = vec!["region=data.region".to_string()];
        let payload = r#"{"apiKey": "k1", "summary": "s", "labels": {"env": "prod"}, "data": {"region": "eu-central-1"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        let labels = event.labels.unwrap();
        assert_eq!(labels.get("env").unwrap(), "prod");
        assert_eq!(labels.get("region").unwrap(), "eu-central-1");
    }

    #[test]
    fn parse_event_map_key_label_overrides_payload_on_conflict() {
        let mut config = default_config();
        config.map_key_labels = vec!["env=data.realEnv".to_string()];
        let payload = r#"{"apiKey": "k1", "summary": "s", "labels": {"env": "stale"}, "data": {"realEnv": "prod"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.labels.unwrap().get("env").unwrap(), "prod");
    }

    #[test]
    fn parse_event_map_key_label_non_string_skipped() {
        let mut config = default_config();
        config.map_key_labels = vec!["count=data.count".to_string()];
        let payload = r#"{"apiKey": "k1", "summary": "s", "data": {"count": 5}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert!(event.labels.is_none());
    }

    // --- severity ---

    #[test]
    fn parse_event_payload_native_severity() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 3}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.severity.unwrap(), 3);
    }

    #[test]
    fn parse_event_map_key_severity_from_number() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "data": {"sev": 4}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.severity.unwrap(), 4);
    }

    #[test]
    fn parse_event_map_key_severity_from_string() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "data": {"sev": "2"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.severity.unwrap(), 2);
    }

    #[test]
    fn parse_event_map_key_severity_out_of_range_dropped() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "data": {"sev": 9}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert!(event.severity.is_none());
    }

    #[test]
    fn parse_event_payload_native_severity_out_of_range_dropped() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 0}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert!(event.severity.is_none());
    }

    #[test]
    fn parse_event_map_key_severity_valid_overrides_native() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 2, "data": {"sev": 4}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.severity.unwrap(), 4);
    }

    #[test]
    fn parse_event_map_key_severity_out_of_range_clears_native() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 2, "data": {"sev": 9}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert!(
            event.severity.is_none(),
            "invalid mapped severity must clear the payload-native value, not fall back to it"
        );
    }

    #[test]
    fn parse_event_map_key_severity_non_integer_clears_native() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 2, "data": {"sev": "high"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert!(event.severity.is_none());
    }

    #[test]
    fn parse_event_map_key_severity_absent_path_keeps_native() {
        let mut config = default_config();
        config.map_key_severity = Some("data.sev".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "severity": 2, "data": {}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.severity.unwrap(), 2);
    }

    // --- routingKey ---

    #[test]
    fn parse_event_payload_native_routing_key() {
        let config = default_config();
        let payload = r#"{"apiKey": "k1", "summary": "s", "routingKey": "team-alpha"}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.routingKey.unwrap(), "team-alpha");
    }

    #[test]
    fn parse_event_map_key_routing_key_extracts() {
        let mut config = default_config();
        config.map_key_routing_key = Some("data.team".to_string());
        let payload = r#"{"apiKey": "k1", "summary": "s", "data": {"team": "team-beta"}}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        assert_eq!(event.routingKey.unwrap(), "team-beta");
    }

    // --- services (payload-native) ---

    #[test]
    fn parse_event_payload_native_services() {
        let config = default_config();
        let payload =
            r#"{"apiKey": "k1", "summary": "s", "services": [{"alias": "web"}, {"id": 42}]}"#;
        let event = EventQueueItemJson::parse_event_json(&config, payload, "t1").unwrap();
        let services = event.services.unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].alias.as_ref().unwrap(), "web");
        assert_eq!(services[1].id.unwrap(), 42);
    }

    // --- to_db / from_db round-trip for new fields ---

    #[test]
    fn to_db_and_from_db_round_trip_new_fields() {
        use ilert::ilert_builders::EventServiceRef;
        use std::collections::HashMap;

        let mut labels = HashMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        labels.insert("dc".to_string(), "eu".to_string());

        let original = EventQueueItemJson {
            integrationKey: "key1".to_string(),
            eventType: "ALERT".to_string(),
            summary: "broke".to_string(),
            details: None,
            alertKey: None,
            priority: None,
            images: None,
            links: None,
            customDetails: None,
            labels: Some(labels),
            severity: Some(3),
            routingKey: Some("team-x".to_string()),
            services: Some(vec![
                EventServiceRef::new("web"),
                EventServiceRef::new_with_id(7),
            ]),
        };

        let db_item = EventQueueItemJson::to_db(original.clone(), None);
        assert_eq!(db_item.severity.unwrap(), 3);
        assert_eq!(db_item.routing_key.as_ref().unwrap(), "team-x");
        assert!(db_item.labels.is_some());
        assert!(db_item.services.is_some());

        let restored = EventQueueItemJson::from_db(db_item);
        let restored_labels = restored.labels.unwrap();
        assert_eq!(restored_labels.get("env").unwrap(), "prod");
        assert_eq!(restored_labels.get("dc").unwrap(), "eu");
        assert_eq!(restored.severity.unwrap(), 3);
        assert_eq!(restored.routingKey.unwrap(), "team-x");
        let restored_services = restored.services.unwrap();
        assert_eq!(restored_services.len(), 2);
        assert_eq!(restored_services[0].alias.as_ref().unwrap(), "web");
        assert_eq!(restored_services[1].id.unwrap(), 7);
    }
}

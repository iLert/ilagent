pub mod kafka;
pub mod mqtt;
pub mod policy;

use crate::config::ILConfig;
use crate::models::event::EventQueueItemJson;
use ilert::ilert_builders::EventServiceRef;
use log::warn;

pub fn prepare_consumer_event(
    config: &ILConfig,
    payload: &str,
    topic: &str,
    default_details: serde_json::Value,
) -> Option<EventQueueItemJson> {
    let mut event = EventQueueItemJson::parse_event_json(config, payload, topic)?;
    if event.customDetails.is_none() {
        if config.forward_message_payload {
            let payload_json: serde_json::Value =
                serde_json::from_str(payload).unwrap_or(default_details.clone());
            event.customDetails = Some(payload_json);
        } else {
            event.customDetails = Some(default_details);
        }
    }
    enrich_event(config, &mut event);
    Some(event)
}

/// Parse a `key=value` token, trimming the key. Returns None for an empty key
/// or a token without a `=` separator. The value keeps any inner `=` characters.
fn parse_kv(token: &str) -> Option<(String, String)> {
    let (key, value) = token.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Parse a `--service` token of the form `alias=NAME` or `id=NUM` into an `EventServiceRef`.
fn parse_service_ref(token: &str) -> Option<EventServiceRef> {
    let (kind, value) = token.split_once('=')?;
    let value = value.trim();
    match kind.trim() {
        "alias" => {
            if value.is_empty() {
                None
            } else {
                Some(EventServiceRef::new(value))
            }
        }
        "id" => value.parse::<i64>().ok().map(EventServiceRef::new_with_id),
        _ => None,
    }
}

/// Apply operator-supplied static enrichment that does not depend on the payload:
/// static labels (operator config wins on key conflict), a severity fallback (only
/// when the event has none), and static service refs (appended to payload services).
///
/// This runs for every ingestion path — consumers and the HTTP endpoint — so that
/// the HTTP bypass of `parse_event_json` still receives the same static config.
pub fn enrich_event(config: &ILConfig, event: &mut EventQueueItemJson) {
    // static labels — operator config overrides any payload/mapped label on conflict
    if !config.static_labels.is_empty() {
        let mut labels = event.labels.take().unwrap_or_default();
        for token in &config.static_labels {
            match parse_kv(token) {
                Some((key, value)) => {
                    labels.insert(key, value);
                }
                None => warn!(
                    "Ignoring malformed --label '{}', expected key=value",
                    token
                ),
            }
        }
        if !labels.is_empty() {
            event.labels = Some(labels);
        }
    }

    // severity — apply the static fallback only when the event carries none
    if event.severity.is_none() {
        if let Some(severity) = config.severity {
            event.severity = Some(severity);
        }
    }

    // static services — appended to any payload-native services
    if !config.static_services.is_empty() {
        let mut services = event.services.take().unwrap_or_default();
        for token in &config.static_services {
            match parse_service_ref(token) {
                Some(service) => services.push(service),
                None => warn!(
                    "Ignoring malformed --service '{}', expected alias=NAME or id=NUM",
                    token
                ),
            }
        }
        if !services.is_empty() {
            event.services = Some(services);
        }
    }
}

pub fn build_event_api_path(consumer_type: &str, integration_key: &str) -> String {
    format!("/v1/events/{}/{}", consumer_type, integration_key)
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
        assert_eq!(
            build_event_api_path("kafka", "key1"),
            "/v1/events/kafka/key1"
        );
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

    #[test]
    fn forward_payload_stores_full_json() {
        let mut config = ILConfig::new();
        config.forward_message_payload = true;
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test", "extra": "data", "nested": {"val": 42}}"#;
        let details = serde_json::json!({"topic": "t1"});
        let event = prepare_consumer_event(&config, payload, "t1", details).unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["extra"], "data");
        assert_eq!(cd["nested"]["val"], 42);
        assert!(
            cd.get("topic").is_none(),
            "metadata should not be merged into forwarded payload"
        );
    }

    #[test]
    fn forward_payload_preserves_explicit_custom_details() {
        let mut config = ILConfig::new();
        config.forward_message_payload = true;
        let payload = r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test", "customDetails": {"env": "prod"}}"#;
        let details = serde_json::json!({"topic": "t1"});
        let event = prepare_consumer_event(&config, payload, "t1", details).unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["env"], "prod");
        assert!(
            cd.get("topic").is_none(),
            "explicit customDetails should be preserved as-is"
        );
    }

    #[test]
    fn forward_payload_disabled_uses_default_details() {
        let mut config = ILConfig::new();
        config.forward_message_payload = false;
        let payload =
            r#"{"apiKey": "k1", "eventType": "ALERT", "summary": "test", "extra": "data"}"#;
        let details = serde_json::json!({"topic": "t1"});
        let event = prepare_consumer_event(&config, payload, "t1", details).unwrap();
        let cd = event.customDetails.unwrap();
        assert_eq!(cd["topic"], "t1");
        assert!(
            cd.get("extra").is_none(),
            "original payload fields should not leak when flag is off"
        );
    }

    // --- enrich_event ---

    fn empty_event() -> EventQueueItemJson {
        EventQueueItemJson {
            integrationKey: "k1".to_string(),
            eventType: "ALERT".to_string(),
            summary: "s".to_string(),
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
        }
    }

    #[test]
    fn enrich_adds_static_labels() {
        let mut config = ILConfig::new();
        config.static_labels = vec!["env=prod".to_string(), "dc=eu-central-1".to_string()];
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        let labels = event.labels.unwrap();
        assert_eq!(labels.get("env").unwrap(), "prod");
        assert_eq!(labels.get("dc").unwrap(), "eu-central-1");
    }

    #[test]
    fn enrich_static_label_overrides_payload_on_conflict() {
        let mut config = ILConfig::new();
        config.static_labels = vec!["env=prod".to_string()];
        let mut event = empty_event();
        let mut existing = std::collections::HashMap::new();
        existing.insert("env".to_string(), "stale".to_string());
        existing.insert("keep".to_string(), "yes".to_string());
        event.labels = Some(existing);
        enrich_event(&config, &mut event);
        let labels = event.labels.unwrap();
        // operator config wins on conflict
        assert_eq!(labels.get("env").unwrap(), "prod");
        // non-conflicting payload label preserved
        assert_eq!(labels.get("keep").unwrap(), "yes");
    }

    #[test]
    fn enrich_malformed_label_skipped() {
        let mut config = ILConfig::new();
        config.static_labels = vec!["noequals".to_string(), "=novalue".to_string(), "ok=1".to_string()];
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        let labels = event.labels.unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("ok").unwrap(), "1");
    }

    #[test]
    fn enrich_label_value_keeps_inner_equals() {
        let mut config = ILConfig::new();
        config.static_labels = vec!["query=a=b".to_string()];
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        assert_eq!(event.labels.unwrap().get("query").unwrap(), "a=b");
    }

    #[test]
    fn enrich_severity_fallback_only_when_absent() {
        let mut config = ILConfig::new();
        config.severity = Some(4);

        // applied when none
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        assert_eq!(event.severity.unwrap(), 4);

        // not applied when already set
        let mut event2 = empty_event();
        event2.severity = Some(2);
        enrich_event(&config, &mut event2);
        assert_eq!(event2.severity.unwrap(), 2);
    }

    #[test]
    fn enrich_adds_static_services_alias_and_id() {
        let mut config = ILConfig::new();
        config.static_services = vec!["alias=web-frontend".to_string(), "id=123".to_string()];
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        let services = event.services.unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].alias.as_ref().unwrap(), "web-frontend");
        assert_eq!(services[1].id.unwrap(), 123);
    }

    #[test]
    fn enrich_static_services_appended_to_payload() {
        let mut config = ILConfig::new();
        config.static_services = vec!["alias=db".to_string()];
        let mut event = empty_event();
        event.services = Some(vec![EventServiceRef::new("web")]);
        enrich_event(&config, &mut event);
        let services = event.services.unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].alias.as_ref().unwrap(), "web");
        assert_eq!(services[1].alias.as_ref().unwrap(), "db");
    }

    #[test]
    fn enrich_bad_service_id_skipped() {
        let mut config = ILConfig::new();
        config.static_services =
            vec!["id=notanumber".to_string(), "bogus".to_string(), "id=5".to_string()];
        let mut event = empty_event();
        enrich_event(&config, &mut event);
        let services = event.services.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id.unwrap(), 5);
    }
}

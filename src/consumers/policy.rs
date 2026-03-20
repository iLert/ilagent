use log::{info, warn, error, debug};
use serde_json::Value;
use ilert::ilert::ILert;

use crate::config::ILConfig;

const DEFAULT_EMAIL_PATH: &str = "data.email";
const DEFAULT_SHIFT_PATH: &str = "data.shift";

pub fn get_nested_value<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = json;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}

pub fn parse_policy_payload(config: &ILConfig, payload: &str) -> Option<Value> {
    let json: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            warn!("Invalid policy message payload json {}", e);
            return None;
        }
    };

    // apply filter (reuse filter_key / filter_val from config)
    if let Some(ref filter_key) = config.filter_key {
        let val_opt = json.get(filter_key);
        if val_opt.is_none() {
            debug!("Dropping policy message because filter key '{}' is missing", filter_key);
            return None;
        }
        if let Some(ref filter_val) = config.filter_val {
            if let Some(val) = val_opt.unwrap().as_str() {
                if !filter_val.eq(val) {
                    debug!("Dropping policy message because filter key value '{}' != '{}'", val, filter_val);
                    return None;
                }
            }
        }
    }

    // validate email is present
    let email_path = config.map_key_email.as_deref().unwrap_or(DEFAULT_EMAIL_PATH);
    let email = get_nested_value(&json, email_path).and_then(|v| v.as_str()).filter(|e| !e.is_empty());
    if email.is_none() {
        warn!("Policy message missing email at '{}'", email_path);
        return None;
    }

    Some(json)
}

pub fn extract_routing_keys<'a>(config: &ILConfig, json: &'a Value) -> Vec<&'a str> {
    let routing_keys_config = match config.policy_routing_keys.as_ref() {
        Some(v) => v,
        None => return Vec::new(),
    };

    routing_keys_config
        .split(',')
        .filter_map(|field| {
            let field = field.trim();
            json.get(field).and_then(|v| v.as_str())
        })
        .collect()
}

pub fn extract_shift(config: &ILConfig, json: &Value) -> u64 {
    let shift_path = config.map_key_shift.as_deref().unwrap_or(DEFAULT_SHIFT_PATH);
    let raw = get_nested_value(json, shift_path)
        .and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| v.as_i64()))
        .unwrap_or(0);
    let adjusted = raw + config.shift_offset;
    if adjusted < 0 { 0 } else { adjusted as u64 }
}

pub fn extract_email<'a>(config: &ILConfig, json: &'a Value) -> Option<&'a str> {
    let email_path = config.map_key_email.as_deref().unwrap_or(DEFAULT_EMAIL_PATH);
    get_nested_value(json, email_path)
        .and_then(|v| v.as_str())
        .filter(|e| !e.is_empty())
}

async fn resolve_escalation_policy(ilert_client: &ILert, routing_key: &str) -> Option<Value> {
    let url = ilert_client.build_url("/escalation-policies/resolve");

    let mut request = ilert_client.http_client
        .get(&url)
        .query(&[("routingKey", routing_key)]);

    if let Some(ref token) = ilert_client.api_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to resolve escalation policy for routing key '{}': {}", routing_key, e);
            return None;
        }
    };

    let status = response.status().as_u16();
    if status != 200 {
        warn!("Resolve escalation policy for '{}' returned status {}", routing_key, status);
        return None;
    }

    match response.json::<Value>().await {
        Ok(body) => Some(body),
        Err(e) => {
            error!("Failed to parse escalation policy response: {}", e);
            None
        }
    }
}

async fn resolve_user_by_email(ilert_client: &ILert, email: &str) -> Option<Value> {
    let url = ilert_client.build_url("/users/resolve");

    let body = serde_json::json!({"email": email});

    let mut request = ilert_client.http_client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string());

    if let Some(ref token) = ilert_client.api_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to resolve user by email '{}': {}", email, e);
            return None;
        }
    };

    let status = response.status().as_u16();
    if status != 200 {
        warn!("Resolve user for '{}' returned status {}", email, status);
        return None;
    }

    match response.json::<Value>().await {
        Ok(body) => Some(body),
        Err(e) => {
            error!("Failed to parse user resolve response: {}", e);
            None
        }
    }
}

async fn update_policy_level(ilert_client: &ILert, policy_id: i64, shift: u64, user: &Value) -> bool {
    let url = ilert_client.build_url(&format!("/escalation-policies/{}/levels/{}", policy_id, shift));

    let mut request = ilert_client.http_client
        .put(&url)
        .header("Content-Type", "application/json")
        .body(user.to_string());

    if let Some(ref token) = ilert_client.api_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to update policy {} level {}: {}", policy_id, shift, e);
            return false;
        }
    };

    let status = response.status().as_u16();
    if status == 200 {
        info!("Successfully updated escalation policy {} level {} with user", policy_id, shift);
        true
    } else {
        warn!("Update policy {} level {} returned status {}", policy_id, shift, status);
        false
    }
}

pub async fn handle_policy_update(ilert_client: &ILert, config: &ILConfig, payload: &str) -> bool {
    let json = match parse_policy_payload(config, payload) {
        Some(j) => j,
        None => return false,
    };

    let email = match extract_email(config, &json) {
        Some(e) => e.to_string(),
        None => return false,
    };
    let shift = extract_shift(config, &json);
    let routing_keys = extract_routing_keys(config, &json);

    if routing_keys.is_empty() {
        warn!("No routing keys could be extracted from policy message");
        return false;
    }

    debug!("Policy update: email={}, shift={}, routing_keys={:?}", email, shift, routing_keys);

    // resolve user once
    let user = match resolve_user_by_email(ilert_client, &email).await {
        Some(u) => u,
        None => {
            error!("Could not resolve user for email '{}'", email);
            return true; // retry
        }
    };

    // for each routing key, resolve policy and update level
    for routing_key in routing_keys {
        let policy = match resolve_escalation_policy(ilert_client, routing_key).await {
            Some(p) => p,
            None => {
                warn!("Could not resolve escalation policy for routing key '{}'", routing_key);
                continue;
            }
        };

        let policy_id = match policy.get("id").and_then(|v| v.as_i64()) {
            Some(id) => id,
            None => {
                error!("Escalation policy response missing 'id' field for routing key '{}'", routing_key);
                continue;
            }
        };

        update_policy_level(ilert_client, policy_id, shift, &user).await;
    }

    false // no retry needed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ILConfig;

    const SAMPLE_PAYLOAD: &str = r#"{
        "uuid": "550x8400-x29b-11d4-x716-446655440000",
        "location": "powerplant",
        "field": "clover",
        "slot": "three50",
        "eventTime": "2025-07-16T15:42:30.621Z",
        "eventType": "active",
        "data": {
            "email": "support@ilert.com",
            "shift": "1"
        }
    }"#;

    // --- get_nested_value ---

    #[test]
    fn nested_value_single_key() {
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        assert_eq!(get_nested_value(&json, "location").unwrap().as_str().unwrap(), "powerplant");
    }

    #[test]
    fn nested_value_dot_path() {
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        assert_eq!(get_nested_value(&json, "data.email").unwrap().as_str().unwrap(), "support@ilert.com");
    }

    #[test]
    fn nested_value_missing_returns_none() {
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        assert!(get_nested_value(&json, "data.nonexistent").is_none());
    }

    #[test]
    fn nested_value_partial_path_returns_none() {
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        assert!(get_nested_value(&json, "data.email.deep").is_none());
    }

    // --- parse + extract with defaults ---

    #[test]
    fn parse_sample_payload_defaults() {
        let config = ILConfig::new();
        let json = parse_policy_payload(&config, SAMPLE_PAYLOAD).unwrap();
        assert_eq!(extract_email(&config, &json).unwrap(), "support@ilert.com");
        assert_eq!(extract_shift(&config, &json), 1);
    }

    #[test]
    fn extract_shift_defaults_to_zero() {
        let config = ILConfig::new();
        let payload = r#"{"eventType": "active", "data": {"email": "test@ilert.com"}}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 0);
    }

    // --- custom field paths ---

    #[test]
    fn custom_email_path_flat() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("contactEmail".to_string());
        let payload = r#"{"contactEmail": "flat@ilert.com"}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_email(&config, &json).unwrap(), "flat@ilert.com");
    }

    #[test]
    fn custom_email_path_nested() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("user.info.email".to_string());
        let payload = r#"{"user": {"info": {"email": "deep@ilert.com"}}}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_email(&config, &json).unwrap(), "deep@ilert.com");
    }

    #[test]
    fn custom_shift_path() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("level".to_string());
        let payload = r#"{"email": "test@ilert.com", "level": "3"}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 3);
    }

    #[test]
    fn custom_shift_path_nested() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("meta.shift".to_string());
        let payload = r#"{"email": "test@ilert.com", "meta": {"shift": "5"}}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 5);
    }

    #[test]
    fn shift_as_integer_value() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("shift".to_string());
        let payload = r#"{"email": "test@ilert.com", "shift": 2}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 2);
    }

    #[test]
    fn shift_offset_negative() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("shift".to_string());
        config.shift_offset = -1;
        let payload = r#"{"email": "test@ilert.com", "shift": 2}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 1);
    }

    #[test]
    fn shift_offset_clamps_to_zero() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("shift".to_string());
        config.shift_offset = -5;
        let payload = r#"{"email": "test@ilert.com", "shift": 2}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 0);
    }

    #[test]
    fn shift_offset_with_string_value() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("email".to_string());
        config.map_key_shift = Some("shift".to_string());
        config.shift_offset = -1;
        let payload = r#"{"email": "test@ilert.com", "shift": "3"}"#;
        let json = parse_policy_payload(&config, payload).unwrap();
        assert_eq!(extract_shift(&config, &json), 2);
    }

    #[test]
    fn missing_custom_email_path_returns_none() {
        let mut config = ILConfig::new();
        config.map_key_email = Some("wrong.path".to_string());
        let payload = r#"{"data": {"email": "test@ilert.com"}}"#;
        assert!(parse_policy_payload(&config, payload).is_none());
    }

    // --- routing keys ---

    #[test]
    fn extract_routing_keys_single() {
        let mut config = ILConfig::new();
        config.policy_routing_keys = Some("location".to_string());
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        let keys = extract_routing_keys(&config, &json);
        assert_eq!(keys, vec!["powerplant"]);
    }

    #[test]
    fn extract_routing_keys_multiple() {
        let mut config = ILConfig::new();
        config.policy_routing_keys = Some("location,slot".to_string());
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        let keys = extract_routing_keys(&config, &json);
        assert_eq!(keys, vec!["powerplant", "three50"]);
    }

    #[test]
    fn extract_routing_keys_with_spaces() {
        let mut config = ILConfig::new();
        config.policy_routing_keys = Some("location , slot".to_string());
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        let keys = extract_routing_keys(&config, &json);
        assert_eq!(keys, vec!["powerplant", "three50"]);
    }

    #[test]
    fn extract_routing_keys_missing_field_skipped() {
        let mut config = ILConfig::new();
        config.policy_routing_keys = Some("location,nonexistent,slot".to_string());
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        let keys = extract_routing_keys(&config, &json);
        assert_eq!(keys, vec!["powerplant", "three50"]);
    }

    #[test]
    fn extract_routing_keys_none_configured() {
        let config = ILConfig::new();
        let json: Value = serde_json::from_str(SAMPLE_PAYLOAD).unwrap();
        let keys = extract_routing_keys(&config, &json);
        assert!(keys.is_empty());
    }

    // --- filter ---

    #[test]
    fn filter_by_event_type_passes() {
        let mut config = ILConfig::new();
        config.filter_key = Some("eventType".to_string());
        config.filter_val = Some("active".to_string());
        assert!(parse_policy_payload(&config, SAMPLE_PAYLOAD).is_some());
    }

    #[test]
    fn filter_by_event_type_rejects() {
        let mut config = ILConfig::new();
        config.filter_key = Some("eventType".to_string());
        config.filter_val = Some("inactive".to_string());
        assert!(parse_policy_payload(&config, SAMPLE_PAYLOAD).is_none());
    }

    #[test]
    fn filter_missing_key_rejects() {
        let mut config = ILConfig::new();
        config.filter_key = Some("nonexistent".to_string());
        assert!(parse_policy_payload(&config, SAMPLE_PAYLOAD).is_none());
    }

    // --- error cases ---

    #[test]
    fn parse_invalid_json_returns_none() {
        let config = ILConfig::new();
        assert!(parse_policy_payload(&config, "not json").is_none());
    }

    #[test]
    fn parse_missing_email_returns_none() {
        let config = ILConfig::new();
        let payload = r#"{"eventType": "active", "data": {}}"#;
        assert!(parse_policy_payload(&config, payload).is_none());
    }

    #[test]
    fn parse_empty_email_returns_none() {
        let config = ILConfig::new();
        let payload = r#"{"eventType": "active", "data": {"email": ""}}"#;
        assert!(parse_policy_payload(&config, payload).is_none());
    }

    #[test]
    fn parse_missing_data_returns_none() {
        let config = ILConfig::new();
        let payload = r#"{"eventType": "active"}"#;
        assert!(parse_policy_payload(&config, payload).is_none());
    }
}

#[cfg(test)]
mod tests {
    use crate::models::heartbeat::HeartbeatJson;

    #[test]
    fn parse_valid_heartbeat() {
        let payload = r#"{"apiKey": "il1hbt123abc"}"#;
        let result = HeartbeatJson::parse_heartbeat_json(payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap().apiKey, "il1hbt123abc");
    }

    #[test]
    fn parse_heartbeat_missing_api_key() {
        let payload = r#"{}"#;
        let result = HeartbeatJson::parse_heartbeat_json(payload);
        assert!(result.is_none());
    }

    #[test]
    fn parse_heartbeat_invalid_json() {
        let result = HeartbeatJson::parse_heartbeat_json("not json");
        assert!(result.is_none());
    }

    #[test]
    fn parse_heartbeat_empty_string() {
        let result = HeartbeatJson::parse_heartbeat_json("");
        assert!(result.is_none());
    }

    #[test]
    fn parse_heartbeat_ignores_extra_fields() {
        let payload = r#"{"apiKey": "key123", "extra": "value"}"#;
        let result = HeartbeatJson::parse_heartbeat_json(payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap().apiKey, "key123");
    }
}

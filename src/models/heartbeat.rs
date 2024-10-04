use log::error;
use serde_derive::{Deserialize, Serialize};

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeartbeatJson {
    pub apiKey: String
}

impl HeartbeatJson {

    pub fn parse_heartbeat_json(payload: &str) -> Option<HeartbeatJson> {
        let parsed = serde_json::from_str(payload);
        match parsed {
            Ok(v) => Some(v),
            Err(e) => {
                error!("Failed to parse heartbeat mqtt payload {}", e);
                None
            }
        }
    }
}
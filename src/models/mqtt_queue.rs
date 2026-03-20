#[derive(Debug, Clone)]
pub struct MqttQueueItem {
    pub id: Option<String>,
    pub topic: String,
    pub payload: String,
    pub inserted_at: Option<String>,
}

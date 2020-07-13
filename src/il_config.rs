#[derive(Clone)]
pub struct ILConfig {
    pub http_host: String,
    pub http_port: i32,
    pub http_worker_count: i8,
    pub db_file: String,
    pub heartbeat_key: Option<String>,
    pub mqtt_host: Option<String>,
    pub mqtt_port: Option<u16>,
    pub mqtt_name: Option<String>,
    pub mqtt_event_topic: Option<String>,
    pub mqtt_heartbeat_topic: Option<String>
}

impl ILConfig {

    pub fn new() -> ILConfig {
        ILConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 8977,
            http_worker_count: 2,
            db_file: "./ilagent.db3".to_string(),
            heartbeat_key: None,
            mqtt_host: None,
            mqtt_port: None,
            mqtt_name: None,
            mqtt_event_topic: None,
            mqtt_heartbeat_topic: None
        }
    }

    pub fn get_http_bind_str(&self) -> String {
        format!("{}:{}", self.http_host, self.http_port)
    }

    pub fn get_port_as_string(&self) -> String {
        return self.http_port.to_string();
    }

    pub fn set_port_from_str(&mut self, str: &str) -> () {
        self.http_port = str.parse::<i32>().expect("Failed to parse http port");
        ()
    }

    pub fn set_mqtt_port_from_str(&mut self, str: &str) -> () {
        let port = str.parse::<u16>().expect("Failed to parse http port");
        self.mqtt_port = Some(port);
        ()
    }
}

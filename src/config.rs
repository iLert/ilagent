#[derive(Clone)]
pub struct ILConfig {
    pub http_host: String,
    pub http_port: i32,
    pub start_http: bool,
    pub http_worker_count: i8,
    pub db_file: String,
    pub heartbeat_key: Option<String>,

    pub mqtt_host: Option<String>,
    pub mqtt_port: Option<u16>,
    pub mqtt_name: Option<String>,
    pub mqtt_username: Option<String>,
    pub mqtt_password: Option<String>,

    pub kafka_brokers: Option<String>,
    pub kafka_group_id: Option<String>,

    pub event_topic: Option<String>,
    pub heartbeat_topic: Option<String>,

    pub event_key: Option<String>,
    pub map_key_etype: Option<String>,
    pub map_key_alert_key: Option<String>,
    pub map_key_summary: Option<String>,

    pub map_val_etype_alert: Option<String>,
    pub map_val_etype_accept: Option<String>,
    pub map_val_etype_resolve: Option<String>,

    pub filter_key: Option<String>,
    pub filter_val: Option<String>
}

impl ILConfig {

    pub fn new() -> ILConfig {
        ILConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 8977,
            start_http: false,
            http_worker_count: 1,
            db_file: "./ilagent.db3".to_string(),
            heartbeat_key: None,
            mqtt_host: None,
            mqtt_port: None,
            mqtt_name: None,
            mqtt_username: None,
            mqtt_password: None,
            kafka_brokers: None,
            kafka_group_id: None,
            event_topic: None,
            heartbeat_topic: None,
            event_key: None,
            map_key_etype: None,
            map_key_alert_key: None,
            map_key_summary: None,
            map_val_etype_alert: None,
            map_val_etype_accept: None,
            map_val_etype_resolve: None,
            filter_key: None,
            filter_val: None
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

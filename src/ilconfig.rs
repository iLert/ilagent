#[derive(Clone)]
pub struct ILConfig {
    pub http_host: String,
    pub http_port: i32,
    pub db_file: String,
}

impl ILConfig {

    pub fn new() -> ILConfig {
        ILConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 8977,
            db_file: "./ilagent.db3".to_string(),
        }
    }

    pub fn get_http_bind_str(&self) -> String {
        format!("{}:{}", self.http_host, self.http_port)
    }
}

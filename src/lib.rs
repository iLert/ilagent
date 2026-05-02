use ilert::ilert::ILert;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::Mutex;

pub const CALLER_AGENT: &str = concat!("ilagent/", env!("CARGO_PKG_VERSION"));

pub mod config;
pub mod consumers;
pub mod db;
pub mod hbt;
pub mod http_server;
pub mod json_util;
pub mod models;
pub mod poll;
pub mod version_check;

pub struct MqttProbeState {
    pub connected: AtomicBool,
    pub subscriptions_ready: AtomicBool,
    pub expected_subscriptions: u32,
    pub received_subscriptions: AtomicU32,
    pub last_error: std::sync::Mutex<Option<String>>,
    pub reconnect_attempts: AtomicU32,
}

impl MqttProbeState {
    pub fn new(expected_subscriptions: u32) -> Self {
        Self {
            connected: AtomicBool::new(false),
            subscriptions_ready: AtomicBool::new(false),
            expected_subscriptions,
            received_subscriptions: AtomicU32::new(0),
            last_error: std::sync::Mutex::new(None),
            reconnect_attempts: AtomicU32::new(0),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.connected.load(Ordering::Relaxed) && self.subscriptions_ready.load(Ordering::Relaxed)
    }

    pub fn set_connected(&self) {
        self.connected.store(true, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.subscriptions_ready.store(false, Ordering::Relaxed);
        self.received_subscriptions.store(0, Ordering::Relaxed);
    }

    pub fn record_suback_success(&self) {
        let prev = self.received_subscriptions.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= self.expected_subscriptions {
            self.subscriptions_ready.store(true, Ordering::Relaxed);
        }
    }

    pub fn record_error(&self, error: String) {
        self.reconnect_attempts.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error);
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }
}

pub struct KafkaProbeState {
    pub consumer_started: AtomicBool,
    pub subscribed: AtomicBool,
    pub worker_exited: AtomicBool,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl KafkaProbeState {
    pub fn new() -> Self {
        Self {
            consumer_started: AtomicBool::new(false),
            subscribed: AtomicBool::new(false),
            worker_exited: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.consumer_started.load(Ordering::Relaxed)
            && self.subscribed.load(Ordering::Relaxed)
            && !self.worker_exited.load(Ordering::Relaxed)
    }

    pub fn record_error(&self, error: String) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error);
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }
}

pub struct DaemonContext {
    pub config: config::ILConfig,
    pub db: Mutex<db::ILDatabase>,
    pub ilert_client: ILert,
    pub running: AtomicBool,
    pub mqtt_probe: Option<MqttProbeState>,
    pub kafka_probe: Option<KafkaProbeState>,
}

use ilert::ilert::ILert;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;

pub mod cleanup;
pub mod config;
pub mod consumers;
pub mod db;
pub mod hbt;
pub mod http_server;
pub mod json_util;
pub mod models;
pub mod poll;
pub mod version_check;

pub struct DaemonContext {
    pub config: config::ILConfig,
    pub db: Mutex<db::ILDatabase>,
    pub ilert_client: ILert,
    pub running: AtomicBool,
}

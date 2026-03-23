use std::sync::atomic::AtomicBool;
use ilert::ilert::ILert;
use tokio::sync::Mutex;

pub mod config;
pub mod db;
pub mod json_util;
pub mod models;
pub mod hbt;
pub mod consumers;
pub mod poll;
pub mod http_server;
pub mod cleanup;
pub mod version_check;

pub struct DaemonContext {
    pub config: config::ILConfig,
    pub db: Mutex<db::ILDatabase>,
    pub ilert_client: ILert,
    pub running: AtomicBool
}

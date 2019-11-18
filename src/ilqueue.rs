use rusqlite::types::ToSql;
use rusqlite::{Connection, NO_PARAMS};
use log::{info, error};
use serde_derive::{Deserialize, Serialize};
use chrono::prelude::*;

const EVENT_TYPE_ALERT: &str = "ALERT";
const CURRENT_VERSION: i32 = 1;
const VERSION_KEY: &str = "version";

#[derive(Debug)]
struct ILAgentItem {
    key: String,
    val: String,
    created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IncidentQueueItem {
    pub id: i32,
    pub api_key: String,
    pub event_type: String,
    pub incident_key: Option<String>,
    pub summary: String,
    pub created_at: Option<String>,
}

pub struct ILQueue {
    conn: Connection,
}

/*
curl -X POST \
  https://ilertnow.com/api/v1/events \
  -H 'Content-Type: application/json' \
  -H 'cache-control: no-cache' \
  -d '{
  "apiKey": "",
  "eventType": "ALERT",
  "incidentKey": null,
  "summary": "Uhh uhhh whats going on."
}'
*/

impl ILQueue {

    pub fn new(path: &str) -> ILQueue {
        info!("SQLite Version: {}", rusqlite::version());
        let conn = Connection::open(path).unwrap();
        ILQueue { conn }
    }

    pub fn prepare_database(&self) -> () {
        info!("Preparing database..");

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS ilagent (
                  key                 TEXT PRIMARY KEY,
                  val                 TEXT NOT NULL,
                  created_at          TEXT NOT NULL
                  )",
            NO_PARAMS,
        ).unwrap();

        let version = self.get_ilagent_value_as_number(VERSION_KEY);
        match version {
            None => {
                info!("Database not bootstrapped yet.");

                self.__migrate_to_version_1().unwrap();

                let result = self.create_ilagent_item(
            self.create_ilagent_item_instance(VERSION_KEY,CURRENT_VERSION.to_string().as_str()));
                match result {
                    Err(e) => panic!("Failed to bootstrap database: {:?}.", e),
                    Ok(result_val) => info!("Bootstrapped database {}.", result_val),
                };
            },
            Some(version_val) => {
                info!("Database on version {}.", version_val);
                // TODO: validate if version needs upgrade and run migrations
            },
        };

        ()
    }

    fn get_ilagent_value(&self, key: &str) -> Option<String> {

        let mut stmt = self.conn.prepare("SELECT key, val, created_at FROM ilagent WHERE key = ?1").unwrap();
        let items = stmt
            .query_map(&[&key], |row| Ok(ILAgentItem {
                key: row.get(0).unwrap(),
                val: row.get(1).unwrap(),
                created_at: row.get(2).unwrap(),
            }));

        match items {
            Err(e) => {
                error!("Failed to get ilagent value: {}; {:?}.", key, e);
                None
            },
            Ok(mut item_values) => {
                match item_values.next() {
                    None => None,
                    Some(val) => Some(val.unwrap().val),
                }
            },
        }
    }

    fn get_ilagent_value_as_number(&self, key: &str) -> Option<i32> {
        let row_val = self.get_ilagent_value(key);
        match row_val {
            None => None,
            Some(val) => match val.parse::<i32>() {
                Err(e) => panic!("Error during parsing of the ilagent db version {:?}.", e),
                Ok(int_val) => Some(int_val)
            }
        }
    }

    fn create_ilagent_item_instance(&self, key: &str, val: &str) -> ILAgentItem {

        let incident = ILAgentItem {
            key: key.to_string(),
            val: val.to_string(),
            created_at: Some(Utc::now().to_string()),
        };

        incident
    }

    fn create_ilagent_item(&self, item: ILAgentItem) -> Result<usize,  rusqlite::Error> {
        let default_created = Utc::now().to_string();
        let created_at = item.created_at.as_ref().unwrap_or(&default_created);
        self.conn.execute(
            "INSERT INTO ilagent (key, val, created_at)
                          VALUES (?1, ?2, ?3)",
            &[&item.key as &dyn ToSql, &item.val, created_at],
        )
    }

    fn __migrate_to_version_1(&self) -> Result<usize,  rusqlite::Error> {
        info!("Running migration to version 1.");

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS incident_items (
                  id                 INTEGER PRIMARY KEY,
                  api_key            TEXT NOT NULL,
                  event_type         TEXT NOT NULL,
                  incident_key       TEXT NOT NULL,
                  summary            TEXT NOT NULL,
                  created_at         TEXT NOT NULL
                  )",
            NO_PARAMS,
        )
    }

    pub fn create_incident_instance(&self, api_key: &str, summary: &str) -> IncidentQueueItem {

        let incident = IncidentQueueItem {
            id: 0,
            api_key: api_key.to_string(),
            event_type: EVENT_TYPE_ALERT.to_string(),
            incident_key: None,
            summary: summary.to_string(),
            created_at: Some(Utc::now().to_string()),
        };

        incident
    }

    pub fn get_incidents(&self, limit: i32) -> Vec<Result<IncidentQueueItem,  rusqlite::Error>> {

        let mut stmt = self.conn.prepare("SELECT id, api_key, event_type, incident_key, summary, created_at FROM incident_items LIMIT ?1").unwrap();
        let items = stmt
            .query_map(&[&limit], |row| Ok(IncidentQueueItem {
                id: row.get(0).unwrap(),
                api_key: row.get(1).unwrap(),
                event_type: row.get(2).unwrap(),
                incident_key: row.get(3).unwrap(),
                summary: row.get(4).unwrap(),
                created_at: row.get(5).unwrap(),
            })).unwrap();

        items.collect::<Vec<Result<IncidentQueueItem,  rusqlite::Error>>>()
    }

    pub fn create_incident(&self, incident: &IncidentQueueItem) -> Result<usize,  rusqlite::Error> {
        let default_created = Utc::now().to_string();
        let created_at = incident.created_at.as_ref().unwrap_or(&default_created);
         self.conn.execute(
            "INSERT INTO incident_items (api_key, event_type, incident_key, summary, created_at)
                  VALUES (?1, ?2, ?3, ?4, ?5)",
            &[&incident.api_key as &dyn ToSql, &incident.event_type, &incident.incident_key,
                &incident.summary, created_at],
        )
    }

    pub fn delete_incident(&self, incident: IncidentQueueItem) -> Result<usize,  rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM incident_items WHERE id = ?1",
            &[&incident.id],
        )
    }
}
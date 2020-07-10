use rusqlite::types::ToSql;
use rusqlite::{Connection, NO_PARAMS};
use log::{info, error};
use serde_derive::{Deserialize, Serialize};
use chrono::prelude::*;
use uuid::Uuid;

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
pub struct EventQueueItem {
    pub id: Option<String>,
    pub api_key: String,
    pub event_type: String,
    pub incident_key: Option<String>,
    pub summary: String,
    pub created_at: Option<String>,
}

impl EventQueueItem {

    pub fn new() -> EventQueueItem {
        EventQueueItem {
            id: None,
            api_key: "".to_string(),
            event_type: EVENT_TYPE_ALERT.to_string(),
            incident_key: None,
            summary: "".to_string(),
            created_at: None
        }
    }
}

pub struct ILDatabase {
    conn: Connection,
}

impl ILDatabase {

    pub fn new(path: &str) -> ILDatabase {
        info!("SQLite Version: {}", rusqlite::version());
        let conn = Connection::open(path).unwrap();
        ILDatabase { conn }
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

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS event_items (
                      id                 TEXT PRIMARY KEY,
                      api_key            TEXT NOT NULL,
                      event_type         TEXT NOT NULL,
                      incident_key       TEXT NULL,
                      summary            TEXT NOT NULL,
                      created_at         TEXT NOT NULL
                  )",
            NO_PARAMS,
        );

        info!("Database is bootstrapped.");
        ()
    }

    fn get_il_value(&self, key: &str) -> Option<String> {

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

    fn get_il_value_as_number(&self, key: &str) -> Option<i32> {
        let row_val = self.get_il_value(key);
        match row_val {
            None => None,
            Some(val) => match val.parse::<i32>() {
                Err(e) => panic!("Error during parsing of the ilagent db version {:?}.", e),
                Ok(int_val) => Some(int_val)
            }
        }
    }

    fn create_il_item(&self, item: ILAgentItem) -> Result<usize,  rusqlite::Error> {
        let default_created = Utc::now().to_string();
        let created_at = item.created_at.as_ref().unwrap_or(&default_created);
        self.conn.execute(
            "INSERT INTO ilagent (key, val, created_at)
                          VALUES (?1, ?2, ?3)",
            &[&item.key as &dyn ToSql, &item.val, created_at],
        )
    }

    pub fn get_il_event(&self, event_id: &str) -> Result<Option<EventQueueItem>, rusqlite::Error> {

        let mut stmt = self.conn.prepare("SELECT * FROM event_items WHERE id = ?1").unwrap();
        let query_result = stmt
            .query_map(&[&event_id], |row| Ok(EventQueueItem {
                id: row.get(0).unwrap(),
                api_key: row.get(1).unwrap(),
                event_type: row.get(2).unwrap(),
                incident_key: row.get(3).unwrap(),
                summary: row.get(4).unwrap(),
                created_at: row.get(5).unwrap(),
            }));

        match query_result {
            Ok(items) => {

                let vec = items
                    .filter(|row_res| {
                        match row_res {
                            Ok(row) => true,
                            _ => false
                        }
                    })
                    .map(|row_res| {
                        match row_res {
                            Ok(row) => row,
                            _ => EventQueueItem::new()
                        }
                    })
                    .collect::<Vec<EventQueueItem>>();

                if vec.len() > 0 {
                    Ok(Some(vec[0].clone()))
                } else {
                    Ok(None)
                }
            },
            Err(e) => {
                error!("Failed to fetch event item {:?}.", e);
                Err(e)
            }
        }
    }

    pub fn get_il_events(&self, limit: i32) -> Vec<Result<EventQueueItem,  rusqlite::Error>> {

        let mut stmt = self.conn.prepare("SELECT * FROM event_items LIMIT ?1").unwrap();
        let query_result = stmt
            .query_map(&[&limit], |row| Ok(EventQueueItem {
                id: row.get(0).unwrap(),
                api_key: row.get(1).unwrap(),
                event_type: row.get(2).unwrap(),
                incident_key: row.get(3).unwrap(),
                summary: row.get(4).unwrap(),
                created_at: row.get(5).unwrap(),
            }));

        match query_result {
            Ok(items) => items.collect::<Vec<Result<EventQueueItem,  rusqlite::Error>>>(),
            Err(error) => {
                error!("Failed to fetch events {:?}.", error);
                Vec::new()
            }
        }
    }

    pub fn create_il_event(&self, item: &EventQueueItem) -> Result<Option<EventQueueItem>,  rusqlite::Error> {

        let default_created = Utc::now().to_string();
        let created_at = item.created_at.as_ref().unwrap_or(&default_created);

        let item_id = match item.id.clone() {
            Some(id) => Some(id),
            None => Some(Uuid::new_v4().to_string()),
        };

        let insert_result = self.conn.execute(
            "INSERT INTO event_items (api_key, event_type, incident_key, summary, created_at, id)
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[&item.api_key as &dyn ToSql, &item.event_type, &item.incident_key,
                &item.summary, created_at, &item_id],
        );

        match insert_result {
            Ok(_) => {
                self.get_il_event(item_id.clone().unwrap().as_str())
            },
            Err(e) => Err(e)
        }
    }

    pub fn delete_il_event(&self, item: EventQueueItem) -> Result<usize,  rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM event_items WHERE id = ?1",
            &[&item.id],
        )
    }
}
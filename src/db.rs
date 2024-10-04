use rusqlite::types::ToSql;
use rusqlite::{Connection, Row};
use log::{info, error};
use chrono::prelude::*;
use uuid::Uuid;

use ilert::ilert_builders::{ILertEventType};
use crate::models::event_db::EventQueueItem;

const DB_MIGRATION_VAL: &str = "1";
const DB_MIGRATION_V1: &str = "mig_1";
const DB_MIGRATION_V2: &str = "mig_2";

#[derive(Debug)]
struct ILAgentItem {
    key: String,
    val: String,
    created_at: Option<String>,
}

impl ILAgentItem {

    pub fn new(key: &str, val: &str) -> ILAgentItem {
        ILAgentItem {
            key: key.to_string(),
            val: val.to_string(),
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
        let conn = Connection::open(path).expect("Failed to setup SQLite connection");
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
            [],
        ).expect("Failed to bootstrap database");

        let mig_1 = self.get_il_value(DB_MIGRATION_V1);
        if mig_1.is_none() {

            self.conn.execute(
                "CREATE TABLE event_items (
                      id                 TEXT PRIMARY KEY,
                      api_key            TEXT NOT NULL,
                      event_type         TEXT NOT NULL,
                      incident_key       TEXT NULL,
                      summary            TEXT NOT NULL,
                      created_at         TEXT NOT NULL,
                      priority           TEXT NULL,
                      images             TEXT NULL,
                      links              TEXT NULL,
                      custom_details     TEXT NULL,
                      details            TEXT NULL
                  )",
                [],
            ).expect("Database migration failed (v1)");

            self.set_il_val(DB_MIGRATION_V1, DB_MIGRATION_VAL)
                .expect("Database migration failed (v1, set)");
            info!("Database migrated to {}", DB_MIGRATION_V1);
        }

        let mig_2 = self.get_il_value(DB_MIGRATION_V2);
        if mig_2.is_none() {

            self.conn.execute(
                "ALTER TABLE event_items RENAME COLUMN incident_key TO alert_key",
                [],
            ).expect("Database migration failed (v2, 1)");

            self.conn.execute(
                "ALTER TABLE event_items ADD COLUMN inserted_at DATETIME DEFAULT(STRFTIME('%Y-%m-%d %H:%M:%f', 'NOW'))",
                [],
            ).expect("Database migration failed (v2, 2)");

            self.set_il_val(DB_MIGRATION_V2, DB_MIGRATION_VAL)
                .expect("Database migration failed (v2, set)");
            info!("Database migrated to {}", DB_MIGRATION_V2);
        }

        /*
        Run simple db migrations, if needed, like this:

        let mig_XX = self.get_il_value(DB_MIGRATION_VXX);
        if mig_XX.is_none() {

            // SQL MAGIC HERE

            self.set_il_val(DB_MIGRATION_VXX, DB_MIGRATION_VAL)
                .expect("Database migration failed");
            info!("Database migrated to {}", DB_MIGRATION_VXX);
        }
         */

        info!("Database is bootstrapped.");
        ()
    }

    pub fn get_il_value(&self, key: &str) -> Option<String> {

        let mut stmt = self.conn.prepare("SELECT * FROM ilagent WHERE key = ?1").unwrap();
        let items = stmt
            .query_map(&[&key], |row| Ok(ILAgentItem {
                key: row.get(0).unwrap_or("".to_string()),
                val: row.get(1).unwrap_or("".to_string()),
                created_at: row.get(2).unwrap_or(None)
            }));

        match items {
            Err(e) => {
                error!("Failed to get ilagent value: {}; {:?}.", key, e);
                None
            },
            Ok(mut item_values) => {
                match item_values.next() {
                    None => None,
                    Some(item) => match item {
                        Ok(item_val) => Some(item_val.val),
                        _ => None
                    }
                }
            },
        }
    }

    pub fn set_il_val(&self, key: &str, val: &str) -> Result<usize,  rusqlite::Error> {
        let item = ILAgentItem::new(key, val);
        if self.get_il_value(key).is_some() {
            self.update_il_item(item)
        } else {
            self.create_il_item(item)
        }
    }

    fn create_il_item(&self, item: ILAgentItem) -> Result<usize,  rusqlite::Error> {
        let default_created = Utc::now().to_string();
        let created_at = item.created_at.as_ref().unwrap_or(&default_created);
        self.conn.execute(
            "INSERT INTO ilagent (key, val, created_at) VALUES (?1, ?2, ?3)",
            &[&item.key as &dyn ToSql, &item.val, created_at],
        )
    }

    fn update_il_item(&self, item: ILAgentItem) -> Result<usize,  rusqlite::Error> {
        self.conn.execute(
            "UPDATE ilagent SET val = ?1 WHERE key = ?2",
            &[&item.val as &dyn ToSql, &item.key],
        )
    }

    /*
    pub fn delete_il_item(&self, key: &str) -> Result<usize,  rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM ilagent WHERE key = ?1",
            &[&key],
        )
    } */

    fn convert_db_row_to_event(row: &Row) -> Result<EventQueueItem, rusqlite::Error> {
        Ok(EventQueueItem {
            id: row.get(0).unwrap_or(None),
            api_key: row.get(1).unwrap_or("".to_string()),
            event_type: row.get(2).unwrap_or(ILertEventType::ALERT.as_str().to_string()),
            alert_key: row.get(3).unwrap_or(None),
            summary: row.get(4).unwrap_or("".to_string()),
            created_at: row.get(5).unwrap_or(None),
            priority: row.get(6).unwrap_or(None),
            images: row.get(7).unwrap_or(None),
            links: row.get(8).unwrap_or(None),
            custom_details: row.get(9).unwrap_or(None),
            details: row.get(10).unwrap_or(None)
        })
    }

    pub fn get_il_event(&self, event_id: &str) -> Result<Option<EventQueueItem>, rusqlite::Error> {

        let mut stmt = self.conn.prepare("SELECT * FROM event_items WHERE id = ?1").unwrap();
        let query_result = stmt
            .query_map(&[&event_id], |row| {
                ILDatabase::convert_db_row_to_event(row)
            });

        match query_result {
            Ok(items) => {

                let vec = items
                    .filter(|row_res| {
                        match row_res {
                            Ok(_row) => true,
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

    pub fn get_il_events(&self, limit: i32) -> Result<Vec<EventQueueItem>,  rusqlite::Error> {

        let mut stmt = self.conn.prepare("SELECT * FROM event_items ORDER BY inserted_at ASC LIMIT ?1").unwrap();
        let query_result = stmt
            .query_map(&[&limit], |row| {
                ILDatabase::convert_db_row_to_event(row)
            });

        match query_result {
            Ok(items) => {

                let vec = items
                    .filter(|row_res| {
                        match row_res {
                            Ok(_row) => true,
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

                Ok(vec)
            },
            Err(e) => {
                error!("Failed to fetch events {:?}.", e);
                Err(e)
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
            "INSERT INTO event_items (api_key, event_type, alert_key, summary, created_at, id,
                priority, images, links, custom_details, details)
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            &[&item.api_key as &dyn ToSql, &item.event_type, &item.alert_key,
                &item.summary, created_at, &item_id,
                &item.priority, &item.images, &item.links, &item.custom_details, &item.details],
        );

        match insert_result {
            Ok(_) => {
                self.get_il_event(item_id.clone().unwrap_or("".to_string()).as_str())
            },
            Err(e) => Err(e)
        }
    }

    pub fn delete_il_event(&self, id: &str) -> Result<usize,  rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM event_items WHERE id = ?1",
            &[&id],
        )
    }
}
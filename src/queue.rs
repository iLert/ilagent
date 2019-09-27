use rusqlite::types::ToSql;
use rusqlite::{Connection, NO_PARAMS};
use time::Timespec;
use log::info;

const EVENT_TYPE_ALERT: &str = "ALERT";

#[derive(Debug)]
pub struct ILAgentItem {
    key: String,
    val: String,
    created_at: Timespec,
}

#[derive(Debug)]
pub struct IncidentQueueItem {
    id: i32,
    api_key: String,
    event_type: String,
    incident_key: Option<String>,
    summary: String,
    created_at: Timespec,
}

pub struct Queue {
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

impl Queue {

    pub fn new(path: &str) -> Queue {
        info!("SQLite Version: {}", rusqlite::version());
        let conn = Connection::open(path).unwrap();
        Queue { conn }
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
            "CREATE TABLE IF NOT EXISTS incident_items (
                  id                 INTEGER PRIMARY KEY,
                  api_key            TEXT NOT NULL,
                  event_type         TEXT NOT NULL,
                  incident_key       TEXT NOT NULL,
                  summary            TEXT NOT NULL,
                  created_at         TEXT NOT NULL
                  )",
            NO_PARAMS,
        ).unwrap();

        let mut stmt = self.conn.prepare("SELECT key, val, created_at FROM ilagent").unwrap();
        let _keys = stmt
            .query_map(NO_PARAMS, |row| Ok(ILAgentItem {
                key: row.get(0).unwrap(),
                val: row.get(1).unwrap(),
                created_at: row.get(2).unwrap(),
            })).unwrap();

        info!("Database prepared.");
        ()
    }

    pub fn create_incident(&self, api_key: &str, summary: &str) -> Result<IncidentQueueItem, &'static str> {

        let incident = IncidentQueueItem {
            id: 0,
            api_key: api_key.to_string(),
            event_type: EVENT_TYPE_ALERT.to_string(),
            incident_key: None,
            summary: summary.to_string(),
            created_at: time::get_time(),
        };

        Ok(incident)
    }

    pub fn store_incident(&self, incident: IncidentQueueItem) -> Result<usize,  rusqlite::Error> {
         self.conn.execute(
            "INSERT INTO person (api_key, event_type, incident_key, summary, created_at)
                  VALUES (?1, ?2, ?3, ?4, ?5)",
            &[&incident.api_key as &dyn ToSql, &incident.event_type, &incident.incident_key,
                &incident.summary, &incident.created_at],
        )
    }

    pub fn test(&self) -> () {

        /*
        let mut stmt = self.conn
            .prepare("SELECT id, name, time_created, data FROM person").unwrap();

        let person_iter = stmt
            .query_map(NO_PARAMS, |row| Ok(Person {
                id: row.get(0).unwrap(),
                name: row.get(1).unwrap(),
                time_created: row.get(2).unwrap(),
                data: row.get(3).unwrap(),
            })).unwrap();

        for person in person_iter {
            info!("Found person {:?}", person.unwrap());
        } */

        ()
    }
}
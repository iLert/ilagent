use ilagent::db::ILDatabase;
use ilagent::models::event_db::EventQueueItem;
use tempfile::NamedTempFile;

fn temp_db() -> (ILDatabase, NamedTempFile) {
    let file = NamedTempFile::new().expect("failed to create temp file");
    let db = ILDatabase::new(file.path().to_str().unwrap());
    db.prepare_database();
    (db, file)
}

// --- migrations ---

#[test]
fn migrations_run_on_fresh_db() {
    let (db, _f) = temp_db();
    assert_eq!(db.get_il_value("mig_1").unwrap(), "1");
    assert_eq!(db.get_il_value("mig_2").unwrap(), "1");
    assert_eq!(db.get_il_value("mig_3").unwrap(), "1");
}

#[test]
fn migrations_are_idempotent() {
    let file = NamedTempFile::new().unwrap();
    let path = file.path().to_str().unwrap();

    let db1 = ILDatabase::new(path);
    db1.prepare_database();
    drop(db1);

    // run again on same file
    let db2 = ILDatabase::new(path);
    db2.prepare_database();
    assert_eq!(db2.get_il_value("mig_3").unwrap(), "1");
}

// --- event CRUD ---

#[test]
fn insert_and_read_event() {
    let (db, _f) = temp_db();

    let event = EventQueueItem::new_with_required(
        "key1",
        "ALERT",
        "Server down",
        Some("alert-1".to_string()),
    );
    let inserted = db.create_il_event(&event).unwrap().unwrap();

    assert!(inserted.id.is_some());
    assert_eq!(inserted.integration_key, "key1");
    assert_eq!(inserted.event_type, "ALERT");
    assert_eq!(inserted.summary, "Server down");
    assert_eq!(inserted.alert_key.unwrap(), "alert-1");

    // read back by id
    let fetched = db
        .get_il_event(inserted.id.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(fetched.integration_key, "key1");
    assert_eq!(fetched.summary, "Server down");
}

#[test]
fn insert_generates_uuid_when_none() {
    let (db, _f) = temp_db();

    let event = EventQueueItem::new();
    let inserted = db.create_il_event(&event).unwrap().unwrap();
    let id = inserted.id.unwrap();
    assert!(!id.is_empty());
    assert!(id.contains('-'), "should be a UUID with hyphens");
}

#[test]
fn insert_preserves_provided_id() {
    let (db, _f) = temp_db();

    let mut event = EventQueueItem::new_with_required("k1", "ALERT", "test", None);
    event.id = Some("my-custom-id".to_string());
    let inserted = db.create_il_event(&event).unwrap().unwrap();
    assert_eq!(inserted.id.unwrap(), "my-custom-id");
}

#[test]
fn get_events_returns_empty_on_fresh_db() {
    let (db, _f) = temp_db();
    let events = db.get_il_events(10).unwrap();
    assert!(events.is_empty());
}

#[test]
fn get_events_respects_limit() {
    let (db, _f) = temp_db();

    for i in 0..5 {
        let event = EventQueueItem::new_with_required("k1", "ALERT", &format!("event {}", i), None);
        db.create_il_event(&event).unwrap();
    }

    let events = db.get_il_events(3).unwrap();
    assert_eq!(events.len(), 3);
}

#[test]
fn get_events_ordered_by_inserted_at() {
    let (db, _f) = temp_db();

    let e1 = EventQueueItem::new_with_required("k1", "ALERT", "first", None);
    db.create_il_event(&e1).unwrap();
    let e2 = EventQueueItem::new_with_required("k1", "ALERT", "second", None);
    db.create_il_event(&e2).unwrap();

    let events = db.get_il_events(10).unwrap();
    assert_eq!(events[0].summary, "first");
    assert_eq!(events[1].summary, "second");
}

#[test]
fn delete_event_removes_from_queue() {
    let (db, _f) = temp_db();

    let event = EventQueueItem::new_with_required("k1", "ALERT", "test", None);
    let inserted = db.create_il_event(&event).unwrap().unwrap();
    let id = inserted.id.clone().unwrap();

    let deleted = db.delete_il_event(&id).unwrap();
    assert_eq!(deleted, 1);

    let fetched = db.get_il_event(&id).unwrap();
    assert!(fetched.is_none());
}

#[test]
fn delete_nonexistent_event_returns_zero() {
    let (db, _f) = temp_db();
    let deleted = db.delete_il_event("does-not-exist").unwrap();
    assert_eq!(deleted, 0);
}

#[test]
fn insert_event_with_all_optional_fields() {
    let (db, _f) = temp_db();

    let mut event =
        EventQueueItem::new_with_required("k1", "ALERT", "full event", Some("ak-1".to_string()));
    event.priority = Some("HIGH".to_string());
    event.details = Some("<b>details</b>".to_string());
    event.images = Some(r#"[{"src":"http://img.png"}]"#.to_string());
    event.links = Some(r#"[{"href":"http://link.com","text":"Link"}]"#.to_string());
    event.custom_details = Some(r#"{"env":"prod"}"#.to_string());
    event.event_api_path = Some("/v1/events/mqtt/k1".to_string());

    let inserted = db.create_il_event(&event).unwrap().unwrap();
    assert_eq!(inserted.priority.unwrap(), "HIGH");
    assert_eq!(inserted.details.unwrap(), "<b>details</b>");
    assert!(inserted.images.unwrap().contains("img.png"));
    assert!(inserted.links.unwrap().contains("link.com"));
    assert!(inserted.custom_details.unwrap().contains("prod"));
    assert_eq!(inserted.event_api_path.unwrap(), "/v1/events/mqtt/k1");
}

// --- metadata key/value ---

#[test]
fn set_and_get_il_value() {
    let (db, _f) = temp_db();
    db.set_il_val("test_key", "test_val").unwrap();
    assert_eq!(db.get_il_value("test_key").unwrap(), "test_val");
}

#[test]
fn set_il_value_updates_existing() {
    let (db, _f) = temp_db();
    db.set_il_val("key", "v1").unwrap();
    db.set_il_val("key", "v2").unwrap();
    assert_eq!(db.get_il_value("key").unwrap(), "v2");
}

#[test]
fn get_missing_value_returns_none() {
    let (db, _f) = temp_db();
    assert!(db.get_il_value("nonexistent").is_none());
}

use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use log::{info, error};
use std::sync::Mutex;
use std::convert::TryInto;
use serde_derive::{Deserialize, Serialize};
use serde_json::json;

use ilert::ilert::ILert;
use ilert::ilert_builders::{EventImage, EventLink, ILertEventType, ILertPriority};

use crate::il_db::{ILDatabase, EventQueueItem};
use crate::il_config::ILConfig;
use crate::il_hbt;

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueItemJson {
    pub apiKey: String,
    pub eventType: String,
    pub summary: String,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>
}

/**
    helper to apply additional mqtt mappings easier
*/
#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueTransitionItemJson {
    pub apiKey: Option<String>,
    pub eventType: Option<String>,
    pub summary: Option<String>,
    pub details: Option<String>,
    pub alertKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImage>>,
    pub links: Option<Vec<EventLink>>,
    pub customDetails: Option<serde_json::Value>
}

impl EventQueueItemJson {

    pub fn from_transition(trans: EventQueueTransitionItemJson) -> EventQueueItemJson {
        EventQueueItemJson {
            apiKey: trans.apiKey.expect("failed to map required event queue item key: apiKey"),
            eventType: trans.eventType.expect("failed to map required event queue item key: eventType"),
            summary: trans.summary.unwrap_or("".to_string()), // field is optional for some event types
            details: trans.details,
            alertKey: trans.alertKey,
            priority: trans.priority,
            images: trans.images,
            links: trans.links,
            customDetails: trans.customDetails
        }
    }

    pub fn to_db(item: EventQueueItemJson) -> EventQueueItem {

        let images = match item.images {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None
                }
            },
            None => None
        };

        let links = match item.links {
            Some(v) => {
                let serialised = serde_json::to_string(&v);
                match serialised {
                    Ok(str) => Some(str),
                    _ => None
                }
            },
            None => None
        };

        let custom_details = match item.customDetails {
            Some(val) => Some(val.to_string()),
            None => None
        };

        EventQueueItem {
            id: None,
            api_key: item.apiKey,
            event_type: item.eventType,
            alert_key: item.alertKey,
            summary: item.summary,
            details: item.details,
            created_at: None,
            priority: item.priority,
            images,
            links,
            custom_details
        }
    }

    pub fn from_db(item: EventQueueItem) -> EventQueueItemJson {

        let images : Option<Vec<EventImage>> = match item.images {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        let links : Option<Vec<EventLink>> = match item.links {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        let custom_details : Option<serde_json::Value> = match item.custom_details {
            Some(str) => {
                let parsed = serde_json::from_str(str.as_str());
                match parsed {
                    Ok(v) => Some(v),
                    _ => None
                }
            },
            None => None
        };

        EventQueueItemJson {
             apiKey: item.api_key,
             eventType: item.event_type,
             summary: item.summary,
             details: item.details,
             alertKey: item.alert_key,
             priority: item.priority,
             images,
             links,
             customDetails: custom_details
        }
    }
}

struct WebContextContainer {
    db: ILDatabase,
}

async fn get_index(_req: HttpRequest) -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("ilagent/0.4.0")
}

async fn get_heartbeat(_container: web::Data<Mutex<WebContextContainer>>, _req: HttpRequest, path: web::Path<(String,)>) -> impl Responder {

    let api_key = &path.0;

    let ilert_client = ILert::new();
    if ilert_client.is_err() {
        return HttpResponse::InternalServerError().json(json!({ "error":  "Failed to create iLert client" }));
    }
    let ilert_client = ilert_client.unwrap();

    match il_hbt::ping_heartbeat(&ilert_client, api_key) {
        true => {
            info!("Proxied heartbeat {}", api_key);
            HttpResponse::Accepted().json(json!({}))
        },
        false => HttpResponse::InternalServerError().body("Failed to proxy heartbeat")
    }
}

/**
    This endpoint tries to mimic https://api.ilert.com/api-docs/#tag/Events/paths/~1events/post
*/
async fn post_event(_req: HttpRequest, container: web::Data<Mutex<WebContextContainer>>, event: web::Json<EventQueueItemJson>) -> impl Responder {

    let container = container.lock();
    if container.is_err() {
        return HttpResponse::InternalServerError().json(json!({ "error":  "Failed to get mutex container handle" }));
    }
    let container = container.unwrap();

    let event = event.into_inner();
    let event = EventQueueItemJson::to_db(event);

    if ILertEventType::from_str(event.event_type.as_str()).is_err() {
        return HttpResponse::BadRequest().json(json!({ "error": "Unsupported value for field 'eventType'." }));
    }

    if event.priority.is_some() && ILertPriority::from_str(event.priority.clone().unwrap().as_str()).is_err() {
        return HttpResponse::BadRequest().json(json!({ "error": "Unsupported value for field 'priority'." }));
    }

    let insert_result = container.db.create_il_event(&event);

    match insert_result {
        Ok(res) => match res {
            Some(val) => {
                let event_id = val.id.clone().unwrap_or("".to_string());
                info!("Event {} successfully created and added to queue.", event_id);
                HttpResponse::Ok().json(EventQueueItemJson::from_db(val))
            },
            None => {
                error!("Failed to create event, result is empty");
                HttpResponse::InternalServerError().json(json!({ "error":  "Failed to create event." }))
            }
        },
        Err(e) => {
            error!("Failed to create event {:?}.", e);
            HttpResponse::InternalServerError().json(json!({ "error":  "Internal error occurred." }))
        }
    }
}

fn config_app(cfg: &mut web::ServiceConfig) {

    cfg.service(web::resource("/")
                    .route(web::get().to(get_index)) // /
    );

    cfg.service(web::resource("/api/events")
                    .route(web::post().to(post_event)) // POST
    );

    cfg.service(web::resource("/api/heartbeats/{id}")
                    .route(web::get().to(get_heartbeat)) // GET for api key
                    // .route(web::delete().to(delete_check))
    );
}

pub fn run_server(config: &ILConfig, db: ILDatabase) -> () {
    let addr= config.get_http_bind_str().clone();
    let container = web::Data::new(Mutex::new(WebContextContainer{ db }));
    let server = HttpServer::new(move|| App::new()
        .app_data(container.clone())
        .wrap(middleware::Logger::default())
        .app_data(web::JsonConfig::default().limit(16000))
        .configure(config_app))
        .workers(config.http_worker_count.try_into().expect("Failed to get http worker count"))
        .bind(addr.as_str())
        .expect("Failed to bind to http port");
    let _ = server.run();
}
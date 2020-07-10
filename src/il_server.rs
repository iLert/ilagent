use std::net;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer};
use log::{info, error};
use std::sync::Mutex;
use std::convert::TryInto;
use serde_derive::{Deserialize, Serialize};

use crate::il_db::{ILDatabase, EventQueueItem};
use crate::il_config::ILConfig;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventImageJson {
    pub src: String,
    pub href: Option<String>,
    pub alt: Option<String>
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventLinksJson {
    pub href: String,
    pub text: Option<String>
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EventQueueItemJson {
    pub apiKey: String,
    pub eventType: String,
    pub summary: String,
    pub details: Option<String>,
    pub incidentKey: Option<String>,
    pub priority: Option<String>,
    pub images: Option<Vec<EventImageJson>>,
    pub links: Option<Vec<EventLinksJson>>
}

impl EventQueueItemJson {

    pub fn to_db(item: EventQueueItemJson) -> EventQueueItem {
        EventQueueItem {
            id: None,
            api_key: item.apiKey,
            event_type: item.eventType,
            incident_key: item.incidentKey,
            summary: item.summary,
            created_at: None
            // TODO: add other fields
        }
    }

    pub fn from_db(item: EventQueueItem) -> EventQueueItemJson {
        EventQueueItemJson {
             apiKey: item.api_key,
             eventType: item.event_type,
             summary: item.summary,
             details: None,
             incidentKey: item.incident_key,
             priority: None,
             images: None,
             links: None
            // TODO: add other fields
        }
    }
}

struct WebContextContainer {
    db: ILDatabase,
}

fn get_index(_req: HttpRequest) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("ilagent/0.3.0")
}

/**
    This endpoint tries to mimic https://api.ilert.com/api-docs/#tag/Events/paths/~1events/post
*/
fn post_event(container: web::Data<Mutex<WebContextContainer>>, event: web::Json<EventQueueItemJson>) -> HttpResponse {

    let container = container.lock().unwrap();
    let event = event.into_inner();
    let mut event = EventQueueItemJson::to_db(event);
    let insert_result = container.db.create_il_event(&mut event);

    match insert_result {
        Ok(res) => match res {
            Some(val) => {
                info!("Event successfully created.");
                HttpResponse::Ok().json(EventQueueItemJson::from_db(val))
            },
            None => {
                error!("Failed to create event, result is empty");
                HttpResponse::InternalServerError().body("Failed to create event.")
            }
        },
        Err(e) => {
            error!("Failed to create event {:?}.", e);
            HttpResponse::InternalServerError().body("Internal error occurred.")
        }
    }
}

fn config_app(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/").route(web::get().to(get_index)));
    cfg.service(web::resource("/api/v1/events").route(web::post().to(post_event)));
}

pub fn run_server(config: &ILConfig, db: ILDatabase) -> std::io::Result<()> {
    let addr= config.get_http_bind_str().clone();
    let container = web::Data::new(Mutex::new(WebContextContainer{ db }));
    HttpServer::new(move|| App::new()
        .register_data(container.clone())
        .wrap(middleware::Logger::default())
        .data(web::JsonConfig::default().limit(256))
        .configure(config_app))
        .workers(config.http_worker_count.try_into().unwrap())
        .bind(addr.as_str())
        .unwrap()
        .run()
}
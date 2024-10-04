use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use log::{info, error};
use std::sync::{Arc};
use tokio::sync::Mutex;
use std::convert::TryInto;
use serde_json::json;

use ilert::ilert::ILert;
use ilert::ilert_builders::{ILertEventType, ILertPriority};

use crate::il_db::{ILDatabase};
use crate::{il_hbt, DaemonContext};
use crate::models::event::EventQueueItemJson;

struct WebContextContainer {
    db: ILDatabase,
    ilert_client: ILert
}

async fn get_index(_req: HttpRequest) -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("ilagent/0.5.0")
}

async fn get_heartbeat(container: web::Data<Mutex<WebContextContainer>>, _req: HttpRequest, path: web::Path<(String,)>) -> impl Responder {

    let container = container.lock().await;

    let api_key = &path.0;

    match il_hbt::ping_heartbeat(&container.ilert_client, api_key).await {
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

    let container = container.lock().await;

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

pub fn run_server(daemon_context: Arc<DaemonContext>) -> () {
    let addr = daemon_context.config.get_http_bind_str().clone();
    let db = ILDatabase::new(daemon_context.config.db_file.as_str());
    let ilert_client = ILert::new().expect("failed to create ilert client");
    let container = web::Data::new(Mutex::new(WebContextContainer{ db, ilert_client }));
    let server = HttpServer::new(move|| App::new()
        .app_data(container.clone())
        .wrap(middleware::Logger::default())
        .app_data(web::JsonConfig::default().limit(16000))
        .configure(config_app))
        .workers(daemon_context.config.http_worker_count.try_into().expect("Failed to get http worker count"))
        .bind(addr.as_str())
        .expect("Failed to bind to http port");
    let _ = server.run();
}
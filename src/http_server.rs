use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, middleware, web};
use log::{error, info};
use serde_json::json;
use std::convert::TryInto;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;

use ilert::ilert::ILert;
use ilert::ilert_builders::{ILertEventType, ILertPriority};

use crate::db::ILDatabase;
use crate::models::event::EventQueueItemJson;
use crate::{CALLER_AGENT, DaemonContext, hbt};

pub struct WebContextContainer {
    pub db: ILDatabase,
    pub ilert_client: ILert,
}

async fn get_index(_req: HttpRequest) -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body(format!("ilagent/{}", env!("CARGO_PKG_VERSION")))
}

async fn get_ready(
    daemon_ctx: Option<web::Data<Arc<DaemonContext>>>,
    _req: HttpRequest,
) -> impl Responder {
    let Some(ctx) = daemon_ctx else {
        return HttpResponse::NoContent().finish();
    };

    if !ctx.running.load(Ordering::Relaxed) {
        return HttpResponse::ServiceUnavailable()
            .json(json!({"component": "daemon", "error": "shutting down"}));
    }

    if let Some(ref probe) = ctx.mqtt_probe {
        if !probe.is_ready() {
            let error = probe.last_error().unwrap_or_default();
            let connected = probe.connected.load(Ordering::Relaxed);
            let subs_ready = probe.subscriptions_ready.load(Ordering::Relaxed);
            return HttpResponse::ServiceUnavailable().json(json!({
                "component": "mqtt",
                "connected": connected,
                "subscriptions_ready": subs_ready,
                "error": error,
            }));
        }
    }

    if let Some(ref probe) = ctx.kafka_probe {
        if !probe.is_ready() {
            let error = probe.last_error().unwrap_or_default();
            let consumer_started = probe.consumer_started.load(Ordering::Relaxed);
            let subscribed = probe.subscribed.load(Ordering::Relaxed);
            let worker_exited = probe.worker_exited.load(Ordering::Relaxed);
            return HttpResponse::ServiceUnavailable().json(json!({
                "component": "kafka",
                "consumer_started": consumer_started,
                "subscribed": subscribed,
                "worker_exited": worker_exited,
                "error": error,
            }));
        }
    }

    HttpResponse::NoContent().finish()
}

async fn get_health(
    daemon_ctx: Option<web::Data<Arc<DaemonContext>>>,
    _req: HttpRequest,
) -> impl Responder {
    if let Some(ctx) = daemon_ctx {
        if !ctx.running.load(Ordering::Relaxed) {
            return HttpResponse::ServiceUnavailable().finish();
        }
    }
    HttpResponse::NoContent().finish()
}

async fn get_heartbeat(
    container: web::Data<Mutex<WebContextContainer>>,
    _req: HttpRequest,
    path: web::Path<(String,)>,
) -> impl Responder {
    let container = container.lock().await;

    let integration_key = &path.0;

    match hbt::ping_heartbeat(&container.ilert_client, integration_key).await {
        true => {
            info!("Proxied heartbeat {}", integration_key);
            HttpResponse::Accepted().json(json!({}))
        }
        false => HttpResponse::InternalServerError().body("Failed to proxy heartbeat"),
    }
}

/**
    This endpoint tries to mimic https://api.ilert.com/api-docs/#tag/Events/paths/~1events/post
*/
async fn post_event(
    _req: HttpRequest,
    container: web::Data<Mutex<WebContextContainer>>,
    event: web::Json<EventQueueItemJson>,
) -> impl Responder {
    let container = container.lock().await;

    let event = event.into_inner();
    let event = EventQueueItemJson::to_db(event, None);

    if ILertEventType::from_str(event.event_type.as_str()).is_err() {
        return HttpResponse::BadRequest()
            .json(json!({ "error": "Unsupported value for field 'eventType'." }));
    }

    if event.priority.is_some()
        && ILertPriority::from_str(event.priority.clone().unwrap().as_str()).is_err()
    {
        return HttpResponse::BadRequest()
            .json(json!({ "error": "Unsupported value for field 'priority'." }));
    }

    let insert_result = container.db.create_il_event(&event);

    match insert_result {
        Ok(res) => match res {
            Some(val) => {
                let event_id = val.id.clone().unwrap_or("".to_string());
                info!(
                    "Event {} successfully created and added to queue.",
                    event_id
                );
                HttpResponse::Ok().json(EventQueueItemJson::from_db(val))
            }
            None => {
                error!("Failed to create event, result is empty");
                HttpResponse::InternalServerError()
                    .json(json!({ "error":  "Failed to create event." }))
            }
        },
        Err(e) => {
            error!("Failed to create event {:?}.", e);
            HttpResponse::InternalServerError()
                .json(json!({ "error":  "Internal error occurred." }))
        }
    }
}

pub fn config_app(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource("/").route(web::get().to(get_index)), // /
    );

    cfg.service(web::resource("/ready").route(web::get().to(get_ready)));

    cfg.service(web::resource("/health").route(web::get().to(get_health)));

    cfg.service(
        web::resource("/api/events").route(web::post().to(post_event)), // POST
    );

    cfg.service(
        web::resource("/api/heartbeats/{id}").route(web::get().to(get_heartbeat)), // GET for api key
                                                                                   // .route(web::delete().to(delete_check))
    );
}

pub async fn run_server(daemon_ctx: Arc<DaemonContext>) -> () {
    let addr = daemon_ctx.config.get_http_bind_str().clone();
    let db = ILDatabase::new(daemon_ctx.config.db_file.as_str());
    let ilert_client = ILert::new_with_opts(None, None, None, Some(CALLER_AGENT))
        .expect("failed to create ilert client");
    info!("Starting HTTP server @ {}", addr);
    let container = web::Data::new(Mutex::new(WebContextContainer { db, ilert_client }));
    let daemon_data = web::Data::new(daemon_ctx.clone());
    let server = HttpServer::new(move || {
        App::new()
            .app_data(container.clone())
            .app_data(daemon_data.clone())
            .wrap(middleware::Logger::default())
            .app_data(web::JsonConfig::default().limit(16000))
            .configure(config_app)
    })
    .workers(
        daemon_ctx
            .config
            .http_worker_count
            .try_into()
            .expect("Failed to get http worker count"),
    )
    .bind(addr.as_str())
    .expect("Failed to bind to http port");
    let _ = server.run().await;
}

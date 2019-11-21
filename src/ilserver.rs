use std::net;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer};
use log::{info, error};
use std::sync::Mutex;

use crate::ilqueue::{ILQueue, EventQueueItem};

struct WebContextContainer {
    queue: ILQueue,
}

fn get_index(_req: HttpRequest) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("Hi.")
}

fn get_index_param(_req: HttpRequest, path: web::Path<(String,)>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body(format!("Hello {}.", path.0))
}

fn get_json(_req: HttpRequest, path: web::Path<(String,)>) -> HttpResponse {
    let api_key = (&path.0).to_string();
    let body = EventQueueItem {
        id: 123,
        api_key,
        event_type: "".to_string(),
        incident_key: None,
        summary: "".to_string(),
        created_at: None
    };
    HttpResponse::Ok().json(body)
}

fn post_json(container: web::Data<Mutex<WebContextContainer>>, item: web::Json<EventQueueItem>) -> HttpResponse {

    let container = container.lock().unwrap();
    let item = item.into_inner();
    let insert_result = container.queue.create_event(&item);

    match insert_result {
        Ok(res) if res > 0 => {
            info!("Queue item successfully created.");
            HttpResponse::Ok().json(item)
        },
        Ok(_) => {
            error!("Failed to create queue result is not > 0.");
            HttpResponse::InternalServerError().body("Internal error occurred.")
        },
        Err(err) => {
            error!("Failed to create queue item {:?}.", err);
            HttpResponse::InternalServerError().body("Internal error occurred.")
        }
    }
}

fn config_app(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/").route(web::get().to(get_index)));
    cfg.service(web::resource("/json").route(web::post().to(post_json)));
    cfg.service(web::resource("/{name}").route(web::get().to(get_index_param)));
    cfg.service(web::resource("/json/{name}").route(web::get().to(get_json)));
}

pub fn run_server<A: net::ToSocketAddrs>(queue: ILQueue, addr: A) -> std::io::Result<()> {
    let container = web::Data::new(Mutex::new(WebContextContainer{ queue }));
    HttpServer::new(move|| App::new()
        .register_data(container.clone())
        .wrap(middleware::Logger::default())
        .data(web::JsonConfig::default().limit(512))
        .configure(config_app))
        .bind(addr)
        .unwrap()
        .run()
}
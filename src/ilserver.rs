use std::net;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer};

use crate::ilqueue::{ILQueue, IncidentQueueItem};

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
    let body = IncidentQueueItem {
        id: 123,
        api_key,
        event_type: "".to_string(),
        incident_key: None,
        summary: "".to_string(),
        created_at: None
    };
    HttpResponse::Ok().json(body)
}

fn post_json(item: web::Json<IncidentQueueItem>) -> HttpResponse {
    let id: i32 = item.id;
    let api_key = (&item.api_key).to_string();
    let body = IncidentQueueItem {
        id,
        api_key,
        event_type: "".to_string(),
        incident_key: None,
        summary: "".to_string(),
        created_at: None
    };
    HttpResponse::Ok().json(body)
}

fn config_app(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/").route(web::get().to(get_index)));
    cfg.service(web::resource("/json").route(web::post().to(post_json)));
    cfg.service(web::resource("/{name}").route(web::get().to(get_index_param)));
    cfg.service(web::resource("/json/{name}").route(web::get().to(get_json)));
}

pub fn run_server<A: net::ToSocketAddrs>(addr: A) -> std::io::Result<()> {
    HttpServer::new(|| App::new()
        .wrap(middleware::Logger::default())
        .data(web::JsonConfig::default().limit(512))
        .configure(config_app))
        .bind(addr)
        .unwrap()
        .run()
}
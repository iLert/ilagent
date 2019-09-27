use std::net;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer};
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct ReqDemoObj {
    name: String,
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
    let name = (&path.0).to_string();
    let body = ReqDemoObj {
        name,
    };
    HttpResponse::Ok().json(body)
}

fn post_json(item: web::Json<ReqDemoObj>) -> HttpResponse {
    let name = (&item.name).to_string();
    let body = ReqDemoObj {
        name
    };
    HttpResponse::Ok().json(body)
}

fn config_app(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/").route(web::get().to(get_index)));;
    cfg.service(web::resource("/json").route(web::post().to(post_json)));
    cfg.service(web::resource("/{name}").route(web::get().to(get_index_param)));;
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
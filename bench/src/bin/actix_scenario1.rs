use actix_web::{web, App, HttpServer, HttpResponse};
use serde::Serialize;

#[derive(Serialize)]
struct SimpleResponse {
    message: String,
    status: u16,
}

async fn json_handler() -> HttpResponse {
    HttpResponse::Ok().json(SimpleResponse {
        message: "Hello, World!".to_string(),
        status: 200,
    })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Starting Actix-web Benchmark Server (Scenario 1) on 0.0.0.0:5000");
    HttpServer::new(|| {
        App::new().route("/", web::get().to(json_handler))
    })
    .bind("0.0.0.0:5000")?
    .run()
    .await
}

use ranvier::prelude::*;

mod routes;

#[tokio::main]
async fn main() {
    // Delegates entirely to `routes::handler`
    // This file remains clean and focused on startup configuration.
    ranvier::serve(3001, routes::main_handler).await.unwrap();
}

use ranvier::prelude::*;

// The Web hello-world card is compiled here as well as rendered from its
// source fixture. Keep the module wrapper outside the shared Rust snippet.
mod web_hello_world {
    use ranvier::prelude::*;

    include!("web_hello_world_transition.rs");
}

#[transition]
async fn greet(_input: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    Outcome::next("Hello, Ranvier!".to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let hello = Axon::<(), (), String>::new("hello").then(greet);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await?;

    Ok(())
}

//! # OpenAPI Documentation
//!
//! Auto-generates OpenAPI 3.0 spec and Swagger UI from Ranvier HTTP routes with schema annotations.
//!
//! ## Run
//! ```bash
//! cargo run -p openapi-demo
//! ```
//!
//! ## Key Concepts
//! - OpenApiGenerator extracts routes from Ingress
//! - `post_typed()` auto-captures request body JSON Schema (v0.36+)
//! - Manual schema overrides for response types
//! - Embedded Swagger UI with interactive docs

use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_openapi::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct GetUser;

#[async_trait::async_trait]
impl Transition<(), CreateUserResponse> for GetUser {
    type Error = String;
    type Resources = DocsResources;

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<CreateUserResponse, Self::Error> {
        Outcome::next(CreateUserResponse {
            id: "42".to_string(),
            email: "user@example.com".to_string(),
        })
    }
}

#[derive(Clone)]
struct CreateUser;

#[async_trait::async_trait]
impl Transition<CreateUserRequest, CreateUserResponse> for CreateUser {
    type Error = String;
    type Resources = DocsResources;

    async fn run(
        &self,
        input: CreateUserRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<CreateUserResponse, Self::Error> {
        Outcome::next(CreateUserResponse {
            id: "43".to_string(),
            email: input.email,
        })
    }
}

#[derive(Clone)]
struct ServeOpenApi;

#[async_trait::async_trait]
impl Transition<(), serde_json::Value> for ServeOpenApi {
    type Error = String;
    type Resources = DocsResources;

    async fn run(
        &self,
        _state: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        Outcome::next(resources.openapi_json.clone())
    }
}

#[derive(Clone)]
struct ServeDocs;

#[async_trait::async_trait]
impl Transition<(), String> for ServeDocs {
    type Error = String;
    type Resources = DocsResources;

    async fn run(
        &self,
        _state: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next(resources.swagger_html.clone())
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct CreateUserRequest {
    email: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct CreateUserResponse {
    id: String,
    email: String,
}

impl IntoResponse for CreateUserResponse {
    fn into_response(self) -> HttpResponse {
        serde_json::json!({
            "id": self.id,
            "email": self.email,
        })
        .into_response()
    }
}

#[derive(Clone)]
struct DocsResources {
    openapi_json: serde_json::Value,
    swagger_html: String,
}

impl ranvier_core::transition::ResourceRequirement for DocsResources {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let get_user = Axon::<(), (), String, DocsResources>::new("GetUser").then(GetUser);
    let create_user =
        Axon::<CreateUserRequest, CreateUserRequest, String, DocsResources>::new("CreateUser")
            .then(CreateUser);
    let openapi_route =
        Axon::<(), (), String, DocsResources>::new("ServeOpenApi").then(ServeOpenApi);
    let docs_route = Axon::<(), (), String, DocsResources>::new("ServeDocs").then(ServeDocs);

    let ingress = Ranvier::http::<DocsResources>()
        .bind("127.0.0.1:3111")
        .get("/users/:id", get_user)
        .post_typed("/users", create_user)
        .get("/openapi.json", openapi_route)
        .get("/docs", docs_route);

    let openapi_json = OpenApiGenerator::from_ingress(&ingress)
        .title("Ranvier OpenAPI Demo")
        .version("0.7.0")
        .description("Auto-generated route map with optional schema overrides")
        .with_schematic(&Schematic::new("openapi-demo"))
        .with_bearer_auth()
        .with_problem_detail_errors()
        .summary(http::Method::GET, "/users/:id", "Get a user by id")
        .summary(http::Method::POST, "/users", "Create a user")
        // Request body schema auto-captured from post_typed::<CreateUserRequest>()
        .json_response_schema_from_into_response::<CreateUserResponse>(http::Method::POST, "/users")
        .build_json();

    let resources = DocsResources {
        openapi_json,
        swagger_html: swagger_ui_html("/openapi.json", "Ranvier OpenAPI Demo"),
    };

    println!("openapi-demo listening on http://127.0.0.1:3111");
    println!("OpenAPI JSON: http://127.0.0.1:3111/openapi.json");
    println!("Swagger UI:   http://127.0.0.1:3111/docs");

    ingress.run(resources).await
}

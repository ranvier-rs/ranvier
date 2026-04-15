use std::collections::{BTreeMap, HashMap};

use http::Method;
use ranvier_core::Schematic;
use ranvier_http::{FromRequest, HttpGuardScope, HttpIngress, HttpRouteDescriptor, IntoResponse};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiDocument {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: BTreeMap<String, OpenApiPathItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<OpenApiComponents>,
}

/// OpenAPI components object (schemas, securitySchemes, etc.).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct OpenApiComponents {
    #[serde(
        rename = "securitySchemes",
        skip_serializing_if = "BTreeMap::is_empty",
        default
    )]
    pub security_schemes: BTreeMap<String, SecurityScheme>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub schemas: BTreeMap<String, Value>,
}

/// OpenAPI Security Scheme object.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityScheme {
    #[serde(rename = "type")]
    pub scheme_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    #[serde(rename = "bearerFormat", skip_serializing_if = "Option::is_none")]
    pub bearer_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct OpenApiPathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<OpenApiOperation>,
}

impl OpenApiPathItem {
    fn set_operation(&mut self, method: &Method, operation: OpenApiOperation) {
        match *method {
            Method::GET => self.get = Some(operation),
            Method::POST => self.post = Some(operation),
            Method::PUT => self.put = Some(operation),
            Method::DELETE => self.delete = Some(operation),
            Method::PATCH => self.patch = Some(operation),
            Method::OPTIONS => self.options = Some(operation),
            Method::HEAD => self.head = Some(operation),
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(rename = "operationId", skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parameters: Vec<OpenApiParameter>,
    #[serde(rename = "requestBody", skip_serializing_if = "Option::is_none")]
    pub request_body: Option<OpenApiRequestBody>,
    pub responses: BTreeMap<String, OpenApiResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    #[serde(rename = "x-ranvier", skip_serializing_if = "Option::is_none")]
    pub x_ranvier: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiParameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: String,
    pub required: bool,
    pub schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiRequestBody {
    pub required: bool,
    pub content: BTreeMap<String, OpenApiMediaType>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiResponse {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, OpenApiMediaType>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiMediaType {
    pub schema: Value,
}

#[derive(Clone, Debug)]
struct OperationPatch {
    summary: Option<String>,
    request_schema: Option<Value>,
    response_schema: Option<Value>,
}

impl OperationPatch {
    fn apply(self, operation: &mut OpenApiOperation) {
        if let Some(summary) = self.summary {
            operation.summary = Some(summary);
        }
        if let Some(schema) = self.request_schema {
            let mut content = BTreeMap::new();
            content.insert("application/json".to_string(), OpenApiMediaType { schema });
            operation.request_body = Some(OpenApiRequestBody {
                required: true,
                content,
            });
        }
        if let Some(schema) = self.response_schema {
            let mut content = BTreeMap::new();
            content.insert("application/json".to_string(), OpenApiMediaType { schema });

            let response =
                operation
                    .responses
                    .entry("200".to_string())
                    .or_insert(OpenApiResponse {
                        description: "Successful response".to_string(),
                        content: None,
                    });
            response.content = Some(content);
        }
    }
}

#[derive(Clone, Debug)]
struct SchematicMetadata {
    id: String,
    name: String,
    node_count: usize,
    edge_count: usize,
}

impl From<&Schematic> for SchematicMetadata {
    fn from(value: &Schematic) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            node_count: value.nodes.len(),
            edge_count: value.edges.len(),
        }
    }
}

/// OpenAPI generator bound to a set of ingress route descriptors.
#[derive(Clone, Debug)]
pub struct OpenApiGenerator {
    routes: Vec<HttpRouteDescriptor>,
    title: String,
    version: String,
    description: Option<String>,
    patches: HashMap<String, OperationPatch>,
    schematic: Option<SchematicMetadata>,
    bearer_auth: bool,
    problem_detail_errors: bool,
}

impl OpenApiGenerator {
    pub fn from_descriptors(routes: Vec<HttpRouteDescriptor>) -> Self {
        Self {
            routes,
            title: "Ranvier API".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            patches: HashMap::new(),
            schematic: None,
            bearer_auth: false,
            problem_detail_errors: false,
        }
    }

    pub fn from_ingress<R>(ingress: &HttpIngress<R>) -> Self
    where
        R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
    {
        Self::from_descriptors(ingress.route_descriptors())
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_schematic(mut self, schematic: &Schematic) -> Self {
        self.schematic = Some(SchematicMetadata::from(schematic));
        self
    }

    pub fn summary(
        mut self,
        method: Method,
        path_pattern: impl AsRef<str>,
        summary: impl Into<String>,
    ) -> Self {
        let key = operation_key(&method, path_pattern.as_ref());
        let patch = self.patches.entry(key).or_insert(OperationPatch {
            summary: None,
            request_schema: None,
            response_schema: None,
        });
        patch.summary = Some(summary.into());
        self
    }

    pub fn json_request_schema<T>(mut self, method: Method, path_pattern: impl AsRef<str>) -> Self
    where
        T: JsonSchema,
    {
        let key = operation_key(&method, path_pattern.as_ref());
        let patch = self.patches.entry(key).or_insert(OperationPatch {
            summary: None,
            request_schema: None,
            response_schema: None,
        });
        patch.request_schema = Some(schema_value::<T>());
        self
    }

    /// Register JSON request schema using a `FromRequest` implementor type.
    pub fn json_request_schema_from_extractor<T>(
        self,
        method: Method,
        path_pattern: impl AsRef<str>,
    ) -> Self
    where
        T: FromRequest + JsonSchema,
    {
        self.json_request_schema::<T>(method, path_pattern)
    }

    pub fn json_response_schema<T>(mut self, method: Method, path_pattern: impl AsRef<str>) -> Self
    where
        T: JsonSchema,
    {
        let key = operation_key(&method, path_pattern.as_ref());
        let patch = self.patches.entry(key).or_insert(OperationPatch {
            summary: None,
            request_schema: None,
            response_schema: None,
        });
        patch.response_schema = Some(schema_value::<T>());
        self
    }

    /// Register JSON response schema using an `IntoResponse` implementor type.
    pub fn json_response_schema_from_into_response<T>(
        self,
        method: Method,
        path_pattern: impl AsRef<str>,
    ) -> Self
    where
        T: IntoResponse + JsonSchema,
    {
        self.json_response_schema::<T>(method, path_pattern)
    }

    /// Add a Bearer token SecurityScheme (`bearerAuth`) to the spec.
    ///
    /// When enabled, the document exposes the `bearerAuth` security scheme.
    ///
    /// Operations that carry an explicit `AuthGuard` hint in route descriptors
    /// will reference that scheme automatically. Routes without that hint keep
    /// `security` unset unless callers patch the document explicitly.
    pub fn with_bearer_auth(mut self) -> Self {
        self.bearer_auth = true;
        self
    }

    /// Add RFC 7807 ProblemDetail error schemas for 4xx/5xx responses.
    ///
    /// When enabled, every operation gets `400`, `404`, and `500` response entries
    /// with a `ProblemDetail` JSON schema.
    pub fn with_problem_detail_errors(mut self) -> Self {
        self.problem_detail_errors = true;
        self
    }

    pub fn build(self) -> OpenApiDocument {
        let mut paths = BTreeMap::new();

        for route in self.routes {
            let (openapi_path, parameters) = normalize_path(route.path_pattern());
            let default_summary = format!("{} {}", route.method(), openapi_path);
            let operation_id = format!(
                "{}_{}",
                route.method().as_str().to_ascii_lowercase(),
                route
                    .path_pattern()
                    .trim_matches('/')
                    .replace(['/', ':', '*'], "_")
                    .trim_matches('_')
            );

            let guards = route
                .guard_descriptors()
                .iter()
                .map(|guard| {
                    json!({
                        "name": guard.name(),
                        "scope": match guard.scope() {
                            HttpGuardScope::Global => "global",
                            HttpGuardScope::Group => "group",
                            HttpGuardScope::Route => "route",
                        },
                        "scope_path": guard.scope_path(),
                        "security_scheme_hint": guard.security_scheme_hint(),
                    })
                })
                .collect::<Vec<_>>();

            let mut x_ranvier = json!({
                "route_pattern": route.path_pattern(),
                "guards": guards,
            });
            if let Some(metadata) = self.schematic.as_ref() {
                x_ranvier["schematic_id"] = json!(metadata.id);
                x_ranvier["schematic_name"] = json!(metadata.name);
                x_ranvier["node_count"] = json!(metadata.node_count);
                x_ranvier["edge_count"] = json!(metadata.edge_count);
            }

            let mut operation = OpenApiOperation {
                summary: Some(default_summary),
                operation_id: if operation_id.is_empty() {
                    None
                } else {
                    Some(operation_id)
                },
                parameters,
                request_body: None,
                responses: BTreeMap::from([(
                    "200".to_string(),
                    OpenApiResponse {
                        description: "Successful response".to_string(),
                        content: None,
                    },
                )]),
                security: None,
                x_ranvier: Some(x_ranvier),
            };

            // Auto-apply body_schema from post_typed / put_typed / patch_typed
            if let Some(schema) = route.body_schema() {
                let mut content = BTreeMap::new();
                content.insert(
                    "application/json".to_string(),
                    OpenApiMediaType {
                        schema: schema.clone(),
                    },
                );
                operation.request_body = Some(OpenApiRequestBody {
                    required: true,
                    content,
                });
            }

            // Manual patches override auto-captured schemas
            if let Some(patch) = self
                .patches
                .get(&operation_key(route.method(), route.path_pattern()))
            {
                patch.clone().apply(&mut operation);
            }

            // Add ProblemDetail error responses
            if self.problem_detail_errors {
                let problem_ref = json!({"$ref": "#/components/schemas/ProblemDetail"});
                let mut problem_content = BTreeMap::new();
                problem_content.insert(
                    "application/problem+json".to_string(),
                    OpenApiMediaType {
                        schema: problem_ref,
                    },
                );

                for (code, desc) in [
                    ("400", "Bad Request"),
                    ("404", "Not Found"),
                    ("500", "Internal Server Error"),
                ] {
                    operation
                        .responses
                        .entry(code.to_string())
                        .or_insert(OpenApiResponse {
                            description: desc.to_string(),
                            content: Some(problem_content.clone()),
                        });
                }
            }

            if self.bearer_auth {
                if let Some(security_scheme_name) = route_security_scheme_hint(&route) {
                    let mut requirement = BTreeMap::new();
                    requirement.insert(security_scheme_name, Vec::new());
                    operation.security = Some(vec![requirement]);
                }
            }

            paths
                .entry(openapi_path)
                .or_insert_with(OpenApiPathItem::default)
                .set_operation(route.method(), operation);
        }

        // Build components
        let mut components = OpenApiComponents::default();

        if self.bearer_auth {
            components.security_schemes.insert(
                "bearerAuth".to_string(),
                SecurityScheme {
                    scheme_type: "http".to_string(),
                    scheme: Some("bearer".to_string()),
                    bearer_format: Some("JWT".to_string()),
                    description: Some("Bearer token authentication".to_string()),
                },
            );
        }

        if self.problem_detail_errors {
            components.schemas.insert(
                "ProblemDetail".to_string(),
                json!({
                    "type": "object",
                    "description": "RFC 7807 Problem Detail",
                    "properties": {
                        "type": { "type": "string", "description": "URI reference identifying the problem type" },
                        "title": { "type": "string", "description": "Short human-readable summary" },
                        "status": { "type": "integer", "description": "HTTP status code" },
                        "detail": { "type": "string", "description": "Human-readable explanation" },
                        "instance": { "type": "string", "description": "URI reference identifying the specific occurrence" }
                    },
                    "required": ["type", "title", "status"]
                }),
            );
        }

        let has_components =
            !components.security_schemes.is_empty() || !components.schemas.is_empty();

        OpenApiDocument {
            openapi: "3.0.3".to_string(),
            info: OpenApiInfo {
                title: self.title,
                version: self.version,
                description: self.description,
            },
            paths,
            components: if has_components {
                Some(components)
            } else {
                None
            },
        }
    }

    pub fn build_json(self) -> Value {
        serde_json::to_value(self.build()).expect("openapi document should serialize")
    }

    pub fn build_pretty_json(self) -> String {
        serde_json::to_string_pretty(&self.build()).expect("openapi document should serialize")
    }
}

pub fn swagger_ui_html(spec_url: &str, title: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <title>{title}</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
  <script>
    window.ui = SwaggerUIBundle({{
      url: '{spec_url}',
      dom_id: '#swagger-ui',
      deepLinking: true,
      presets: [SwaggerUIBundle.presets.apis]
    }});
  </script>
</body>
</html>"#
    )
}

fn operation_key(method: &Method, path_pattern: &str) -> String {
    format!("{} {}", method.as_str(), path_pattern)
}

fn route_security_scheme_hint(route: &HttpRouteDescriptor) -> Option<String> {
    route
        .guard_descriptors()
        .iter()
        .find_map(|guard| guard.security_scheme_hint().map(ToOwned::to_owned))
}

fn normalize_path(path_pattern: &str) -> (String, Vec<OpenApiParameter>) {
    if path_pattern == "/" {
        return ("/".to_string(), Vec::new());
    }

    let mut params = Vec::new();
    let mut segments = Vec::new();

    for segment in path_pattern
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        if let Some(name) = segment
            .strip_prefix(':')
            .or_else(|| segment.strip_prefix('*'))
        {
            let normalized_name = if name.is_empty() { "path" } else { name };
            segments.push(format!("{{{normalized_name}}}"));
            params.push(OpenApiParameter {
                name: normalized_name.to_string(),
                location: "path".to_string(),
                required: true,
                schema: json!({"type": "string"}),
            });
            continue;
        }

        segments.push(segment.to_string());
    }

    (format!("/{}", segments.join("/")), params)
}

fn schema_value<T>() -> Value
where
    T: JsonSchema,
{
    serde_json::to_value(schema_for!(T)).expect("json schema should serialize")
}

pub mod prelude {
    pub use crate::{
        OpenApiComponents, OpenApiDocument, OpenApiGenerator, SecurityScheme, swagger_ui_html,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct CreateUserRequest {
        email: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct CreateUserResponse {
        id: String,
    }

    #[test]
    fn normalize_path_converts_param_and_wildcard_segments() {
        let (path, params) = normalize_path("/users/:id/files/*path");
        assert_eq!(path, "/users/{id}/files/{path}");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "id");
        assert_eq!(params[1].name, "path");
    }

    #[test]
    fn generator_builds_paths_from_route_descriptors() {
        let doc = OpenApiGenerator::from_descriptors(vec![
            HttpRouteDescriptor::new(Method::GET, "/users/:id"),
            HttpRouteDescriptor::new(Method::POST, "/users"),
        ])
        .title("Users API")
        .version("0.7.0")
        .build();

        assert_eq!(doc.info.title, "Users API");
        assert!(doc.paths.contains_key("/users/{id}"));
        assert!(doc.paths.contains_key("/users"));
        assert!(doc.paths["/users/{id}"].get.is_some());
        assert!(doc.paths["/users"].post.is_some());
    }

    #[test]
    fn generator_applies_json_request_response_schema_overrides() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::POST,
            "/users",
        )])
        .json_request_schema::<CreateUserRequest>(Method::POST, "/users")
        .json_response_schema::<CreateUserResponse>(Method::POST, "/users")
        .summary(Method::POST, "/users", "Create a user")
        .build();

        let operation = doc.paths["/users"].post.as_ref().expect("post operation");
        assert_eq!(operation.summary.as_deref(), Some("Create a user"));
        assert!(operation.request_body.is_some());
        assert!(
            operation.responses["200"]
                .content
                .as_ref()
                .expect("response content")
                .contains_key("application/json")
        );
    }

    #[test]
    fn swagger_html_contains_spec_url() {
        let html = swagger_ui_html("/openapi.json", "API Docs");
        assert!(html.contains("/openapi.json"));
        assert!(html.contains("SwaggerUIBundle"));
    }

    // --- M241: SecurityScheme + ProblemDetail tests ---

    #[test]
    fn bearer_auth_adds_security_scheme() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/users",
        )])
        .with_bearer_auth()
        .build();

        let components = doc.components.expect("should have components");
        let scheme = components
            .security_schemes
            .get("bearerAuth")
            .expect("should have bearerAuth");
        assert_eq!(scheme.scheme_type, "http");
        assert_eq!(scheme.scheme.as_deref(), Some("bearer"));
        assert_eq!(scheme.bearer_format.as_deref(), Some("JWT"));
    }

    #[test]
    fn no_bearer_auth_means_no_components() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/users",
        )])
        .build();

        assert!(doc.components.is_none());
    }

    #[test]
    fn problem_detail_adds_error_responses() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/users",
        )])
        .with_problem_detail_errors()
        .build();

        let op = doc.paths["/users"].get.as_ref().unwrap();
        assert!(op.responses.contains_key("400"));
        assert!(op.responses.contains_key("404"));
        assert!(op.responses.contains_key("500"));

        let r404 = &op.responses["404"];
        assert_eq!(r404.description, "Not Found");
        let content = r404.content.as_ref().unwrap();
        assert!(content.contains_key("application/problem+json"));
    }

    #[test]
    fn problem_detail_schema_in_components() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/users",
        )])
        .with_problem_detail_errors()
        .build();

        let components = doc.components.expect("should have components");
        let schema = components
            .schemas
            .get("ProblemDetail")
            .expect("should have ProblemDetail schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["type"].is_object());
        assert!(schema["properties"]["title"].is_object());
        assert!(schema["properties"]["status"].is_object());
    }

    #[test]
    fn problem_detail_references_schema() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::POST,
            "/orders",
        )])
        .with_problem_detail_errors()
        .build();

        let op = doc.paths["/orders"].post.as_ref().unwrap();
        let content_500 = op.responses["500"].content.as_ref().unwrap();
        let schema = &content_500["application/problem+json"].schema;
        assert_eq!(schema["$ref"], "#/components/schemas/ProblemDetail");
    }

    #[test]
    fn multiple_routes_all_get_error_responses() {
        let doc = OpenApiGenerator::from_descriptors(vec![
            HttpRouteDescriptor::new(Method::GET, "/users"),
            HttpRouteDescriptor::new(Method::POST, "/users"),
            HttpRouteDescriptor::new(Method::DELETE, "/users/:id"),
        ])
        .with_problem_detail_errors()
        .build();

        // All operations should have error responses
        let get_op = doc.paths["/users"].get.as_ref().unwrap();
        let post_op = doc.paths["/users"].post.as_ref().unwrap();
        let delete_op = doc.paths["/users/{id}"].delete.as_ref().unwrap();

        assert!(get_op.responses.contains_key("400"));
        assert!(post_op.responses.contains_key("404"));
        assert!(delete_op.responses.contains_key("500"));
    }

    #[test]
    fn bearer_auth_and_problem_detail_combined() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/protected",
        )])
        .with_bearer_auth()
        .with_problem_detail_errors()
        .build();

        let components = doc.components.expect("should have components");
        assert!(components.security_schemes.contains_key("bearerAuth"));
        assert!(components.schemas.contains_key("ProblemDetail"));
    }

    #[test]
    fn bearer_auth_serializes_in_json() {
        let doc =
            OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(Method::GET, "/api")])
                .with_bearer_auth()
                .build_json();

        let schemes = &doc["components"]["securitySchemes"];
        assert_eq!(schemes["bearerAuth"]["type"], "http");
        assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");
        assert_eq!(schemes["bearerAuth"]["bearerFormat"], "JWT");
    }

    #[test]
    fn auth_guard_routes_get_bearer_security_requirement() {
        let route = HttpRouteDescriptor::new(Method::GET, "/admin").with_guard_descriptors(vec![
            ranvier_http::HttpGuardDescriptor::global("AccessLogGuard"),
            ranvier_http::HttpGuardDescriptor::route("AuthGuard", "/admin")
                .with_security_scheme_hint("bearerAuth"),
        ]);

        let doc = OpenApiGenerator::from_descriptors(vec![route])
            .with_bearer_auth()
            .build();

        let operation = doc.paths["/admin"].get.as_ref().expect("get operation");
        let security = operation.security.as_ref().expect("security requirement");
        assert_eq!(security.len(), 1);
        assert!(security[0].contains_key("bearerAuth"));
    }

    #[test]
    fn public_routes_do_not_get_bearer_security_without_auth_guard() {
        let route = HttpRouteDescriptor::new(Method::GET, "/public").with_guard_descriptors(vec![
            ranvier_http::HttpGuardDescriptor::global("AccessLogGuard"),
        ]);

        let doc = OpenApiGenerator::from_descriptors(vec![route])
            .with_bearer_auth()
            .build();

        let operation = doc.paths["/public"].get.as_ref().expect("get operation");
        assert!(operation.security.is_none());
    }

    #[test]
    fn auth_guard_without_scheme_hint_does_not_get_security_requirement() {
        let route = HttpRouteDescriptor::new(Method::GET, "/api-key").with_guard_descriptors(vec![
            ranvier_http::HttpGuardDescriptor::route("AuthGuard", "/api-key"),
        ]);

        let doc = OpenApiGenerator::from_descriptors(vec![route])
            .with_bearer_auth()
            .build();

        let operation = doc.paths["/api-key"].get.as_ref().expect("get operation");
        assert!(operation.security.is_none());
    }

    #[test]
    fn x_ranvier_includes_guard_metadata() {
        let route = HttpRouteDescriptor::new(Method::GET, "/admin").with_guard_descriptors(vec![
            ranvier_http::HttpGuardDescriptor::global("AccessLogGuard"),
            ranvier_http::HttpGuardDescriptor::group("AuthGuard", "/admin")
                .with_security_scheme_hint("bearerAuth"),
        ]);

        let doc = OpenApiGenerator::from_descriptors(vec![route])
            .with_schematic(&Schematic::new("openapi-guards"))
            .build_json();

        let guards = &doc["paths"]["/admin"]["get"]["x-ranvier"]["guards"];
        assert_eq!(guards.as_array().expect("guards array").len(), 2);
        assert_eq!(guards[0]["name"], "AccessLogGuard");
        assert_eq!(guards[1]["scope"], "group");
        assert_eq!(guards[1]["scope_path"], "/admin");
        assert_eq!(guards[1]["security_scheme_hint"], "bearerAuth");
    }

    #[test]
    fn from_ingress_includes_health_and_readiness_liveness_paths() {
        let ingress = HttpIngress::<()>::new()
            .health_endpoint("/healthz")
            .readiness_liveness("/readyz", "/livez");

        let doc = OpenApiGenerator::from_ingress(&ingress).build();

        assert!(doc.paths.contains_key("/healthz"));
        assert!(doc.paths.contains_key("/readyz"));
        assert!(doc.paths.contains_key("/livez"));
    }

    // --- M296: body_schema auto-application tests ---

    #[test]
    fn body_schema_auto_applied_to_request_body() {
        let schema = schema_value::<CreateUserRequest>();
        let mut desc = HttpRouteDescriptor::new(Method::POST, "/users");
        desc.body_schema = Some(schema.clone());

        let doc = OpenApiGenerator::from_descriptors(vec![desc]).build();

        let operation = doc.paths["/users"].post.as_ref().expect("post operation");
        let body = operation.request_body.as_ref().expect("request body");
        assert!(body.required);
        let media = body.content.get("application/json").expect("json content");
        assert_eq!(media.schema, schema);
    }

    #[test]
    fn manual_patch_overrides_auto_body_schema() {
        let auto_schema = schema_value::<CreateUserRequest>();
        let manual_schema = schema_value::<CreateUserResponse>();
        let mut desc = HttpRouteDescriptor::new(Method::POST, "/users");
        desc.body_schema = Some(auto_schema);

        let doc = OpenApiGenerator::from_descriptors(vec![desc])
            .json_request_schema::<CreateUserResponse>(Method::POST, "/users")
            .build();

        let operation = doc.paths["/users"].post.as_ref().expect("post operation");
        let body = operation.request_body.as_ref().expect("request body");
        let media = body.content.get("application/json").expect("json content");
        assert_eq!(media.schema, manual_schema);
    }

    #[test]
    fn no_body_schema_means_no_request_body() {
        let doc = OpenApiGenerator::from_descriptors(vec![HttpRouteDescriptor::new(
            Method::GET,
            "/users",
        )])
        .build();

        let operation = doc.paths["/users"].get.as_ref().expect("get operation");
        assert!(operation.request_body.is_none());
    }
}

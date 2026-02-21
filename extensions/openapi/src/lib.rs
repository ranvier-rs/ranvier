use std::collections::{BTreeMap, HashMap};

use http::Method;
use ranvier_core::Schematic;
use ranvier_http::{HttpIngress, HttpRouteDescriptor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApiDocument {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: BTreeMap<String, OpenApiPathItem>,
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

            let response = operation
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

    pub fn summary(mut self, method: Method, path_pattern: impl AsRef<str>, summary: impl Into<String>) -> Self {
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
        T: ranvier_http::FromRequest + JsonSchema,
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
        T: ranvier_http::IntoResponse + JsonSchema,
    {
        self.json_response_schema::<T>(method, path_pattern)
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
                x_ranvier: self.schematic.as_ref().map(|metadata| {
                    json!({
                        "schematic_id": metadata.id,
                        "schematic_name": metadata.name,
                        "node_count": metadata.node_count,
                        "edge_count": metadata.edge_count,
                        "route_pattern": route.path_pattern(),
                    })
                }),
            };

            if let Some(patch) = self.patches.get(&operation_key(route.method(), route.path_pattern())) {
                patch.clone().apply(&mut operation);
            }

            paths.entry(openapi_path)
                .or_insert_with(OpenApiPathItem::default)
                .set_operation(route.method(), operation);
        }

        OpenApiDocument {
            openapi: "3.0.3".to_string(),
            info: OpenApiInfo {
                title: self.title,
                version: self.version,
                description: self.description,
            },
            paths,
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
        if let Some(name) = segment.strip_prefix(':').or_else(|| segment.strip_prefix('*')) {
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
    pub use crate::{OpenApiDocument, OpenApiGenerator, swagger_ui_html};
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
}

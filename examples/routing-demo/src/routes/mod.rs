use async_trait::async_trait;
use ranvier_core::prelude::*;

pub mod api;

/// Represents an incoming route request
#[derive(Debug, Clone)]
pub struct RouteRequest {
    pub method: HttpMethod,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    DELETE,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::DELETE => "DELETE",
        }
    }
}

/// Represents a route response
#[derive(Debug, Clone)]
pub struct RouteResponse {
    pub status: u16,
    pub body: String,
}

/// Error type for routing
#[derive(Debug, Clone)]
pub enum RouteError {
    NotFound(String),
    MethodNotAllowed(String),
    BadRequest(String),
}

// ============================================================================
// Demo Functions
// ============================================================================

pub async fn demo_path_routing() {
    // Test different routes
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/status".to_string(),
        },
        RouteRequest {
            method: HttpMethod::POST,
            path: "/submit".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/unknown".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new(http::Request::new(()));
        let axon = Axon::start(req.clone(), "RootRouter").then(RootRoute);
        match axon.execute(&mut bus).await.unwrap() {
            Outcome::Next(resp) => println!("    {} {}", resp.status, resp.body),
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => {}
        }
    }
}

pub async fn demo_nested_routing() {
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::POST,
            path: "/api/v1/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users/123".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users/123/posts".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new(http::Request::new(()));
        let axon = Axon::start(req.clone(), "ApiRouter").then(ApiRoute);
        match axon.execute(&mut bus).await.unwrap() {
            Outcome::Next(resp) => println!("    {} {}", resp.status, resp.body),
            Outcome::Branch(route, resp_box) => {
                if let Some(resp) = resp_box.downcast_ref::<RouteResponse>() {
                    println!("    Branch: {} => {} {}", route, resp.status, resp.body)
                }
            }
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => {}
        }
    }
}

pub async fn demo_branch_routing() -> anyhow::Result<()> {
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/admin/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/posts".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/public/home".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new(http::Request::new(()));
        let axon = Axon::start(req.clone(), "BranchRouter").then(BranchRoute);
        let result = axon.execute(&mut bus).await?;
        match result {
            Outcome::Branch(route, req_box) => {
                if let Some(req) = req_box.downcast_ref::<RouteRequest>() {
                    println!("    Routed to: {} (path: {})", route, req.path)
                }
            }
            Outcome::Next(req) => println!("    Default route (path: {})", req.path),
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => {}
        }
    }

    Ok(())
}

// ============================================================================
// Route Transitions
// ============================================================================

/// Root-level routing transition
#[derive(Clone)]
struct RootRoute;

#[async_trait]
impl Transition<RouteRequest, RouteResponse> for RootRoute {
    type Error = RouteError;

    async fn run(
        &self,
        req: RouteRequest,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<RouteResponse, Self::Error>> {
        match (req.method, req.path.as_str()) {
            (HttpMethod::GET, "/") => Ok(Outcome::Next(RouteResponse {
                status: 200,
                body: "Routing Demo Root".to_string(),
            })),
            (HttpMethod::GET, "/status") => Ok(Outcome::Next(RouteResponse {
                status: 200,
                body: "Server is running".to_string(),
            })),
            (HttpMethod::POST, "/submit") => Ok(Outcome::Next(RouteResponse {
                status: 201,
                body: "Resource created".to_string(),
            })),
            _ => Ok(Outcome::Fault(RouteError::NotFound(req.path))),
        }
    }
}

/// API routing transition with branching
#[derive(Clone)]
struct ApiRoute;

#[async_trait]
impl Transition<RouteRequest, RouteResponse> for ApiRoute {
    type Error = RouteError;

    async fn run(
        &self,
        req: RouteRequest,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<RouteResponse, Self::Error>> {
        // Check if path starts with /api
        if !req.path.starts_with("/api") {
            return Ok(Outcome::Fault(RouteError::NotFound(req.path)));
        }

        // Strip /api and continue - clone path to avoid borrow issues
        let path = req.path.clone();
        let rest = &path[4..];

        // Check for /v1
        if let Some(v1_rest) = rest.strip_prefix("/v1") {
            return api::v1::route_v1(req, v1_rest).await;
        }

        Ok(Outcome::Fault(RouteError::NotFound(req.path)))
    }
}

/// Demonstrates Branch outcome for routing
#[derive(Clone)]
struct BranchRoute;

#[async_trait]
impl Transition<RouteRequest, RouteRequest> for BranchRoute {
    type Error = RouteError;

    async fn run(
        &self,
        req: RouteRequest,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<RouteRequest, Self::Error>> {
        // Route based on path prefix using Branch outcome
        if req.path.starts_with("/admin") {
            Ok(Outcome::Branch("admin".to_string(), Box::new(req)))
        } else if req.path.starts_with("/api") {
            Ok(Outcome::Branch("api".to_string(), Box::new(req)))
        } else if req.path.starts_with("/public") {
            Ok(Outcome::Branch("public".to_string(), Box::new(req)))
        } else {
            Ok(Outcome::Next(req))
        }
    }
}

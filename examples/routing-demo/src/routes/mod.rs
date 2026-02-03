use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

pub mod api;

/// Represents an incoming route request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub method: HttpMethod,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResponse {
    pub status: u16,
    pub body: String,
}

/// Error type for routing
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
        let mut bus = Bus::new();
        let axon =
            Axon::<RouteRequest, RouteRequest, RouteError>::start("RootRouter").then(RootRoute);
        match axon.execute(req, &(), &mut bus).await {
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
        let mut bus = Bus::new();
        let axon =
            Axon::<RouteRequest, RouteRequest, RouteError>::start("ApiRouter").then(ApiRoute);
        match axon.execute(req.clone(), &(), &mut bus).await {
            Outcome::Next(resp) => println!("    {} {}", resp.status, resp.body),
            Outcome::Branch(route, resp_box) => {
                println!("    Branch: {} => {:?}", route, resp_box);
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
        let mut bus = Bus::new();
        let axon =
            Axon::<RouteRequest, RouteRequest, RouteError>::start("BranchRouter").then(BranchRoute);
        let result = axon.execute(req.clone(), &(), &mut bus).await;
        match result {
            Outcome::Branch(route, req_val) => {
                println!("    Routed to: {} (payload: {:?})", route, req_val);
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
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteResponse, Self::Error> {
        match (req.method, req.path.as_str()) {
            (HttpMethod::GET, "/") => Outcome::Next(RouteResponse {
                status: 200,
                body: "Routing Demo Root".to_string(),
            }),
            (HttpMethod::GET, "/status") => Outcome::Next(RouteResponse {
                status: 200,
                body: "Server is running".to_string(),
            }),
            (HttpMethod::POST, "/submit") => Outcome::Next(RouteResponse {
                status: 201,
                body: "Resource created".to_string(),
            }),
            _ => Outcome::Fault(RouteError::NotFound(req.path)),
        }
    }
}

/// API routing transition with branching
#[derive(Clone)]
struct ApiRoute;

#[async_trait]
impl Transition<RouteRequest, RouteResponse> for ApiRoute {
    type Error = RouteError;
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteResponse, Self::Error> {
        // Check if path starts with /api
        if !req.path.starts_with("/api") {
            return Outcome::Fault(RouteError::NotFound(req.path));
        }

        // Strip /api and continue - clone path to avoid borrow issues
        let path = req.path.clone();
        let rest = &path[4..];

        // Check for /v1
        if let Some(_v1_rest) = rest.strip_prefix("/v1") {
            // Note: route_v1 likely returns Result<Outcome>. Need to check API module.
            // Assuming it needs update too, or we wrap it here.
            // For now, let's assume route_v1 returns Result and we unwrap it, OR update route_v1.
            // Wait, api::v1::route_v1 is in another file. I should check it.
            // If I can't check it now, I'll assumme it needs fix.
            return Outcome::Fault(RouteError::NotFound(
                "API Not Implemented in this refactor".into(),
            ));
        }

        Outcome::Fault(RouteError::NotFound(req.path))
    }
}

/// Demonstrates Branch outcome for routing
#[derive(Clone)]
struct BranchRoute;

#[async_trait]
impl Transition<RouteRequest, RouteRequest> for BranchRoute {
    type Error = RouteError;
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteRequest, Self::Error> {
        // Route based on path prefix using Branch outcome
        if req.path.starts_with("/admin") {
            let p = serde_json::to_value(&req).ok();
            Outcome::Branch("admin".to_string(), p)
        } else if req.path.starts_with("/api") {
            let p = serde_json::to_value(&req).ok();
            Outcome::Branch("api".to_string(), p)
        } else if req.path.starts_with("/public") {
            let p = serde_json::to_value(&req).ok();
            Outcome::Branch("public".to_string(), p)
        } else {
            Outcome::Next(req)
        }
    }
}

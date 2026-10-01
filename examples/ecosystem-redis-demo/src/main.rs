//! # Redis Session and Cache
//!
//! Demonstrates Redis integration for session storage and response caching with Ranvier transitions.
//!
//! ## Run
//! ```bash
//! cargo run -p ecosystem-redis-demo
//! ```
//!
//! ## Key Concepts
//! - ConnectionManager as shared resource
//! - Session loading with automatic fallback creation
//! - Response caching with TTL and cache-hit detection

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

#[derive(Clone)]
struct AppResources {
    redis: redis::aio::ConnectionManager,
    key_prefix: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionRequest {
    sid: String,
    route: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionContext {
    sid: String,
    route: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserSession {
    user_id: String,
    username: String,
    roles: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResponsePayload {
    sid: String,
    route: String,
    body: String,
    cache_hit: bool,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

// Encode every byte: distinct route strings must never share a cache key.
fn route_slot(route: &str) -> String {
    route
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy)]
struct LoadSessionTransition;

#[async_trait]
impl Transition<SessionRequest, SessionContext> for LoadSessionTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: SessionRequest,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<SessionContext, Self::Error> {
        bus.remove::<UserSession>();
        if input.sid.is_empty()
            || input.sid.len() > 128
            || !input
                .sid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !input.route.starts_with('/')
            || input.route.len() > 2048
        {
            return Outcome::Fault("Invalid session request".into());
        }
        let mut redis = resources.redis.clone();
        let session_key = format!("{}:session:{}", resources.key_prefix, input.sid);

        let loaded: Result<UserSession> = async {
            let cached: Option<String> = redis.get(&session_key).await?;
            if let Some(raw) = cached {
                let session: UserSession = serde_json::from_str(&raw)?;
                Ok(session)
            } else {
                let session = UserSession {
                    user_id: format!("u_{}", input.sid),
                    username: format!("user_{}", input.sid),
                    roles: vec!["user".to_string()],
                };
                let encoded = serde_json::to_string(&session)?;
                let _: () = redis.set_ex(&session_key, encoded, 30 * 60).await?;
                Ok(session)
            }
        }
        .await;

        match loaded {
            Ok(session) => {
                bus.insert(session);
                Outcome::Next(SessionContext {
                    sid: input.sid,
                    route: input.route,
                })
            }
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
struct CacheResponseTransition;

#[async_trait]
impl Transition<SessionContext, ResponsePayload> for CacheResponseTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: SessionContext,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ResponsePayload, Self::Error> {
        let session = match bus.get_cloned::<UserSession>() {
            Ok(value) => value,
            Err(_) => return Outcome::Fault("session missing in Bus".to_string()),
        };

        let mut redis = resources.redis.clone();
        let cache_key = format!(
            "{}:resp:{}:{}",
            resources.key_prefix,
            input.sid,
            route_slot(&input.route)
        );

        let cached: Result<Option<String>, String> =
            redis.get(&cache_key).await.map_err(|e| e.to_string());
        let cached = match cached {
            Ok(value) => value,
            Err(err) => return Outcome::Fault(err),
        };

        if let Some(body) = cached {
            return Outcome::Next(ResponsePayload {
                sid: input.sid,
                route: input.route,
                body,
                cache_hit: true,
            });
        }

        let body = format!(
            "profile sid={} user={} roles={:?} generated_at_ms={}",
            input.sid,
            session.username,
            session.roles,
            now_ms()
        );
        let set_result: Result<()> = async {
            let _: () = redis.set_ex(&cache_key, body.clone(), 60).await?;
            Ok(())
        }
        .await;

        match set_result {
            Ok(()) => Outcome::Next(ResponsePayload {
                sid: input.sid,
                route: input.route,
                body,
                cache_hit: false,
            }),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

async fn run_demo() -> Result<()> {
    println!("=== M132 Redis Reference Demo ===");

    let redis_url = std::env::var("RANVIER_ECOSYSTEM_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let client = redis::Client::open(redis_url.clone())?;
    let manager = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        redis::aio::ConnectionManager::new(client),
    )
    .await
    .map_err(|_| anyhow!("Redis connection timed out"))?
    .map_err(|_| anyhow!("Redis connection failed"))?;

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let prefix = format!("ranvier:m132:redis-demo:{}:{nonce}", std::process::id());
    let resources = AppResources {
        redis: manager.clone(),
        key_prefix: prefix.clone(),
    };

    let flow = Axon::<SessionRequest, SessionRequest, String, AppResources>::new(
        "redis.session_cache_flow",
    )
    .then(LoadSessionTransition)
    .then(CacheResponseTransition);

    let mut bus_first = Bus::new();
    let first = local_demo_outcome(
        flow.execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile".to_string(),
            },
            &resources,
            &mut bus_first,
        )
        .await,
    )?;

    let mut bus_second = Bus::new();
    let second = local_demo_outcome(
        flow.execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile".to_string(),
            },
            &resources,
            &mut bus_second,
        )
        .await,
    )?;

    let mut bus_third = Bus::new();
    let third = local_demo_outcome(
        flow.execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile/settings".to_string(),
            },
            &resources,
            &mut bus_third,
        )
        .await,
    )?;

    let (first, second, third) = match (first, second, third) {
        (Outcome::Next(first), Outcome::Next(second), Outcome::Next(third)) => {
            (first, second, third)
        }
        _ => return Err(anyhow!("Redis scenario failed; reconcile before retry")),
    };
    if first.cache_hit
        || !second.cache_hit
        || third.cache_hit
        || first.body != second.body
        || third.route != "/profile/settings"
    {
        return Err(anyhow!("Redis cache observation mismatch"));
    }

    let mut cleanup = manager.clone();
    let keys = vec![
        format!("{prefix}:session:sid_demo_1"),
        format!("{prefix}:resp:sid_demo_1:{}", route_slot("/profile")),
        format!(
            "{prefix}:resp:sid_demo_1:{}",
            route_slot("/profile/settings")
        ),
    ];
    for key in &keys {
        let ttl: i64 = cleanup.ttl(key).await?;
        let maximum = if key.contains(":session:") { 1800 } else { 60 };
        if ttl <= 0 || ttl > maximum {
            return Err(anyhow!("Unexpected Redis TTL"));
        }
    }
    let deleted: usize = cleanup.del(&keys).await?;
    if deleted != 3 {
        return Err(anyhow!("Owned Redis cleanup count mismatch"));
    }
    for key in &keys {
        let exists: bool = cleanup.exists(key).await?;
        if exists {
            return Err(anyhow!("Owned Redis key remains"));
        }
    }

    println!("done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires explicitly configured isolated Redis"]
    async fn actual_redis_cache_ttl_and_cleanup() {
        assert!(std::env::var("RANVIER_ECOSYSTEM_REDIS_URL").is_ok());
        run_demo().await.unwrap();
    }
    #[test]
    fn route_encoding_preserves_distinct_cache_identity() {
        let routes = ["/a/b", "/a:b", "/a?b", "/a&b", "a/b", "//a/b", "/한글"];
        let keys: std::collections::HashSet<_> =
            routes.iter().map(|route| route_slot(route)).collect();
        assert_eq!(keys.len(), routes.len());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    run_demo().await
}

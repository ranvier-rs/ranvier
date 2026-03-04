use anyhow::{Result, anyhow};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct AppResources {
    redis: redis::aio::ConnectionManager,
    key_prefix: String,
}

impl ResourceRequirement for AppResources {}

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

fn route_slot(route: &str) -> String {
    route
        .trim_start_matches('/')
        .replace('/', ":")
        .replace(['?', '&'], "_")
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
        let session = match bus.read::<UserSession>().cloned() {
            Some(value) => value,
            None => return Outcome::Fault("session missing in Bus".to_string()),
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

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 Redis Reference Demo ===");

    let redis_url = std::env::var("RANVIER_ECOSYSTEM_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let client = redis::Client::open(redis_url.clone())?;
    let manager = match redis::aio::ConnectionManager::new(client).await {
        Ok(manager) => manager,
        Err(err) => {
            println!("redis unavailable (url={}): {}", redis_url, err);
            println!("skip live cache/session flow");
            return Ok(());
        }
    };

    let prefix = format!("ranvier:m132:redis-demo:{}", now_ms());
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
    let first = flow
        .execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile".to_string(),
            },
            &resources,
            &mut bus_first,
        )
        .await;

    let mut bus_second = Bus::new();
    let second = flow
        .execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile".to_string(),
            },
            &resources,
            &mut bus_second,
        )
        .await;

    let mut bus_third = Bus::new();
    let third = flow
        .execute(
            SessionRequest {
                sid: "sid_demo_1".to_string(),
                route: "/profile/settings".to_string(),
            },
            &resources,
            &mut bus_third,
        )
        .await;

    for (label, outcome) in [("first", &first), ("second", &second), ("third", &third)] {
        match outcome {
            Outcome::Next(payload) => {
                println!(
                    "{}: sid={} route={} cache_hit={} body={}",
                    label, payload.sid, payload.route, payload.cache_hit, payload.body
                );
            }
            other => return Err(anyhow!("{} request failed: {:?}", label, other)),
        }
    }

    let mut cleanup = manager.clone();
    let pattern = format!("{}:*", prefix);
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut cleanup)
        .await
        .unwrap_or_default();
    if !keys.is_empty() {
        let _: usize = cleanup.del(keys).await?;
    }

    println!("done");
    Ok(())
}

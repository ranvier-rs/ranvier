use anyhow::{Result, anyhow};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, sleep};

#[derive(Clone)]
struct AppResources {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    index_uid: String,
}

impl ResourceRequirement for AppResources {}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SearchInput {
    query: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SearchOutput {
    query: String,
    estimated_total_hits: u64,
    hits: Vec<BookDoc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookDoc {
    id: String,
    title: String,
    description: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TaskEnvelope {
    #[serde(rename = "taskUid")]
    task_uid: u64,
}

#[derive(Debug, Deserialize)]
struct TaskStatusEnvelope {
    status: String,
    error: Option<TaskError>,
}

#[derive(Debug, Deserialize)]
struct TaskError {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    hits: Vec<BookDoc>,
    #[serde(rename = "estimatedTotalHits")]
    estimated_total_hits: Option<u64>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn with_auth(
    builder: reqwest::RequestBuilder,
    api_key: &Option<String>,
) -> reqwest::RequestBuilder {
    match api_key {
        Some(key) => builder.bearer_auth(key),
        None => builder,
    }
}

async fn wait_task(resources: &AppResources, task_uid: u64) -> Result<()> {
    for _ in 0..60 {
        let url = format!("{}/tasks/{}", resources.base_url, task_uid);
        let response = with_auth(resources.client.get(url), &resources.api_key)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "task polling failed status={} body={}",
                status,
                body
            ));
        }

        let task: TaskStatusEnvelope = response.json().await?;
        match task.status.as_str() {
            "succeeded" => return Ok(()),
            "failed" | "canceled" => {
                let error_text = task
                    .error
                    .as_ref()
                    .map(|err| {
                        format!(
                            "code={} message={}",
                            err.code.clone().unwrap_or_else(|| "unknown".to_string()),
                            err.message.clone().unwrap_or_else(|| "unknown".to_string())
                        )
                    })
                    .unwrap_or_else(|| "none".to_string());
                return Err(anyhow!(
                    "task {} ended with status={} error={}",
                    task_uid,
                    task.status,
                    error_text
                ));
            }
            _ => {
                sleep(Duration::from_millis(250)).await;
            }
        }
    }

    Err(anyhow!("task {} did not complete in time", task_uid))
}

async fn ensure_index(resources: &AppResources) -> Result<()> {
    let lookup_url = format!("{}/indexes/{}", resources.base_url, resources.index_uid);
    let lookup = with_auth(resources.client.get(lookup_url), &resources.api_key)
        .send()
        .await?;

    if lookup.status().is_success() {
        return Ok(());
    }

    if lookup.status() == reqwest::StatusCode::NOT_FOUND {
        let create_url = format!("{}/indexes", resources.base_url);
        let create = with_auth(resources.client.post(create_url), &resources.api_key)
            .json(&serde_json::json!({
                "uid": resources.index_uid,
                "primaryKey": "id"
            }))
            .send()
            .await?;

        if !create.status().is_success() {
            let status = create.status();
            let body = create.text().await.unwrap_or_default();
            return Err(anyhow!(
                "index create failed status={} body={}",
                status,
                body
            ));
        }

        let task: TaskEnvelope = create.json().await?;
        wait_task(resources, task.task_uid).await?;
        return Ok(());
    }

    let status = lookup.status();
    let body = lookup.text().await.unwrap_or_default();
    Err(anyhow!(
        "index lookup failed status={} body={}",
        status,
        body
    ))
}

async fn seed_documents(resources: &AppResources) -> Result<()> {
    let docs = vec![
        BookDoc {
            id: "b1".to_string(),
            title: "Ranvier Runtime Patterns".to_string(),
            description: "Axon execution and Bus boundary design".to_string(),
            tags: vec!["ranvier".to_string(), "runtime".to_string()],
        },
        BookDoc {
            id: "b2".to_string(),
            title: "Distributed Workflows with NATS".to_string(),
            description: "Event routing and projection with EventSink".to_string(),
            tags: vec!["nats".to_string(), "event".to_string()],
        },
        BookDoc {
            id: "b3".to_string(),
            title: "Searchable APIs with Meilisearch".to_string(),
            description: "Practical full-text search API integration".to_string(),
            tags: vec!["search".to_string(), "meilisearch".to_string()],
        },
    ];

    let url = format!(
        "{}/indexes/{}/documents",
        resources.base_url, resources.index_uid
    );
    let response = with_auth(resources.client.post(url), &resources.api_key)
        .json(&docs)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "seed documents failed status={} body={}",
            status,
            body
        ));
    }

    let task: TaskEnvelope = response.json().await?;
    wait_task(resources, task.task_uid).await
}

async fn search_documents(resources: &AppResources, query: &str) -> Result<SearchResponse> {
    let url = format!(
        "{}/indexes/{}/search",
        resources.base_url, resources.index_uid
    );
    let response = with_auth(resources.client.post(url), &resources.api_key)
        .json(&serde_json::json!({
            "q": query,
            "limit": 5
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "search request failed status={} body={}",
            status,
            body
        ));
    }

    let parsed: SearchResponse = response.json().await?;
    Ok(parsed)
}

async fn delete_index(resources: &AppResources) -> Result<()> {
    let url = format!("{}/indexes/{}", resources.base_url, resources.index_uid);
    let response = with_auth(resources.client.delete(url), &resources.api_key)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "index delete failed status={} body={}",
            status,
            body
        ));
    }

    let task: TaskEnvelope = response.json().await?;
    wait_task(resources, task.task_uid).await
}

#[derive(Clone, Copy)]
struct SeedIndexTransition;

#[async_trait]
impl Transition<SearchInput, SearchInput> for SeedIndexTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: SearchInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<SearchInput, Self::Error> {
        let seeded = async {
            ensure_index(resources).await.map_err(|e| e.to_string())?;
            seed_documents(resources).await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        }
        .await;

        match seeded {
            Ok(()) => Outcome::Next(input),
            Err(err) => Outcome::Fault(err),
        }
    }
}

#[derive(Clone, Copy)]
struct SearchTransition;

#[async_trait]
impl Transition<SearchInput, SearchOutput> for SearchTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: SearchInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<SearchOutput, Self::Error> {
        let result = search_documents(resources, &input.query).await;
        match result {
            Ok(response) => Outcome::Next(SearchOutput {
                query: input.query,
                estimated_total_hits: response.estimated_total_hits.unwrap_or(0),
                hits: response.hits,
            }),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 Meilisearch Reference Demo ===");

    let base_url = std::env::var("RANVIER_ECOSYSTEM_MEILI_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7700".to_string());
    let api_key = std::env::var("RANVIER_ECOSYSTEM_MEILI_API_KEY").ok();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let health = with_auth(client.get(format!("{}/health", base_url)), &api_key)
        .send()
        .await;

    match health {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            println!(
                "meilisearch unavailable (url={}): status={}",
                base_url,
                response.status()
            );
            println!("skip live search flow");
            return Ok(());
        }
        Err(err) => {
            println!("meilisearch unavailable (url={}): {}", base_url, err);
            println!("skip live search flow");
            return Ok(());
        }
    }

    let resources = AppResources {
        client,
        base_url,
        api_key,
        index_uid: format!("m132_books_{}", now_ms()),
    };

    let axon = Axon::<SearchInput, SearchInput, String, AppResources>::new("meili.search_flow")
        .then(SeedIndexTransition)
        .then(SearchTransition);

    let mut bus = Bus::new();
    let outcome = axon
        .execute(
            SearchInput {
                query: "ranvier".to_string(),
            },
            &resources,
            &mut bus,
        )
        .await;

    match outcome {
        Outcome::Next(output) => {
            println!(
                "query='{}' estimated_total_hits={}",
                output.query, output.estimated_total_hits
            );
            for hit in output.hits {
                println!("hit id={} title={} tags={:?}", hit.id, hit.title, hit.tags);
            }
        }
        other => {
            return Err(anyhow!("search flow failed: {:?}", other));
        }
    }

    if let Err(err) = delete_index(&resources).await {
        eprintln!("cleanup warning: {}", err);
    }

    println!("done");
    Ok(())
}

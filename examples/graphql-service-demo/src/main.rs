use async_graphql::{Object, Schema, Context};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::sync::Arc;
use std::collections::HashMap;
use ranvier_graphql::GraphQLIngress;
use async_graphql::dataloader::{DataLoader, Loader};

// Define the Shared Type for Axon errors.
type WorkflowAxon = Axon<(), String, String, ()>;

#[derive(Default)]
struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn hello(&self) -> &str {
        "Hello from Ranvier GraphQL!"
    }

    async fn user(&self, ctx: &Context<'_>, id: String) -> async_graphql::Result<Option<String>> {
        let loader = ctx.data::<DataLoader<UserLoader>>()?;
        loader.load_one(id).await.map_err(|e| async_graphql::Error::new(e.to_string()))
    }
}

pub struct UserLoader;

impl Loader<String> for UserLoader {
    type Value = String;
    type Error = Arc<String>;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Self::Value>, Self::Error> {
        let mut map = HashMap::new();
        for key in keys {
            map.insert(key.clone(), format!("User_Data_For_{}", key));
        }
        Ok(map)
    }
}

#[derive(Default)]
struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn trigger_workflow(&self, ctx: &Context<'_>) -> async_graphql::Result<String> {
        // Retrieve Axon from Context
        let axon = ctx.data::<Arc<WorkflowAxon>>().expect("Axon not found in context");
        
        let mut bus = Bus::new();
        let result = axon.execute((), &(), &mut bus).await;

        match result {
            Outcome::Next(state) => Ok(format!("Workflow completed with state: {}", state)),
            Outcome::Fault(err) => Err(async_graphql::Error::new(err.to_string())),
            _ => Err(async_graphql::Error::new("Unexpected Outcome")),
        }
    }
}

#[derive(Default)]
struct SubscriptionRoot;

#[async_graphql::Subscription]
impl SubscriptionRoot {
    async fn workflow_events(&self) -> impl futures_util::Stream<Item = String> {
        futures_util::stream::iter(vec![
            "Workflow Started".to_string(),
            "Processing...".to_string(),
            "Workflow Completed".to_string(),
        ])
    }
}

// Atomic Steps
#[derive(Clone)]
pub struct StartWorkflow;

#[async_trait]
impl Transition<(), String> for StartWorkflow {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        tracing::info!("Workflow triggered via GraphQL");
        Outcome::Next("completed".to_string())
    }
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // 1. Build the Execution Chain
    let axon = Axon::<(), (), String>::start("GraphQL Demo Workflow")
        .then(StartWorkflow);
        
    let axon = Arc::new(axon);

    // 2. Build the GraphQL schema, injecting Axon into the context data
    let schema = Schema::build(QueryRoot, MutationRoot, SubscriptionRoot)
        .data(axon.clone())
        .data(DataLoader::new(UserLoader, tokio::spawn))
        .limit_complexity(100) // RQ4: Complexity analysis constraint
        .finish();

    // 3. The ingress wrapper
    let graphql_ingress = GraphQLIngress::new(schema);

    tracing::info!("GraphQL schema generated and ready.");
    
    // 4. Test Demo internally
    tracing::info!("--- Testing Mutation ---");
    let req = async_graphql::Request::new(r#"
        mutation {
            triggerWorkflow
        }
    "#);
    
    let res = graphql_ingress.execute(req).await;
    tracing::info!("Mutation Response: {:?}", res);

    tracing::info!("--- Testing Query with DataLoader ---");
    let req_user = async_graphql::Request::new(r#"
        query {
            u1: user(id: "1")
            u2: user(id: "2")
        }
    "#);
    let res_user = graphql_ingress.execute(req_user).await;
    tracing::info!("DataLoader Query Response: {:?}", res_user);

    tracing::info!("--- Testing Subscription ---");
    let sub_req = async_graphql::Request::new(r#"
        subscription {
            workflowEvents
        }
    "#);
    
    use futures_util::StreamExt;
    let mut stream = graphql_ingress.execute_stream(sub_req);
    while let Some(res) = stream.next().await {
        tracing::info!("Subscription Event: {:?}", res);
    }

    Ok(())
}

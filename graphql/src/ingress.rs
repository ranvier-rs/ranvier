use async_graphql::{ObjectType, Request, Response, Schema, SubscriptionType};

/// An ingress adapter that bridges `async-graphql` execution to Ranvier Axon circuits.
pub struct GraphQLIngress<Query, Mutation, Subscription> {
    schema: Schema<Query, Mutation, Subscription>,
}

impl<Query, Mutation, Subscription> GraphQLIngress<Query, Mutation, Subscription>
where
    Query: ObjectType + 'static,
    Mutation: ObjectType + 'static,
    Subscription: SubscriptionType + 'static,
{
    /// Creates a new GraphQL Ingress with the given schema.
    pub fn new(schema: Schema<Query, Mutation, Subscription>) -> Self {
        Self { schema }
    }

    /// Executes a GraphQL request, potentially dispatching to the underlying Axon.
    pub async fn execute(&self, request: Request) -> Response {
        self.schema.execute(request).await
    }

    /// Executes a GraphQL subscription request, returning a stream of Responses.
    pub fn execute_stream(
        &self,
        request: Request,
    ) -> impl async_graphql::futures_util::Stream<Item = Response> {
        self.schema.execute_stream(request)
    }

    /// Generates the HTML for the GraphQL Playground pointing to the given endpoint.
    pub fn playground(endpoint: &str) -> String {
        async_graphql::http::playground_source(
            async_graphql::http::GraphQLPlaygroundConfig::new(endpoint)
                .subscription_endpoint(endpoint),
        )
    }
}

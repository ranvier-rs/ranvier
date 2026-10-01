# HTTP Service Call Demo

This example performs read-only HTTP GET user lookups through typed transitions:
ValidateRequest → FetchUserFromApi → EnrichProfile. Configure
SERVICE_CALL_BASE_URL or use the demonstration JSONPlaceholder endpoint, then
run `cargo run -p service-call-demo`. The public endpoint is not an acceptance
fixture; deterministic regression tests use disposable loopback TCP servers.

Endpoint configuration permits HTTP/HTTPS without embedded credentials, query
or fragment. The client rejects redirects and has a ten-second request timeout.
The guarded scenario adds a five-second local wait timeout. The runnable chain
does not retry; its prior retry label was incorrect. Dropping the local wait
does not prove server-side cancellation. A future write adapter needs effect
identity and reconciliation before any retry policy.

Successful responses require a matching user ID and nonempty name/email/company
fields. JSON bodies are limited to 64 KiB while reading; known oversized content
length also rejects early. Missing fields never become a successful "unknown"
profile. Network/parse failures return fixed errors rather than raw URL/body text.
4xx returns client_error Branch with status/lookup ID; other unhandled statuses
return Fault. Unexpected control outcomes fail the demo.

The local ExecutionReport policy accepts only Unpersisted with complete cleanup;
uncertain/persistent reports require reconciliation without automatic retry.
Each request uses a fresh Bus. TLS/mTLS, authentication, production endpoint
allowlists, data privacy, chunked/compressed boundary coverage, retry/backoff,
native/customer and real third-party availability acceptance remain OPEN.

Tests use real loopback HTTP requests for status/response mapping, input rejection
before connection, local timeout and cross-origin redirect refusal. They do not
establish Internet/TLS or production service correctness.

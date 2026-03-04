use criterion::{Criterion, criterion_group, criterion_main};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_db::pool::SqlitePool;
use ranvier_runtime::Axon;
use sqlx::Row;
use tokio::runtime::Runtime;

#[derive(Clone)]
struct QueryResult {
    id: i64,
}

#[derive(Debug)]
struct DbBenchError(String);

impl std::fmt::Display for DbBenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DbBenchError({})", self.0)
    }
}

impl std::error::Error for DbBenchError {}
impl From<anyhow::Error> for DbBenchError {
    fn from(e: anyhow::Error) -> Self {
        DbBenchError(e.to_string())
    }
}

#[derive(Clone)]
struct SelectTransition;

#[async_trait::async_trait]
impl Transition<(), QueryResult> for SelectTransition {
    type Error = DbBenchError;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<QueryResult, Self::Error> {
        let pool = bus.read::<SqlitePool>().unwrap();

        let result = sqlx::query("SELECT 1 as id").fetch_one(pool.inner()).await;

        match result {
            Ok(row) => {
                let id: i64 = row.get("id");
                Outcome::Next(QueryResult { id })
            }
            Err(e) => Outcome::Fault(DbBenchError(e.to_string())),
        }
    }
}

fn bench_db_query_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let axon = Axon::new("db_query_axon").then(SelectTransition).clone();

    // Setup in-memory SQLite pool
    let pool = rt.block_on(async { SqlitePool::new("sqlite::memory:").await.unwrap() });

    c.bench_function("db_query_latency_sqlite_mem", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            bus.insert(pool.clone());
            let _ = axon.execute((), &(), &mut bus).await;
        })
    });
}

criterion_group!(benches, bench_db_query_latency);
criterion_main!(benches);

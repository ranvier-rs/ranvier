// Generated from the actual SQLx catalog string; demo types are in the parent module.
#[rustfmt::skip]
#[tokio::test]
async fn public_sqlx_catalog_inserts_with_explicit_report_policy() -> anyhow::Result<()> {
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await?;
    sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, email TEXT NOT NULL)").execute(&pool).await?;
    let check_pool = pool.clone();
    let req = CreateUserReq { name: "Catalog".into(), email: "catalog@example.com".into() };
    let mut bus = Bus::new();
let db = DbPool(pool);
let insert_axon = Axon::<CreateUserReq, CreateUserReq, String, DbPool>::new("insert-user")
    .then(InsertUser);
let report = insert_axon.execute(req, &db, &mut bus).await;
// Local demo: workflow durability is intentionally not configured.
if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
    anyhow::bail!("Execution requires reconciliation before retry");
}
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(user), .. } => {
        println!("Inserted user id={}", user.id);
    }
    _ => anyhow::bail!("Insertion requires reconciliation before retry"),
}
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE name = ? AND email = ?").bind("Catalog").bind("catalog@example.com").fetch_one(&check_pool).await?;
    assert_eq!(count, 1);
    Ok(())
}

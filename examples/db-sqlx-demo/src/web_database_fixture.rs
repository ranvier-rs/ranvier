// Actual PostgreSQL export: declarations and composition separated for Rust item scope.
#[rustfmt::skip]
mod database_philosophy {
use super::*;
// PostgreSQL example: explicit input, Resources and checked update.
// This is not a payment ledger or an automatic-retry/idempotency contract.
#[derive(Clone)]
struct BalanceDb(sqlx::PgPool);
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BalanceUpdate { account_id: i64, amount: i64 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PendingBalance { account_id: i64, version: i64, next: i64 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AccountBalance { account_id: i64, balance: i64 }
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum DbError { Unavailable, NotFound, InvalidBalance, VersionExhausted, Conflict }

#[derive(Clone)]
struct FetchBalance;
#[async_trait::async_trait]
impl Transition<BalanceUpdate, PendingBalance> for FetchBalance {
    type Error = DbError;
    type Resources = BalanceDb;
    async fn run(&self, input: BalanceUpdate, db: &BalanceDb, _: &mut Bus) -> Outcome<PendingBalance, DbError> {
        let (previous, version) = match sqlx::query_as::<_, (i64, i64)>("SELECT balance, version FROM accounts WHERE id = $1")
            .bind(input.account_id).fetch_optional(&db.0).await {
            Ok(Some(row)) => row,
            Ok(None) => return Outcome::Fault(DbError::NotFound),
            Err(_) => return Outcome::Fault(DbError::Unavailable),
        };
        if version < 0 || version == i64::MAX { return Outcome::Fault(DbError::VersionExhausted); }
        let Some(next) = previous.checked_add(input.amount).filter(|value| *value >= 0) else {
            return Outcome::Fault(DbError::InvalidBalance);
        };
        Outcome::Next(PendingBalance { account_id: input.account_id, version, next })
    }
}

#[derive(Clone)]
struct UpdateBalance;
#[async_trait::async_trait]
impl Transition<PendingBalance, AccountBalance> for UpdateBalance {
    type Error = DbError;
    type Resources = BalanceDb;
    async fn run(&self, input: PendingBalance, db: &BalanceDb, _: &mut Bus) -> Outcome<AccountBalance, DbError> {
        if input.next < 0 { return Outcome::Fault(DbError::InvalidBalance); }
        if input.version < 0 || input.version == i64::MAX { return Outcome::Fault(DbError::VersionExhausted); }
        // Version comparison also rejects a balance that changed and changed back.
        // Do not automatically retry or reset/reuse account versions.
        match sqlx::query("UPDATE accounts SET balance = $1, version = version + 1 WHERE id = $2 AND version = $3")
            .bind(input.next).bind(input.account_id).bind(input.version).execute(&db.0).await {
            Ok(result) if result.rows_affected() == 1 => Outcome::Next(AccountBalance { account_id: input.account_id, balance: input.next }),
            Ok(_) => Outcome::Fault(DbError::Conflict),
            Err(_) => Outcome::Fault(DbError::Unavailable),
        }
    }
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL fixture"]
async fn real_postgres_versioned_balance_flow() -> anyhow::Result<()> {
    let url = std::env::var("RANVIER_BALANCE_TEST_POSTGRES")?;
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
    sqlx::query("CREATE TABLE accounts (id BIGINT PRIMARY KEY, balance BIGINT NOT NULL CHECK(balance >= 0), version BIGINT NOT NULL CHECK(version >= 0))").execute(&pool).await?;
    sqlx::query("INSERT INTO accounts VALUES (1, 100, 0), (2, 9223372036854775807, 0), (3, 1, 9223372036854775807)").execute(&pool).await?;
    let db = BalanceDb(pool.clone());

let axon = Axon::<BalanceUpdate, BalanceUpdate, DbError, BalanceDb>::new("update-balance")
    .then(FetchBalance)
    .then(UpdateBalance);
    let mut bus = Bus::new();
    let report = axon.execute(BalanceUpdate { account_id: 1, amount: 20 }, &db, &mut bus).await;
    assert!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Next(AccountBalance { balance: 120, .. }), .. }));
    for (account_id, amount, expected) in [(99, 1, DbError::NotFound), (1, -121, DbError::InvalidBalance), (2, 1, DbError::InvalidBalance), (3, 0, DbError::VersionExhausted)] {
        let report = axon.execute(BalanceUpdate { account_id, amount }, &db, &mut Bus::new()).await;
        assert!(matches!(report, ExecutionReport::Unpersisted { result: Outcome::Fault(ref error), .. } if *error == expected));
    }
    // Two readers of one version: exactly one succeeds; the other is a conflict.
    let Outcome::Next(first) = FetchBalance.run(BalanceUpdate { account_id: 1, amount: 10 }, &db, &mut bus).await else { panic!("load") };
    let second = first.clone();
    let mut first_bus = Bus::new();
    let mut second_bus = Bus::new();
    let (a, b) = tokio::join!(UpdateBalance.run(first, &db, &mut first_bus), UpdateBalance.run(second, &db, &mut second_bus));
    assert_eq!([&a, &b].iter().filter(|value| matches!(value, Outcome::Next(_))).count(), 1);
    assert_eq!([&a, &b].iter().filter(|value| matches!(value, Outcome::Fault(DbError::Conflict))).count(), 1);
    // Balance changes and changes back, but the original version stays stale.
    let Outcome::Next(stale) = FetchBalance.run(BalanceUpdate { account_id: 1, amount: 5 }, &db, &mut bus).await else { panic!("load") };
    for amount in [1, -1] {
        assert!(matches!(axon.execute(BalanceUpdate { account_id: 1, amount }, &db, &mut bus).await, ExecutionReport::Unpersisted { result: Outcome::Next(_), .. }));
    }
    assert!(matches!(UpdateBalance.run(stale, &db, &mut bus).await, Outcome::Fault(DbError::Conflict)));
    let (balance, version): (i64, i64) = sqlx::query_as("SELECT balance, version FROM accounts WHERE id = 1").fetch_one(&pool).await?;
    assert_eq!((balance, version), (130, 4));
    pool.close().await;
    assert!(matches!(FetchBalance.run(BalanceUpdate { account_id: 1, amount: 1 }, &db, &mut bus).await, Outcome::Fault(DbError::Unavailable)));
    Ok(())
}
}

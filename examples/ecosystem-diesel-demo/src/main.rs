//! # Diesel ORM Integration
//!
//! Demonstrates Diesel for SQLite database operations with migrations and CRUD workflows.
//!
//! ## Run
//! ```bash
//! cargo run -p ecosystem-diesel-demo
//! ```
//!
//! ## Key Concepts
//! - r2d2 connection pooling in Resources
//! - Embedded migrations with diesel_migrations
//! - Blocking database calls via tokio::task::spawn_blocking

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
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

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

diesel::table! {
    users (id) {
        id -> Integer,
        username -> Text,
        email -> Text,
    }
}

#[derive(Clone)]
struct AppResources {
    pool: Pool<ConnectionManager<SqliteConnection>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateUserInput {
    username: String,
    email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UpdateEmailInput {
    id: i32,
    email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DeleteUserInput {
    id: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FetchAllInput;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserSummary {
    id: i32,
    username: String,
    email: String,
}

#[derive(Debug, Queryable, Identifiable)]
#[diesel(table_name = users)]
struct UserRow {
    id: i32,
    username: String,
    email: String,
}

#[derive(Insertable)]
#[diesel(table_name = users)]
struct NewUser<'a> {
    username: &'a str,
    email: &'a str,
}

#[derive(AsChangeset)]
#[diesel(table_name = users)]
struct UpdateUserEmail<'a> {
    email: &'a str,
}

impl From<UserRow> for UserSummary {
    fn from(row: UserRow) -> Self {
        Self {
            id: row.id,
            username: row.username,
            email: row.email,
        }
    }
}

async fn run_blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| anyhow!("blocking task join error: {err}"))?
}

#[derive(Clone, Copy)]
struct CreateUserTransition;

#[async_trait]
impl Transition<CreateUserInput, UserSummary> for CreateUserTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: CreateUserInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserSummary, Self::Error> {
        let pool = resources.pool.clone();
        let username = input.username;
        let email = input.email;
        let created = run_blocking(move || {
            let mut conn = pool.get().map_err(|e| anyhow!("{}", e))?;
            let insert = NewUser {
                username: &username,
                email: &email,
            };
            conn.transaction::<_, anyhow::Error, _>(|conn| {
                diesel::insert_into(users::table)
                    .values(&insert)
                    .execute(conn)
                    .map_err(|e| anyhow!("{}", e))?;

                use self::users::dsl;
                #[derive(QueryableByName)]
                struct InsertedId {
                    #[diesel(sql_type = diesel::sql_types::BigInt)]
                    id: i64,
                }
                let inserted = diesel::sql_query("SELECT last_insert_rowid() AS id")
                    .get_result::<InsertedId>(conn)?;
                let id = i32::try_from(inserted.id)?;
                let row = dsl::users
                    .find(id)
                    .first::<UserRow>(conn)
                    .map_err(|e| anyhow!("{}", e))?;
                Ok(UserSummary::from(row))
            })
        })
        .await;

        match created {
            Ok(user) => Outcome::Next(user),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
struct UpdateUserEmailTransition;

#[async_trait]
impl Transition<UpdateEmailInput, UserSummary> for UpdateUserEmailTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: UpdateEmailInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserSummary, Self::Error> {
        let pool = resources.pool.clone();
        let user_id = input.id;
        let next_email = input.email;
        let updated = run_blocking(move || {
            use self::users::dsl;

            let mut conn = pool.get().map_err(|e| anyhow!("{}", e))?;
            conn.transaction::<_, anyhow::Error, _>(|conn| {
                let affected = diesel::update(dsl::users.filter(dsl::id.eq(user_id)))
                    .set(UpdateUserEmail { email: &next_email })
                    .execute(conn)
                    .map_err(|e| anyhow!("{}", e))?;
                if affected == 0 {
                    return Err(anyhow!("user id={} not found", user_id));
                }

                let row = dsl::users
                    .find(user_id)
                    .first::<UserRow>(conn)
                    .map_err(|e| anyhow!("{}", e))?;
                Ok(UserSummary::from(row))
            })
        })
        .await;

        match updated {
            Ok(user) => Outcome::Next(user),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
struct DeleteUserTransition;

#[async_trait]
impl Transition<DeleteUserInput, i32> for DeleteUserTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: DeleteUserInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<i32, Self::Error> {
        let pool = resources.pool.clone();
        let user_id = input.id;
        let deleted = run_blocking(move || {
            use self::users::dsl;

            let mut conn = pool.get().map_err(|e| anyhow!("{}", e))?;
            let affected = diesel::delete(dsl::users.filter(dsl::id.eq(user_id)))
                .execute(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;
            if affected == 0 {
                return Err(anyhow!("user id={} not found", user_id));
            }
            Ok(user_id)
        })
        .await;

        match deleted {
            Ok(id) => Outcome::Next(id),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
struct ListUsersTransition;

#[async_trait]
impl Transition<FetchAllInput, Vec<UserSummary>> for ListUsersTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        _input: FetchAllInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Vec<UserSummary>, Self::Error> {
        let pool = resources.pool.clone();
        let listed = run_blocking(move || {
            use self::users::dsl;

            let mut conn = pool.get().map_err(|e| anyhow!("{}", e))?;
            let rows = dsl::users
                .order(dsl::id.asc())
                .load::<UserRow>(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;
            Ok(rows.into_iter().map(UserSummary::from).collect())
        })
        .await;

        match listed {
            Ok(users) => Outcome::Next(users),
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

// Each run exclusively creates its own file; existing databases are never removed.
struct OwnedDemoDb(std::path::PathBuf);
impl OwnedDemoDb {
    fn create() -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ranvier-diesel-{}-{nonce}.sqlite",
            std::process::id()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }
}
impl Drop for OwnedDemoDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn require_next<T>(outcome: Outcome<T, String>) -> Result<T> {
    match outcome {
        Outcome::Next(value) => Ok(value),
        _ => Err(anyhow!("Database operation failed; reconcile before retry")),
    }
}
#[derive(Debug)]
struct SqliteConnectionPolicy;
impl diesel::r2d2::CustomizeConnection<SqliteConnection, diesel::r2d2::Error>
    for SqliteConnectionPolicy
{
    fn on_acquire(
        &self,
        conn: &mut SqliteConnection,
    ) -> std::result::Result<(), diesel::r2d2::Error> {
        use diesel::connection::SimpleConnection;
        conn.batch_execute("PRAGMA busy_timeout = 5000;")
            .map_err(diesel::r2d2::Error::QueryError)
    }
}
fn resources(path: &std::path::Path) -> Result<AppResources> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Invalid database path"))?;
    let manager = ConnectionManager::<SqliteConnection>::new(path);
    let pool = Pool::builder()
        .max_size(4)
        .connection_customizer(Box::new(SqliteConnectionPolicy))
        .build(manager)?;
    {
        let mut conn = pool.get()?;
        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|_| anyhow!("Database migration failed"))?;
    }
    Ok(AppResources { pool })
}
async fn run_demo(path: &std::path::Path) -> Result<()> {
    let resources = resources(path)?;
    let create =
        Axon::<CreateUserInput, CreateUserInput, String, AppResources>::new("diesel.create_user")
            .then(CreateUserTransition);
    let list = Axon::<FetchAllInput, FetchAllInput, String, AppResources>::new("diesel.list_users")
        .then(ListUsersTransition);
    let update = Axon::<UpdateEmailInput, UpdateEmailInput, String, AppResources>::new(
        "diesel.update_user_email",
    )
    .then(UpdateUserEmailTransition);
    let delete =
        Axon::<DeleteUserInput, DeleteUserInput, String, AppResources>::new("diesel.delete_user")
            .then(DeleteUserTransition);
    let alice = require_next(local_demo_outcome(
        create
            .execute(
                CreateUserInput {
                    username: "alice".into(),
                    email: "alice@demo.local".into(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?)?;
    let bob = require_next(local_demo_outcome(
        create
            .execute(
                CreateUserInput {
                    username: "bob".into(),
                    email: "bob@demo.local".into(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?)?;
    if alice.id == bob.id {
        return Err(anyhow!("Created identities are not distinct"));
    }
    let before = require_next(local_demo_outcome(
        list.execute(FetchAllInput, &resources, &mut Bus::new())
            .await,
    )?)?;
    if before.len() != 2 {
        return Err(anyhow!("Unexpected initial row count"));
    }
    let updated = require_next(local_demo_outcome(
        update
            .execute(
                UpdateEmailInput {
                    id: alice.id,
                    email: "alice+updated@demo.local".into(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?)?;
    if updated.id != alice.id || updated.email != "alice+updated@demo.local" {
        return Err(anyhow!("Updated row does not match request"));
    }
    let deleted = require_next(local_demo_outcome(
        delete
            .execute(DeleteUserInput { id: bob.id }, &resources, &mut Bus::new())
            .await,
    )?)?;
    if deleted != bob.id {
        return Err(anyhow!("Deleted identity does not match request"));
    }
    let after = require_next(local_demo_outcome(
        list.execute(FetchAllInput, &resources, &mut Bus::new())
            .await,
    )?)?;
    if after.len() != 1 || after[0].id != alice.id || after[0].email != updated.email {
        return Err(anyhow!("Final database state does not match workflow"));
    }
    println!("Diesel file-backed CRUD verified");
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let db = OwnedDemoDb::create()?;
    run_demo(&db.0).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_file_crud_and_owned_cleanup() -> Result<()> {
        let db = OwnedDemoDb::create()?;
        let path = db.0.clone();
        run_demo(&path).await?;
        drop(db);
        assert!(!path.exists());
        Ok(())
    }
    #[tokio::test]
    async fn concurrent_creates_return_their_own_rows() -> Result<()> {
        let db = OwnedDemoDb::create()?;
        let resources = resources(&db.0)?;
        let mut bus_a = Bus::new();
        let mut bus_b = Bus::new();
        let (a, b) = tokio::join!(
            CreateUserTransition.run(
                CreateUserInput {
                    username: "a".into(),
                    email: "a@local".into()
                },
                &resources,
                &mut bus_a
            ),
            CreateUserTransition.run(
                CreateUserInput {
                    username: "b".into(),
                    email: "b@local".into()
                },
                &resources,
                &mut bus_b
            )
        );
        let a = require_next(a)?;
        let b = require_next(b)?;
        assert_ne!(a.id, b.id);
        assert_eq!(a.username, "a");
        assert_eq!(b.username, "b");
        Ok(())
    }
}

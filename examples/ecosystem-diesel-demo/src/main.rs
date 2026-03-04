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
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

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

impl ResourceRequirement for AppResources {}

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
            diesel::insert_into(users::table)
                .values(&insert)
                .execute(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;

            use self::users::dsl;
            let row = dsl::users
                .order(dsl::id.desc())
                .first::<UserRow>(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;
            Ok(UserSummary::from(row))
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
            let affected = diesel::update(dsl::users.filter(dsl::id.eq(user_id)))
                .set(UpdateUserEmail { email: &next_email })
                .execute(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;
            if affected == 0 {
                return Err(anyhow!("user id={} not found", user_id));
            }

            let row = dsl::users
                .find(user_id)
                .first::<UserRow>(&mut conn)
                .map_err(|e| anyhow!("{}", e))?;
            Ok(UserSummary::from(row))
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

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 Diesel Reference Demo ===");

    let db_path = "target/m132_diesel_demo.sqlite";
    if std::path::Path::new(db_path).exists() {
        let _ = std::fs::remove_file(db_path);
    }

    let manager = ConnectionManager::<SqliteConnection>::new(db_path);
    let pool = Pool::builder().max_size(4).build(manager)?;

    {
        let mut conn = pool.get()?;
        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|err| anyhow!("failed to run migrations: {err}"))?;
    }

    let resources = AppResources { pool };
    let mut bus = Bus::new();

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

    let alice = create
        .execute(
            CreateUserInput {
                username: "alice".to_string(),
                email: "alice@demo.local".to_string(),
            },
            &resources,
            &mut bus,
        )
        .await;
    let bob = create
        .execute(
            CreateUserInput {
                username: "bob".to_string(),
                email: "bob@demo.local".to_string(),
            },
            &resources,
            &mut bus,
        )
        .await;

    match (&alice, &bob) {
        (Outcome::Next(a), Outcome::Next(b)) => {
            println!("created: {}#{}, {}#{}", a.username, a.id, b.username, b.id);
        }
        _ => {
            return Err(anyhow!("create failed: alice={:?}, bob={:?}", alice, bob));
        }
    }

    let before: Outcome<Vec<UserSummary>, String> =
        list.execute(FetchAllInput, &resources, &mut bus).await;
    if let Outcome::Next(users) = &before {
        println!("list(before): {} users", users.len());
    }

    let updated = update
        .execute(
            UpdateEmailInput {
                id: 1,
                email: "alice+updated@demo.local".to_string(),
            },
            &resources,
            &mut bus,
        )
        .await;
    if let Outcome::Next(user) = &updated {
        println!("updated: {} -> {}", user.username, user.email);
    }

    let deleted = delete
        .execute(DeleteUserInput { id: 2 }, &resources, &mut bus)
        .await;
    if let Outcome::Next(id) = &deleted {
        println!("deleted id={}", id);
    }

    let after: Outcome<Vec<UserSummary>, String> =
        list.execute(FetchAllInput, &resources, &mut bus).await;
    match after {
        Outcome::Next(users) => {
            println!("list(after): {} users", users.len());
            for user in users {
                println!(
                    "- id={} username={} email={}",
                    user.id, user.username, user.email
                );
            }
        }
        other => {
            return Err(anyhow!("final list failed: {:?}", other));
        }
    }

    println!("done");
    Ok(())
}

//! # SeaORM Integration
//!
//! Full CRUD demo with SeaORM showing migrations, entity models, and Axon-based database operations.
//!
//! ## Run
//! ```bash
//! cargo run -p ecosystem-seaorm-demo
//! ```
//!
//! ## Key Concepts
//! - SeaORM entities and migrations
//! - DatabaseConnection as shared resource
//! - Create/Read/Update/Delete flows via Axon transitions

use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, Database, DatabaseConnection, EntityTrait, ModelTrait,
};
use sea_orm_migration::prelude::*;
use serde::{Deserialize, Serialize};

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

#[derive(Clone)]
struct AppResources {
    db: DatabaseConnection,
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

mod user {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub username: String,
        pub email: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(CreateUsersTable)]
    }
}

#[derive(DeriveMigrationName)]
struct CreateUsersTable;

#[async_trait::async_trait]
impl MigrationTrait for CreateUsersTable {
    async fn up(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("users"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("username")).string().not_null())
                    .col(ColumnDef::new(Alias::new("email")).string().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("users")).to_owned())
            .await
    }
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
        let model = user::ActiveModel {
            username: Set(input.username),
            email: Set(input.email),
            ..Default::default()
        }
        .insert(&resources.db)
        .await
        .map_err(|e| e.to_string());

        match model {
            Ok(model) => Outcome::Next(UserSummary {
                id: model.id,
                username: model.username,
                email: model.email,
            }),
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
        let found = user::Entity::find_by_id(input.id)
            .one(&resources.db)
            .await
            .map_err(|e| e.to_string());

        let model = match found {
            Ok(Some(model)) => model,
            Ok(None) => return Outcome::Fault(format!("user id={} not found", input.id)),
            Err(err) => return Outcome::Fault(err),
        };

        let mut active: user::ActiveModel = model.into();
        active.email = Set(input.email);

        let updated = active
            .update(&resources.db)
            .await
            .map_err(|e| e.to_string());

        match updated {
            Ok(updated) => Outcome::Next(UserSummary {
                id: updated.id,
                username: updated.username,
                email: updated.email,
            }),
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
        let found = user::Entity::find_by_id(input.id)
            .one(&resources.db)
            .await
            .map_err(|e| e.to_string());

        let model = match found {
            Ok(Some(model)) => model,
            Ok(None) => return Outcome::Fault(format!("user id={} not found", input.id)),
            Err(err) => return Outcome::Fault(err),
        };

        let deleted_id = model.id;
        let deleted = model.delete(&resources.db).await.map_err(|e| e.to_string());

        match deleted {
            Ok(_) => Outcome::Next(deleted_id),
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
        let rows = user::Entity::find()
            .all(&resources.db)
            .await
            .map_err(|e| e.to_string());

        match rows {
            Ok(rows) => {
                let users = rows
                    .into_iter()
                    .map(|row| UserSummary {
                        id: row.id,
                        username: row.username,
                        email: row.email,
                    })
                    .collect();
                Outcome::Next(users)
            }
            Err(err) => Outcome::Fault(err.to_string()),
        }
    }
}

async fn run_demo() -> Result<()> {
    println!("=== M132 SeaORM Reference Demo ===");

    // This finite CRUD fixture owns no persistent file and is safe to repeat.
    // Durable database migration/recovery acceptance is a separate gate.
    let db = Database::connect("sqlite::memory:").await?;
    Migrator::up(&db, None).await?;
    let resources = AppResources { db };

    let create =
        Axon::<CreateUserInput, CreateUserInput, String, AppResources>::new("seaorm.create_user")
            .then(CreateUserTransition);

    let list = Axon::<FetchAllInput, FetchAllInput, String, AppResources>::new("seaorm.list_users")
        .then(ListUsersTransition);

    let update = Axon::<UpdateEmailInput, UpdateEmailInput, String, AppResources>::new(
        "seaorm.update_user_email",
    )
    .then(UpdateUserEmailTransition);

    let delete =
        Axon::<DeleteUserInput, DeleteUserInput, String, AppResources>::new("seaorm.delete_user")
            .then(DeleteUserTransition);

    let alice = local_demo_outcome(
        create
            .execute(
                CreateUserInput {
                    username: "alice".to_string(),
                    email: "alice@demo.local".to_string(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?;
    let bob = local_demo_outcome(
        create
            .execute(
                CreateUserInput {
                    username: "bob".to_string(),
                    email: "bob@demo.local".to_string(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?;

    let (alice_id, bob_id) = match (&alice, &bob) {
        (Outcome::Next(a), Outcome::Next(b)) => {
            println!("created: {}#{}, {}#{}", a.username, a.id, b.username, b.id);
            (a.id, b.id)
        }
        _ => {
            return Err(anyhow::anyhow!(
                "create failed: alice={:?}, bob={:?}",
                alice,
                bob
            ));
        }
    };

    let before: Outcome<Vec<UserSummary>, String> = local_demo_outcome(
        list.execute(FetchAllInput, &resources, &mut Bus::new())
            .await,
    )?;
    match &before {
        Outcome::Next(users) if users.len() == 2 => println!("list(before): {} users", users.len()),
        _ => return Err(anyhow::anyhow!("initial list outcome mismatch")),
    }

    let updated = local_demo_outcome(
        update
            .execute(
                UpdateEmailInput {
                    id: alice_id,
                    email: "alice+updated@demo.local".to_string(),
                },
                &resources,
                &mut Bus::new(),
            )
            .await,
    )?;
    match &updated {
        Outcome::Next(user) if user.id == alice_id && user.email == "alice+updated@demo.local" => {
            println!("updated: {} -> {}", user.username, user.email)
        }
        _ => return Err(anyhow::anyhow!("update outcome mismatch")),
    }

    let deleted = local_demo_outcome(
        delete
            .execute(DeleteUserInput { id: bob_id }, &resources, &mut Bus::new())
            .await,
    )?;
    match &deleted {
        Outcome::Next(id) if *id == bob_id => println!("deleted id={}", id),
        _ => return Err(anyhow::anyhow!("delete outcome mismatch")),
    }

    let after: Outcome<Vec<UserSummary>, String> = local_demo_outcome(
        list.execute(FetchAllInput, &resources, &mut Bus::new())
            .await,
    )?;
    match after {
        Outcome::Next(users)
            if users.len() == 1
                && users[0].id == alice_id
                && users[0].email == "alice+updated@demo.local" =>
        {
            println!("list(after): {} users", users.len());
            for user in users {
                println!(
                    "- id={} username={} email={}",
                    user.id, user.username, user.email
                );
            }
        }
        other => {
            return Err(anyhow::anyhow!("final list failed: {:?}", other));
        }
    }

    println!("done");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run_demo().await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_sqlite_crud() {
        run_demo().await.unwrap();
    }
}

use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, Database, DatabaseConnection, EntityTrait, ModelTrait,
};
use sea_orm_migration::prelude::*;

#[derive(Clone)]
struct AppResources {
    db: DatabaseConnection,
}

impl ResourceRequirement for AppResources {}

#[derive(Clone, Debug)]
struct CreateUserInput {
    username: String,
    email: String,
}

#[derive(Clone, Debug)]
struct UpdateEmailInput {
    id: i32,
    email: String,
}

#[derive(Clone, Debug)]
struct DeleteUserInput {
    id: i32,
}

#[derive(Clone, Debug)]
struct FetchAllInput;

#[derive(Clone, Debug)]
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
    type Error = anyhow::Error;
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
        .map_err(anyhow::Error::from);

        match model {
            Ok(model) => Outcome::Next(UserSummary {
                id: model.id,
                username: model.username,
                email: model.email,
            }),
            Err(err) => Outcome::Fault(err),
        }
    }
}

#[derive(Clone, Copy)]
struct UpdateUserEmailTransition;

#[async_trait]
impl Transition<UpdateEmailInput, UserSummary> for UpdateUserEmailTransition {
    type Error = anyhow::Error;
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
            .map_err(anyhow::Error::from);

        let model = match found {
            Ok(Some(model)) => model,
            Ok(None) => return Outcome::Fault(anyhow::anyhow!("user id={} not found", input.id)),
            Err(err) => return Outcome::Fault(err),
        };

        let mut active: user::ActiveModel = model.into();
        active.email = Set(input.email);

        let updated = active
            .update(&resources.db)
            .await
            .map_err(anyhow::Error::from);

        match updated {
            Ok(updated) => Outcome::Next(UserSummary {
                id: updated.id,
                username: updated.username,
                email: updated.email,
            }),
            Err(err) => Outcome::Fault(err),
        }
    }
}

#[derive(Clone, Copy)]
struct DeleteUserTransition;

#[async_trait]
impl Transition<DeleteUserInput, i32> for DeleteUserTransition {
    type Error = anyhow::Error;
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
            .map_err(anyhow::Error::from);

        let model = match found {
            Ok(Some(model)) => model,
            Ok(None) => return Outcome::Fault(anyhow::anyhow!("user id={} not found", input.id)),
            Err(err) => return Outcome::Fault(err),
        };

        let deleted_id = model.id;
        let deleted = model.delete(&resources.db).await.map_err(anyhow::Error::from);

        match deleted {
            Ok(_) => Outcome::Next(deleted_id),
            Err(err) => Outcome::Fault(err),
        }
    }
}

#[derive(Clone, Copy)]
struct ListUsersTransition;

#[async_trait]
impl Transition<FetchAllInput, Vec<UserSummary>> for ListUsersTransition {
    type Error = anyhow::Error;
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
            .map_err(anyhow::Error::from);

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
            Err(err) => Outcome::Fault(err),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 SeaORM Reference Demo ===");

    let db_path = "target/m132_seaorm_demo.sqlite";
    if std::path::Path::new(db_path).exists() {
        let _ = std::fs::remove_file(db_path);
    }
    let db = Database::connect(format!("sqlite://{}?mode=rwc", db_path)).await?;
    Migrator::up(&db, None).await?;
    let resources = AppResources { db };
    let mut bus = Bus::new();

    let create = Axon::<CreateUserInput, CreateUserInput, anyhow::Error, AppResources>::start(
        "seaorm.create_user",
    )
    .then(CreateUserTransition);

    let list = Axon::<FetchAllInput, FetchAllInput, anyhow::Error, AppResources>::start(
        "seaorm.list_users",
    )
    .then(ListUsersTransition);

    let update = Axon::<UpdateEmailInput, UpdateEmailInput, anyhow::Error, AppResources>::start(
        "seaorm.update_user_email",
    )
    .then(UpdateUserEmailTransition);

    let delete = Axon::<DeleteUserInput, DeleteUserInput, anyhow::Error, AppResources>::start(
        "seaorm.delete_user",
    )
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
            return Err(anyhow::anyhow!(
                "create failed: alice={:?}, bob={:?}",
                alice,
                bob
            ));
        }
    }

    let before: Outcome<Vec<UserSummary>, anyhow::Error> =
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

    let after: Outcome<Vec<UserSummary>, anyhow::Error> =
        list.execute(FetchAllInput, &resources, &mut bus).await;
    match after {
        Outcome::Next(users) => {
            println!("list(after): {} users", users.len());
            for user in users {
                println!("- id={} username={} email={}", user.id, user.username, user.email);
            }
        }
        other => {
            return Err(anyhow::anyhow!("final list failed: {:?}", other));
        }
    }

    println!("done");
    Ok(())
}

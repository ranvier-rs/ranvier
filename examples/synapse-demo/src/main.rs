use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::synapse::Synapse;
use std::time::Duration;
use tokio::time::sleep;

// --- Mock Domain Data ---
#[derive(Debug)]
struct User {
    id: u32,
    username: String,
}

// --- Define Synapse (Integration) ---
struct PostgresSynapse {
    connection_string: String,
}

#[async_trait]
impl Synapse for PostgresSynapse {
    type Input = u32; // User ID
    type Output = Option<User>;
    type Error = String;

    async fn call(&self, user_id: Self::Input) -> Result<Self::Output, Self::Error> {
        println!(
            "[Synapse] Connecting to Postgres at {}...",
            self.connection_string
        );

        // Simulate network latency
        sleep(Duration::from_millis(500)).await;

        if user_id == 0 {
            return Err("Invalid User ID".to_string());
        }

        if user_id == 404 {
            println!("[Synapse] User not found.");
            return Ok(None);
        }

        println!("[Synapse] User {} found.", user_id);
        Ok(Some(User {
            id: user_id,
            username: format!("user_{}", user_id),
        }))
    }
}

// --- Axon Node using Synapse ---
struct GetUserNode {
    db: PostgresSynapse,
}

impl GetUserNode {
    async fn execute(&self, user_id: u32) -> Result<()> {
        println!("\n[Node] Executing GetUserNode for ID: {}", user_id);

        match self.db.call(user_id).await {
            Ok(Some(user)) => {
                println!("[Node] Success: Found {:?}", user);
                // In real Axon, we would return Outcome::Next(user)
            }
            Ok(None) => {
                println!("[Node] Branch: User Not Found");
                // Outcome::Branch("not_found")
            }
            Err(e) => {
                println!("[Node] Fault: {}", e);
                // Outcome::Fault(e)
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let node = GetUserNode {
        db: PostgresSynapse {
            connection_string: "postgres://localhost/mydb".to_string(),
        },
    };

    // Scenario 1: Success
    node.execute(42).await?;

    // Scenario 2: Not Found
    node.execute(404).await?;

    // Scenario 3: Error
    node.execute(0).await?;

    Ok(())
}

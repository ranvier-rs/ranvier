//! # TypeScript Codegen Demo
//!
//! Demonstrates generating TypeScript type definitions from Rust structs using `ts-rs`.
//! Replaces the removed `ranvier-synapse` crate for TypeScript interop.
//!
//! ## Run
//! ```bash
//! cargo run -p typescript-codegen-demo
//! ```
//!
//! ## Key Concepts
//! - `#[derive(TS)]` on domain structs for automatic TypeScript type generation
//! - Export `.ts` files for frontend consumption
//! - No wrapper crate needed — `ts-rs` derives handle everything

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ============================================================================
// Domain Types with TypeScript Export
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
struct User {
    id: u32,
    name: String,
    email: String,
    role: UserRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
enum UserRole {
    Admin,
    Editor,
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
struct CreateUserRequest {
    name: String,
    email: String,
    role: UserRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
struct ApiResponse<T: TS> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
struct PaginatedList<T: TS> {
    items: Vec<T>,
    total: u64,
    page: u32,
    per_page: u32,
}

// ============================================================================
// Main
// ============================================================================

fn main() -> anyhow::Result<()> {
    println!("=== TypeScript Codegen Demo ===\n");

    // Print generated TypeScript definitions
    println!("--- Generated TypeScript Types ---\n");

    println!("// User.ts");
    println!("{}\n", User::decl());

    println!("// UserRole.ts");
    println!("{}\n", UserRole::decl());

    println!("// CreateUserRequest.ts");
    println!("{}\n", CreateUserRequest::decl());

    println!("// ApiResponse.ts");
    println!("{}\n", <ApiResponse<User>>::decl());

    println!("// PaginatedList.ts");
    println!("{}\n", <PaginatedList<User>>::decl());

    // Optionally export to files
    let output_dir = std::env::var("TS_OUTPUT_DIR").unwrap_or_else(|_| "./dist/ts".into());

    if std::env::args().any(|a| a == "--export") {
        std::fs::create_dir_all(&output_dir)?;

        User::export_all_to(&output_dir)?;
        println!("Exported TypeScript definitions to {}/", output_dir);
    } else {
        println!("Tip: Run with --export to write .ts files to {}/", output_dir);
    }

    println!("\ndone");
    Ok(())
}

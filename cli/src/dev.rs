//! Development server command

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;

/// Run development server with hot reload
pub fn run_dev_command(project_path: Option<&str>, port: Option<u16>) -> Result<()> {
    let project_dir: PathBuf = if let Some(path) = project_path {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?
    };

    let port = port.unwrap_or(3000);

    println!("🚀 Starting Ranvier development server...");
    println!("   Project: {}", project_dir.display());
    println!("   Port: {}", port);

    // Check if Cargo.toml exists
    let cargo_toml = project_dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        anyhow::bail!("Cargo.toml not found. Is this a Rust project?");
    }

    // Run cargo watch with the binary
    let package_name = extract_package_name(&cargo_toml)?;

    println!("   Package: {}", package_name);
    println!();
    println!("   Server starting at http://0.0.0.0:{}", port);
    println!("   Press Ctrl+C to stop");
    println!();

    // Run cargo run with environment variables for dev mode
    let status = Command::new("cargo")
        .args([
            "run",
            &format!("--bin {}", package_name),
            // Could add RANVIER_DEV=1 env var here
        ])
        .current_dir(&project_dir)
        .spawn()?;

    // In a full implementation, we would:
    // - Start a file watcher for hot reload
    // - Start an inspector UI server
    // - Stream logs from the child process

    println!("   Development server started (PID: {:?})", status.id());

    // Wait for user to stop
    println!();
    println!("Note: This is a basic dev server. Full implementation will include:");
    println!("  - File watcher and hot reload");
    println!("  - Inspector UI at /_ranvier/inspect");
    println!("  - Schematic auto-regeneration");

    Ok(())
}

/// Extract package name from Cargo.toml
fn extract_package_name(cargo_toml: &PathBuf) -> Result<String> {
    let content = std::fs::read_to_string(cargo_toml)?;

    for line in content.lines() {
        if let Some(name) = line.strip_prefix("name = \"") {
            if let Some(end) = name.strip_suffix("\"") {
                return Ok(end.to_string());
            }
        }
    }

    anyhow::bail!("Could not find package name in Cargo.toml")
}

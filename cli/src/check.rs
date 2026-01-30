//! Project validation and schematic checking

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;

/// Run project validation checks
pub fn run_check_command(project_path: Option<&str>, _fix: bool) -> Result<()> {
    let project_dir: PathBuf = if let Some(path) = project_path {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?
    };

    println!("Checking Ranvier project at: {}", project_dir.display());

    // 1. Check if Cargo.toml exists
    let cargo_toml = project_dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        anyhow::bail!("Cargo.toml not found. Is this a Rust project?");
    }

    // 2. Run cargo check
    println!("  Running cargo check...");
    let output = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(&project_dir)
        .output();

    match output {
        Ok(result) if result.status.success() => {
            println!("  ✅ cargo check passed");
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            println!("  ❌ cargo check failed:\n{}", stderr);
            anyhow::bail!("Compilation errors detected");
        }
        Err(e) => {
            println!("  ⚠️  Could not run cargo check: {}", e);
        }
    }

    // 3. Check for ranvier-core dependency
    println!("  Checking ranvier-core dependency...");
    let cargo_content = std::fs::read_to_string(&cargo_toml)?;
    if cargo_content.contains("ranvier-core") {
        println!("  ✅ ranvier-core dependency found");
    } else {
        println!("  ⚠️  ranvier-core dependency not found");
    }

    // 4. Analyze project structure
    analyze_project_structure(&project_dir)?;

    println!();
    println!("✅ Check complete!");

    Ok(())
}

/// Analyze project structure for common issues
fn analyze_project_structure(project_dir: &PathBuf) -> Result<()> {
    println!("  Analyzing project structure...");

    let src_dir = project_dir.join("src");
    if !src_dir.exists() {
        println!("  ⚠️  No src/ directory found");
        return Ok(());
    }

    // Check for main.rs
    let main_rs = src_dir.join("main.rs");
    if main_rs.exists() {
        let content = std::fs::read_to_string(&main_rs)?;

        // Check for Axon usage
        if content.contains("Axon") {
            println!("  ✅ Axon usage detected");
        } else {
            println!("  ℹ️  No Axon usage detected (new project?)");
        }

        // Check for Transition implementations
        let transition_count = content.matches("impl Transition").count();
        if transition_count > 0 {
            println!("  ✅ Found {} Transition implementation(s)", transition_count);
        }
    }

    // Check for examples folder
    let examples_dir = project_dir.join("examples");
    if examples_dir.exists() {
        let count = std::fs::read_dir(examples_dir)?.count();
        if count > 0 {
            println!("  ℹ️  Found {} example(s)", count);
        }
    }

    Ok(())
}

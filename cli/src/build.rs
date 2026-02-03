//! Static build command implementation

use anyhow::{Context, Result};
use ranvier_core::static_gen::{write_json_file, StaticBuildConfig, StaticManifest};
use std::path::PathBuf;
use std::process::Command;

/// Run the static build command
pub fn run_static_build(
    example: Option<&str>,
    only: Option<&str>,
    out: Option<&str>,
    pretty: bool,
) -> Result<()> {
    let output_dir = out.unwrap_or("./dist/static");
    let config = StaticBuildConfig::new()
        .with_output_dir(output_dir)
        .with_pretty(pretty);

    if let Some(only_name) = only {
        run_single_static_build(example, only_name, &config)
    } else {
        run_all_static_build(example, &config)
    }
}

/// Build a single static axon
fn run_single_static_build(
    example: Option<&str>,
    name: &str,
    config: &StaticBuildConfig,
) -> Result<()> {
    println!("Building static axon: {}", name);

    if let Some(example_name) = example {
        // Build from an example project
        build_example_static(example_name, name, config)
    } else {
        // Build from current project (not yet implemented)
        anyhow::bail!(
            "Building from current project is not yet supported. \
            Please specify an example with --example <name>"
        )
    }
}

/// Build all static axons in an example
fn run_all_static_build(example: Option<&str>, config: &StaticBuildConfig) -> Result<()> {
    let example_name = example.context("Please specify an example with --example <name>")?;

    println!("Building all static axons from example: {}", example_name);

    // Create output directory
    let output_dir = PathBuf::from(config.get_output_dir());
    std::fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    // Run the example with static build environment variable
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_dir = std::path::Path::new(manifest_dir).parent().unwrap();

    let output_result = Command::new("cargo")
        .args([
            "run",
            "-q",
            "-p",
            example_name,
            "--",
            "--static-build",
            "--output-dir",
            config.get_output_dir(),
        ])
        .current_dir(workspace_dir)
        .output()
        .context("Failed to run cargo command")?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        anyhow::bail!("Static build failed:\n{}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output_result.stdout);
    if !stdout.is_empty() {
        println!("{}", stdout);
    }

    println!("Static build complete: {}", config.get_output_dir());
    Ok(())
}

/// Build static state from an example project
fn build_example_static(example_name: &str, _name: &str, config: &StaticBuildConfig) -> Result<()> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_dir = std::path::Path::new(manifest_dir).parent().unwrap();

    let output_result = Command::new("cargo")
        .args([
            "run",
            "-q",
            "-p",
            example_name,
            "--",
            "--static-build",
            "--output-dir",
            config.get_output_dir(),
        ])
        .current_dir(workspace_dir)
        .output()
        .context("Failed to run cargo command")?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        anyhow::bail!("Static build failed:\n{}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output_result.stdout);
    if !stdout.is_empty() {
        println!("{}", stdout);
    }

    Ok(())
}

/// Write the manifest file
#[allow(dead_code)]
pub fn write_manifest(output_dir: &PathBuf, manifest: &StaticManifest) -> Result<()> {
    let manifest_path = output_dir.join("manifest.json");
    write_json_file(&manifest_path, manifest, true)?;
    println!("Manifest written to: {}", manifest_path.display());
    Ok(())
}

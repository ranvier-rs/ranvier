//! Ranvier CLI - Command-line interface for Ranvier framework
//!
//! # Commands
//! - `ranvier new <name>` - Create a new Ranvier project
//! - `ranvier check [path]` - Validate project and schematic
//! - `ranvier schematic <example>` - 예제를 실행하고 schematic JSON 출력
//! - `ranvier codegen <input> [output]` - Schematic JSON을 TypeScript 타입으로 변환
//! - `ranvier studio [file]` - Studio 데스크탑 앱 실행
//! - `ranvier build static` - 정적 상태 빌드

mod build;
mod check;
mod dev;
mod new;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ranvier_synapse::TypeScriptGenerator;
use std::process::Command;

/// Ranvier Framework CLI
#[derive(Parser)]
#[command(name = "ranvier")]
#[command(
    author,
    version,
    about = "Command-line interface for Ranvier framework"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new Ranvier project
    New {
        /// Project name
        name: String,

        /// Template to use (minimal, fullstack)
        #[arg(short, long, default_value = "minimal")]
        template: String,
    },

    /// Validate project and schematic
    Check {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,

        /// Automatically fix issues
        #[arg(long)]
        fix: bool,
    },

    /// Start development server
    Dev {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,

        /// Port to listen on (default: 3000)
        #[arg(long)]
        port: Option<u16>,
    },

    /// Extract schematic JSON from an example
    Schematic {
        /// Name of the example to run (e.g., basic-schematic, complex-schematic)
        example: String,

        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Launch Ranvier Studio desktop application
    Studio {
        /// Optional JSON file to open
        file: Option<String>,
    },

    /// Generate TypeScript types from Schematic JSON
    Codegen {
        /// Input Schematic JSON file
        input: String,

        /// Output TypeScript file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Build static state artifacts
    Build {
        #[command(subcommand)]
        target: BuildTarget,
    },
}

#[derive(Subcommand)]
enum BuildTarget {
    /// Generate static state snapshots (SSG)
    Static {
        /// Specific axon to build (default: all discovered)
        #[arg(short, long)]
        only: Option<String>,

        /// Output directory (default: ./dist/static)
        #[arg(short, long)]
        out: Option<String>,

        /// Disable pretty-printing JSON output
        #[arg(long, default_value = "true")]
        pretty: bool,

        /// Example project to build
        #[arg(short, long)]
        example: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::New { name, template } => new::run_new_command(&name, Some(template.as_str())),
        Commands::Check { path, fix } => check::run_check_command(path.as_deref(), fix),
        Commands::Dev { path, port } => dev::run_dev_command(path.as_deref(), port),
        Commands::Schematic { example, output } => {
            run_schematic_command(&example, output.as_deref())
        }
        Commands::Studio { file } => run_studio_command(file.as_deref()),
        Commands::Codegen { input, output } => run_codegen_command(&input, output.as_deref()),
        Commands::Build { target } => run_build_command(target),
    }
}

/// Build command handler
fn run_build_command(target: BuildTarget) -> Result<()> {
    match target {
        BuildTarget::Static {
            only,
            out,
            pretty,
            example,
        } => build::run_static_build(example.as_deref(), only.as_deref(), out.as_deref(), pretty),
    }
}

/// 예제를 실행하고 schematic JSON 추출
fn run_schematic_command(example: &str, output: Option<&str>) -> Result<()> {
    // 예제는 workspace crate이므로 cargo run -p 사용
    // -q (quiet): 빌드 로그를 숨기고 JSON만 stdout으로 출력하기 위함
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_dir = std::path::Path::new(manifest_dir).parent().unwrap();

    let output_result = Command::new("cargo")
        .args(["run", "-q", "-p", example])
        .env("RANVIER_SCHEMATIC", "1")
        .current_dir(workspace_dir)
        .output()
        .context("Failed to run cargo command")?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        anyhow::bail!("Example failed to run:\n{}", stderr);
    }

    let json_output = String::from_utf8_lossy(&output_result.stdout);

    // Validate JSON roughly
    if json_output.trim().is_empty() || !json_output.trim().starts_with('{') {
        anyhow::bail!("Output is not valid JSON:\n{}", json_output);
    }

    match output {
        Some(path) => {
            std::fs::write(path, json_output.as_bytes()).context("Failed to write output file")?;
            println!("Schematic saved to: {}", path);
        }
        None => {
            println!("{}", json_output.trim());
        }
    }

    Ok(())
}

/// Studio 데스크탑 앱 실행
fn run_studio_command(file: Option<&str>) -> Result<()> {
    println!("Launching Ranvier Studio...");

    // Studio 경로 계산
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let studio_path = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("studio");

    // npm run tauri:dev 또는 빌드된 바이너리 실행
    let mut cmd = Command::new("npm");
    cmd.args(["run", "tauri:dev"]).current_dir(&studio_path);

    // 파일이 지정된 경우 환경 변수로 전달
    if let Some(file_path) = file {
        let abs_path = std::fs::canonicalize(file_path).context("Failed to resolve file path")?;
        cmd.env("RANVIER_OPEN_FILE", abs_path);
        println!("Opening file: {}", file_path);
    }

    cmd.spawn().context("Failed to launch Studio")?;

    println!("Studio launched successfully.");
    Ok(())
}

/// Schematic JSON을 TypeScript로 변환
fn run_codegen_command(input_path: &str, output_path: Option<&str>) -> Result<()> {
    // 1. Read JSON
    let json_content = std::fs::read_to_string(input_path)
        .with_context(|| format!("Failed to read input file: {}", input_path))?;

    // 2. Parse Schematic
    let schematic: ranvier_core::schematic::Schematic =
        serde_json::from_str(&json_content).context("Failed to parse Schematic JSON")?;

    // 3. Generate TypeScript
    let generator = TypeScriptGenerator::new();
    let ts_output = generator
        .generate(&schematic)
        .context("Failed to generate TypeScript")?;

    // 4. Output
    match output_path {
        Some(path) => {
            std::fs::write(path, ts_output)
                .with_context(|| format!("Failed to write output file: {}", path))?;
            println!("TypeScript definitions saved to: {}", path);
        }
        None => {
            println!("{}", ts_output);
        }
    }

    Ok(())
}

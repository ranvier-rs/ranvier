//! Project scaffolding command

use anyhow::Result;
use std::fs;
use std::path::Path;

/// Create a new Ranvier project
pub fn run_new_command(name: &str, template: Option<&str>) -> Result<()> {
    let project_dir = Path::new(name);
    let template_name = template.unwrap_or("minimal");

    // Validate project name
    if name.is_empty() {
        anyhow::bail!("Project name cannot be empty");
    }

    // Check if directory already exists
    if project_dir.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    println!("Creating new Ranvier project: {}", name);

    // Create project structure
    create_project_structure(project_dir, name, template_name)?;

    println!("✅ Project created successfully!");
    println!();
    println!("Next steps:");
    println!("  cd {}", name);
    println!("  cargo run");

    Ok(())
}

fn create_project_structure(project_dir: &Path, name: &str, template: &str) -> Result<()> {
    // Create directories
    fs::create_dir_all(project_dir.join("src"))?;

    // Generate Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[dependencies]
ranvier-core = "0.1"
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
anyhow = "1.0"
http = "1"
tokio = {{ version = "1", features = ["full"] }}
tracing-subscriber = "0.3"
"#,
        sanitize_name(name)
    );
    fs::write(project_dir.join("Cargo.toml"), cargo_toml)?;

    // Generate .gitignore
    let gitignore = "/target\n**/*.rs.bk\nCargo.lock\n.DS_Store\ndist/\n";
    fs::write(project_dir.join(".gitignore"), gitignore)?;

    // Generate main.rs based on template
    let main_rs = generate_main_rs(name, template)?;
    fs::write(project_dir.join("src/main.rs"), main_rs)?;

    // Generate README.md
    let readme = format!(
        r#"# {}

A Ranvier application.

## Getting Started

```bash
cargo run
```

## Learn More

- [Ranvier Documentation](https://github.com/ranvier-rs/ranvier)
- [Axon Execution Model](https://github.com/ranvier-rs/ranvier/docs/01_architecture/core_framework.md)
"#,
        name
    );
    fs::write(project_dir.join("README.md"), readme)?;

    Ok(())
}

fn generate_main_rs(name: &str, template: &str) -> Result<String> {
    match template {
        "minimal" => Ok(generate_minimal_main_rs(name)),
        "fullstack" => Ok(generate_fullstack_main_rs(name)),
        _ => {
            anyhow::bail!("Unknown template: {}. Available: minimal, fullstack", template)
        }
    }
}

fn generate_minimal_main_rs(name: &str) -> String {
    let name_lit = format!(r#""{}""#, name);
    format!(
        r#"//! {}
//!
//! A Ranvier application with minimal setup.

use anyhow::Result;
use http::Request;
use ranvier_core::prelude::*;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {{
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let addr: SocketAddr = ([0, 0, 0, 0], 3000).into();
    println!("{{}} starting on {{}}", {}, addr);

    // Build your Axon here
    let axon = Axon::start((), "hello")
        .then(HelloTransition);

    // Example execution
    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    match axon.execute(&mut bus).await? {{
        Outcome::Next(result) => {{
            println!("Result: {{:?}}", result);
        }}
        Outcome::Fault(e) => {{
            eprintln!("Error: {{:?}}", e);
        }}
        _ => {{}}
    }}

    Ok(())
}}

// ============================================================
// Transitions
// ============================================================

pub struct HelloTransition;

impl Transition<(), String> for HelloTransition {{
    async fn run(_input: (), _bus: &mut Bus) -> Outcome<String, anyhow::Error> {{
        Outcome::Next("Hello from Ranvier!".to_string())
    }}
}}
"#,
        name, name_lit
    )
}

fn generate_fullstack_main_rs(name: &str) -> String {
    let name_lit = format!(r#""{}""#, name);
    format!(
        r#"//! {}
//!
//! A Ranvier application with SSG and frontend integration.

use anyhow::Result;
use http::Request;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::{{StaticAxon, StaticManifest, write_json_file}};
use serde::{{Deserialize, Serialize}};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {{
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // Check for static build mode
    if args.len() > 1 && args[1] == "--static-build" {{
        return run_static_build();
    }}

    // Normal server mode
    let addr: SocketAddr = ([0, 0, 0, 0], 3000).into();
    println!("{{}} starting on {{}}", {}, addr);

    let axon = Axon::start((), "app")
        .then(AppStateTransition);

    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    match axon.execute(&mut bus).await? {{
        Outcome::Next(state) => {{
            println!("App state: {{:#?}}", state);
        }}
        _ => {{}}
    }}

    Ok(())
}}

fn run_static_build() -> Result<()> {{
    println!("Running static build...");

    let out_dir = std::path::Path::new("./dist/static");
    std::fs::create_dir_all(out_dir)?;

    let mut manifest = StaticManifest::new();

    // Build static states
    let axon = HomePageAxon;
    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    if let Outcome::Next(state) = axon.generate(&mut bus)? {{
        let path = out_dir.join("home.json");
        write_json_file(&path, &state, true)?;
        manifest.add_state("home", "home.json");
    }}

    // Write manifest
    let manifest_path = out_dir.join("manifest.json");
    write_json_file(&manifest_path, &manifest, true)?;

    println!("Static build complete!");
    Ok(())
}}

// ============================================================
// Transitions
// ============================================================

pub struct AppStateTransition;

impl Transition<(), AppState> for AppStateTransition {{
    async fn run(_input: (), _bus: &mut Bus) -> Outcome<AppState, anyhow::Error> {{
        Outcome::Next(AppState {{
            title: "Welcome to Ranvier".to_string(),
            version: "0.1.0".to_string(),
        }})
    }}
}}

// ============================================================
// Static Axons
// ============================================================

pub struct HomePageAxon;

impl StaticAxon for HomePageAxon {{
    type Output = HomePageState;
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {{
        "home"
    }}

    fn generate(&self, _bus: &mut Bus) -> Result<Outcome<HomePageState, Self::Error>> {{
        Ok(Outcome::Next(HomePageState {{
            title: "Welcome".to_string(),
            features: vec![
                "Axon Execution".to_string(),
                "Schematic Visualization".to_string(),
                "Static State Generation".to_string(),
            ],
        }}))
    }}
}}

// ============================================================
// State Types
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {{
    pub title: String,
    pub version: String,
}}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomePageState {{
    pub title: String,
    pub features: Vec<String>,
}}
"#,
        name, name_lit
    )
}

fn sanitize_name(name: &str) -> String {
    name.replace('-', "_")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

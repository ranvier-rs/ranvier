//! test-full
//!
//! A Ranvier application with SSG and frontend integration.

use anyhow::Result;
use http::Request;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::{StaticAxon, StaticManifest, write_json_file};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // Check for static build mode
    if args.len() > 1 && args[1] == "--static-build" {
        return run_static_build();
    }

    // Normal server mode
    let addr: SocketAddr = ([0, 0, 0, 0], 3000).into();
    println!("{} starting on {}", "test-full", addr);

    let axon = Axon::start((), "app")
        .then(AppStateTransition);

    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    match axon.execute(&mut bus).await? {
        Outcome::Next(state) => {
            println!("App state: {:#?}", state);
        }
        _ => {}
    }

    Ok(())
}

fn run_static_build() -> Result<()> {
    println!("Running static build...");

    let out_dir = std::path::Path::new("./dist/static");
    std::fs::create_dir_all(out_dir)?;

    let mut manifest = StaticManifest::new();

    // Build static states
    let axon = HomePageAxon;
    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    if let Outcome::Next(state) = axon.generate(&mut bus)? {
        let path = out_dir.join("home.json");
        write_json_file(&path, &state, true)?;
        manifest.add_state("home", "home.json");
    }

    // Write manifest
    let manifest_path = out_dir.join("manifest.json");
    write_json_file(&manifest_path, &manifest, true)?;

    println!("Static build complete!");
    Ok(())
}

// ============================================================
// Transitions
// ============================================================

pub struct AppStateTransition;

impl Transition<(), AppState> for AppStateTransition {
    async fn run(_input: (), _bus: &mut Bus) -> Outcome<AppState, anyhow::Error> {
        Outcome::Next(AppState {
            title: "Welcome to Ranvier".to_string(),
            version: "0.1.0".to_string(),
        })
    }
}

// ============================================================
// Static Axons
// ============================================================

pub struct HomePageAxon;

impl StaticAxon for HomePageAxon {
    type Output = HomePageState;
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        "home"
    }

    fn generate(&self, _bus: &mut Bus) -> Result<Outcome<HomePageState, Self::Error>> {
        Ok(Outcome::Next(HomePageState {
            title: "Welcome".to_string(),
            features: vec![
                "Axon Execution".to_string(),
                "Schematic Visualization".to_string(),
                "Static State Generation".to_string(),
            ],
        }))
    }
}

// ============================================================
// State Types
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomePageState {
    pub title: String,
    pub features: Vec<String>,
}

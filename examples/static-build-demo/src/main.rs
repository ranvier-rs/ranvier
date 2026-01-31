//! Static Build Demo
//!
//! This example demonstrates the Static State Generation (SSG) capability of Ranvier.
//!
//! Run modes:
//! - Normal: `cargo run -p static-build-demo`
//! - Static build: `cargo run -p static-build-demo -- --static-build --output-dir ./dist`
//! - Via CLI: `ranvier build static --example static-build-demo`

use anyhow::Result;
use chrono::{DateTime, Utc};
use http::Request;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::static_gen::{write_json_file, StaticAxon, StaticManifest};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

// ============================================================
// Static State Types
// ============================================================

/// Landing page state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LandingState {
    pub title: String,
    pub subtitle: String,
    pub features: Vec<FeatureItem>,
    pub cta: CtaButton,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub icon: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtaButton {
    pub text: String,
    pub link: String,
    pub primary: bool,
}

/// Pricing page state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingState {
    pub title: String,
    pub plans: Vec<PricingPlan>,
    pub currency: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPlan {
    pub id: String,
    pub name: String,
    pub price: u32,
    pub period: String,
    pub features: Vec<String>,
    pub highlighted: bool,
}

/// Documentation index state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocsIndexState {
    pub title: String,
    pub categories: Vec<DocCategory>,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocCategory {
    pub id: String,
    pub title: String,
    pub pages: Vec<DocPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocPage {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub description: String,
}

// ============================================================
// Static Axons
// ============================================================

pub struct LandingPageAxon;

impl StaticAxon for LandingPageAxon {
    type Output = LandingState;
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        "landing_page"
    }

    fn generate(&self, _bus: &mut Bus) -> Result<Outcome<LandingState, Self::Error>> {
        let state = LandingState {
            title: "Welcome to Ranvier".to_string(),
            subtitle: "A Typed Decision Engine for Rust".to_string(),
            features: vec![
                FeatureItem {
                    id: "axon".to_string(),
                    title: "Axon Execution".to_string(),
                    description: "Build typed decision trees with explicit control flow"
                        .to_string(),
                    icon: "⚡".to_string(),
                },
                FeatureItem {
                    id: "schematic".to_string(),
                    title: "Schematic Visualization".to_string(),
                    description: "Auto-generate structural diagrams from your code".to_string(),
                    icon: "📊".to_string(),
                },
                FeatureItem {
                    id: "ssg".to_string(),
                    title: "Static State Generation".to_string(),
                    description: "Build-time state generation for zero cold starts".to_string(),
                    icon: "🚀".to_string(),
                },
                FeatureItem {
                    id: "otel".to_string(),
                    title: "OTLP Native".to_string(),
                    description: "Built-in observability without middleware overhead".to_string(),
                    icon: "📡".to_string(),
                },
            ],
            cta: CtaButton {
                text: "Get Started".to_string(),
                link: "/docs/getting-started".to_string(),
                primary: true,
            },
            generated_at: Utc::now(),
        };

        Ok(Outcome::Next(state))
    }
}

pub struct PricingPageAxon;

impl StaticAxon for PricingPageAxon {
    type Output = PricingState;
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        "pricing_page"
    }

    fn generate(&self, _bus: &mut Bus) -> Result<Outcome<PricingState, Self::Error>> {
        let state = PricingState {
            title: "Simple, Transparent Pricing".to_string(),
            currency: "USD".to_string(),
            plans: vec![
                PricingPlan {
                    id: "starter".to_string(),
                    name: "Starter".to_string(),
                    price: 0,
                    period: "forever".to_string(),
                    features: vec![
                        "Core Axon Engine".to_string(),
                        "Basic Schematic Generation".to_string(),
                        "Community Support".to_string(),
                    ]
                    .into_iter()
                    .collect(),
                    highlighted: false,
                },
                PricingPlan {
                    id: "pro".to_string(),
                    name: "Pro".to_string(),
                    price: 29,
                    period: "month".to_string(),
                    features: vec![
                        "Everything in Starter".to_string(),
                        "Advanced Static Generation".to_string(),
                        "Studio Desktop App".to_string(),
                        "Email Support".to_string(),
                    ]
                    .into_iter()
                    .collect(),
                    highlighted: true,
                },
                PricingPlan {
                    id: "enterprise".to_string(),
                    name: "Enterprise".to_string(),
                    price: 99,
                    period: "month".to_string(),
                    features: vec![
                        "Everything in Pro".to_string(),
                        "Custom Integrations".to_string(),
                        "SLA Guarantee".to_string(),
                        "Dedicated Support".to_string(),
                    ]
                    .into_iter()
                    .collect(),
                    highlighted: false,
                },
            ],
            generated_at: Utc::now(),
        };

        Ok(Outcome::Next(state))
    }
}

pub struct DocsIndexAxon;

impl StaticAxon for DocsIndexAxon {
    type Output = DocsIndexState;
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        "docs_index"
    }

    fn generate(&self, _bus: &mut Bus) -> Result<Outcome<DocsIndexState, Self::Error>> {
        let state = DocsIndexState {
            title: "Ranvier Documentation".to_string(),
            categories: vec![
                DocCategory {
                    id: "getting-started".to_string(),
                    title: "Getting Started".to_string(),
                    pages: vec![
                        DocPage {
                            id: "installation".to_string(),
                            title: "Installation".to_string(),
                            slug: "/docs/installation".to_string(),
                            description: "How to install and configure Ranvier".to_string(),
                        },
                        DocPage {
                            id: "quickstart".to_string(),
                            title: "Quick Start".to_string(),
                            slug: "/docs/quickstart".to_string(),
                            description: "Build your first Axon in 5 minutes".to_string(),
                        },
                    ],
                },
                DocCategory {
                    id: "core-concepts".to_string(),
                    title: "Core Concepts".to_string(),
                    pages: vec![
                        DocPage {
                            id: "axon".to_string(),
                            title: "Axon Execution Model".to_string(),
                            slug: "/docs/axon".to_string(),
                            description: "Understanding the execution layer".to_string(),
                        },
                        DocPage {
                            id: "schematic".to_string(),
                            title: "Schematic Analysis".to_string(),
                            slug: "/docs/schematic".to_string(),
                            description: "Structural visualization and analysis".to_string(),
                        },
                        DocPage {
                            id: "outcome".to_string(),
                            title: "Outcome & Control Flow".to_string(),
                            slug: "/docs/outcome".to_string(),
                            description: "Explicit control flow as data".to_string(),
                        },
                    ],
                },
                DocCategory {
                    id: "advanced".to_string(),
                    title: "Advanced Topics".to_string(),
                    pages: vec![
                        DocPage {
                            id: "ssg".to_string(),
                            title: "Static State Generation".to_string(),
                            slug: "/docs/ssg".to_string(),
                            description: "Build-time state generation for SSG".to_string(),
                        },
                        DocPage {
                            id: "synapse".to_string(),
                            title: "Synapse & Frontend Integration".to_string(),
                            slug: "/docs/synapse".to_string(),
                            description: "Type-safe frontend code generation".to_string(),
                        },
                    ],
                },
            ],
            generated_at: Utc::now(),
        };

        Ok(Outcome::Next(state))
    }
}

// ============================================================
// Static Build Runner
// ============================================================

/// Registry of all static axons in this project
fn get_static_axons() -> Vec<Box<dyn StaticAxon<Output = serde_json::Value, Error = anyhow::Error>>>
{
    vec![
        Box::new(ValueAxon::new(LandingPageAxon)),
        Box::new(ValueAxon::new(PricingPageAxon)),
        Box::new(ValueAxon::new(DocsIndexAxon)),
    ]
}

/// Wrapper to convert any serializable output to serde_json::Value
struct ValueAxon<T> {
    inner: T,
}

impl<T> ValueAxon<T> {
    fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: StaticAxon> StaticAxon for ValueAxon<T>
where
    T::Output: Serialize,
{
    type Output = serde_json::Value;
    type Error = T::Error;

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn generate(&self, bus: &mut Bus) -> Result<Outcome<Self::Output, Self::Error>> {
        let outcome = self.inner.generate(bus)?;
        match outcome {
            Outcome::Next(output) => {
                let value = serde_json::to_value(output)
                    .map_err(|e| anyhow::anyhow!("Serialization failed: {}", e))?;
                Ok(Outcome::Next(value))
            }
            Outcome::Fault(e) => Ok(Outcome::Fault(e)),
            _ => Ok(Outcome::Next(serde_json::json!({}))),
        }
    }
}

/// Run the static build process
fn run_static_build(output_dir: &str) -> Result<()> {
    println!("🏗️  Running static build...");
    println!("   Output directory: {}", output_dir);

    let out_path = PathBuf::from(output_dir);
    let mut manifest = StaticManifest::new();

    let axons = get_static_axons();

    for axon in axons {
        let name = axon.name();
        println!("   📦 Building: {}", name);

        // Create an empty request for static context
        let empty_req = Request::builder().uri("/").body(()).unwrap();
        let mut bus = Bus::new(empty_req);
        match axon.generate(&mut bus) {
            Ok(Outcome::Next(value)) => {
                let file_name = format!("{}.json", name);
                let file_path = out_path.join(&file_name);

                write_json_file(&file_path, &value, true)?;
                println!("     ✅ Wrote: {}", file_name);

                manifest.add_state(name, file_name);
            }
            Ok(Outcome::Fault(e)) => {
                eprintln!("     ❌ Fault: {:?}", e);
            }
            Ok(_) => {
                eprintln!("     ⚠️  Unexpected outcome type");
            }
            Err(e) => {
                eprintln!("     ❌ Error: {}", e);
            }
        }
    }

    // Write manifest
    let manifest_path = out_path.join("manifest.json");
    write_json_file(&manifest_path, &manifest, true)?;
    println!("   📋 Wrote: manifest.json");

    println!("✅ Static build complete!");
    println!("   📁 Output: {}/", out_path.display());

    Ok(())
}

// ============================================================
// Main Entry Point
// ============================================================

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    // Check for static build mode
    let mut static_build = false;
    let mut output_dir = "./dist/static";

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--static-build" => {
                static_build = true;
            }
            "--output-dir" => {
                if i + 1 < args.len() {
                    output_dir = &args[i + 1];
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if static_build {
        run_static_build(output_dir)
    } else {
        // Normal mode: just show what would be built
        println!("🧬 Ranvier Static Build Demo");
        println!();
        println!("This example demonstrates Static State Generation (SSG).");
        println!();
        println!("Available static axons:");
        let axons = get_static_axons();
        for axon in &axons {
            println!("  - {}", axon.name());
        }
        println!();
        println!("To run static build:");
        println!("  cargo run -p static-build-demo -- --static-build");
        println!("  ranvier build static --example static-build-demo");
        println!();

        Ok(())
    }
}

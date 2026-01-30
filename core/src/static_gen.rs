//! Static State Generation Support
//!
//! This module provides traits and types for building static state at build time.
//! Static axons are executed without external input to generate pre-computed state
//! for frontend SSG (Static Site Generation).

use crate::bus::Bus;
use crate::outcome::Outcome;
use crate::schematic::NodeKind;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Legacy trait for static graph nodes (kept for backward compatibility)
pub trait StaticNode {
    /// Unique identifier for the node
    fn id(&self) -> &'static str;

    /// The kind of node (Start, Process, etc.)
    fn kind(&self) -> NodeKind;

    /// List of IDs of nodes this node connects to
    fn next_nodes(&self) -> Vec<&'static str>;
}

/// Marker trait for axons that can be executed statically at build time.
///
/// # Static Safety Contract
///
/// Static axons MUST:
/// - Have NO external input (request-free execution)
/// - Have limited side-effects (read-only operations preferred)
/// - Be deterministic (same input → same output)
/// - NOT use WebSocket, DB writes, or random sources
///
/// # Example
///
/// ```rust
/// struct LandingPageAxon;
///
/// impl StaticAxon for LandingPageAxon {
///     type Output = LandingState;
///     type Error = AppError;
///
///     fn name(&self) -> &'static str {
///         "landing_page"
///     }
///
///     fn generate(&self, bus: &mut Bus) -> Result<Outcome<LandingState, AppError>> {
///         // Load features, pricing from read-only sources
///         Ok(Outcome::Next(LandingState { ... }))
///     }
/// }
/// ```
pub trait StaticAxon: Send + Sync {
    /// The output state type (must be serializable)
    type Output: Serialize;

    /// Error type for static generation failures
    type Error: Into<anyhow::Error> + std::fmt::Debug;

    /// Unique identifier for this static state
    fn name(&self) -> &'static str;

    /// Execute the static axon to generate state.
    ///
    /// This is called at build time with an empty or pre-configured Bus.
    fn generate(&self, bus: &mut Bus) -> Result<Outcome<Self::Output, Self::Error>>;
}

/// Manifest for static build output.
///
/// The manifest lists all generated static states and metadata about the build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticManifest {
    /// Schema version
    pub version: String,

    /// When this build was generated
    pub generated_at: DateTime<Utc>,

    /// List of generated state entries
    pub states: Vec<StaticStateEntry>,
}

impl StaticManifest {
    /// Create a new manifest with the current timestamp
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: Utc::now(),
            states: Vec::new(),
        }
    }

    /// Add a state entry to the manifest
    pub fn add_state(&mut self, name: impl Into<String>, file: impl Into<String>) {
        self.states.push(StaticStateEntry {
            name: name.into(),
            file: file.into(),
            content_type: "application/json".to_string(),
        });
    }
}

impl Default for StaticManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Entry in the static build manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticStateEntry {
    /// Logical name of the state (e.g., "landing_page")
    pub name: String,

    /// Relative path to the generated JSON file
    pub file: String,

    /// MIME type of the content
    pub content_type: String,
}

/// Configuration for static builds.
#[derive(Debug, Clone)]
pub struct StaticBuildConfig {
    /// Output directory for generated files
    pub output_dir: Option<String>,

    /// Optional filter to build only specific axons
    pub only: Option<String>,

    /// Whether to include schematic.json in output
    pub include_schematic: bool,

    /// Whether to pretty-print JSON output
    pub pretty: bool,
}

impl StaticBuildConfig {
    /// Create a new default build config
    pub fn new() -> Self {
        Self {
            output_dir: None,
            only: None,
            include_schematic: true,
            pretty: true,
        }
    }

    /// Set the output directory
    pub fn with_output_dir(mut self, dir: impl Into<String>) -> Self {
        self.output_dir = Some(dir.into());
        self
    }

    /// Filter to build only the specified axon
    pub fn with_only(mut self, name: impl Into<String>) -> Self {
        self.only = Some(name.into());
        self
    }

    /// Enable or disable schematic output
    pub fn with_schematic(mut self, include: bool) -> Self {
        self.include_schematic = include;
        self
    }

    /// Enable or disable pretty JSON output
    pub fn with_pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// Get the default output directory
    pub fn get_output_dir(&self) -> &str {
        self.output_dir.as_deref().unwrap_or("./dist/static")
    }
}

impl Default for StaticBuildConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a single static build execution.
#[derive(Debug)]
pub struct StaticBuildResult {
    /// Name of the axon that was built
    pub name: String,

    /// Path to the generated JSON file
    pub file_path: String,

    /// Whether the build was successful
    pub success: bool,
}

/// Write a serializable value to a JSON file.
pub fn write_json_file<T: Serialize>(
    path: &Path,
    value: &T,
    pretty: bool,
) -> anyhow::Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(path, json)?;
    Ok(())
}

/// Read a JSON file and deserialize it.
pub fn read_json_file<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> anyhow::Result<T> {
    let content = std::fs::read_to_string(path)?;
    let value = serde_json::from_str(&content)?;
    Ok(value)
}

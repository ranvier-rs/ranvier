use ranvier_core::prelude::*;
use ranvier_core::schematic::Schematic;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use ranvier_status::{projections_from_timeline, write_projection_files, TimelineProjectionOptions};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[transition]
async fn step_one(input: i32) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(input + 10)
}

#[transition]
async fn step_two(input: i32) -> Outcome<String, anyhow::Error> {
    Outcome::Next(format!("Result: {}", input))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    let fmt_layer = tracing_subscriber::fmt::layer();
    let inspector_layer = ranvier_inspector::layer();

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(inspector_layer)
        .init();

    tracing::info!("Starting Studio Demo...");

    // Start with i32 -> i32 identity
    let info_axon = Axon::<i32, i32, anyhow::Error>::start("Studio Demo Circuit")
        .then(step_one)
        .then(step_two);

    if info_axon.maybe_export_and_exit_with(|request| {
        tracing::info!(
            "Schematic mode requested. Skipping inspector bootstrap and runtime loop. output={:?}",
            request.output
        );
    })? {
        return Ok(());
    }

    // Configure default local artifact paths for "run once and inspect" workflow.
    let dist_dir = PathBuf::from("./dist/studio-demo");
    fs::create_dir_all(&dist_dir)?;
    let timeline_path = dist_dir.join("timeline.raw.json");
    let public_path = dist_dir.join("trace.public.json");
    let internal_path = dist_dir.join("trace.internal.json");

    set_env_if_missing("RANVIER_TIMELINE_OUTPUT", timeline_path.display().to_string());
    set_env_if_missing("RANVIER_TIMELINE_MODE", "overwrite".to_string());
    set_env_if_missing(
        "RANVIER_TRACE_PUBLIC_PATH",
        public_path.display().to_string(),
    );
    set_env_if_missing(
        "RANVIER_TRACE_INTERNAL_PATH",
        internal_path.display().to_string(),
    );

    let axon = info_axon.serve_inspector(9000);

    tracing::info!("Inspector mode: RANVIER_MODE=dev|prod, enabled by RANVIER_INSPECTOR=1|0");
    tracing::info!("Inspector dev page: http://localhost:9000/quick-view");
    tracing::info!("Raw endpoints: /schematic, /trace/public, /trace/internal (dev only)");
    tracing::info!(
        "Projection artifacts: {}, {}",
        public_path.display(),
        internal_path.display()
    );

    loop {
        tracing::info!("Executing Axon...");
        let _ = axon.execute(50, &(), &mut Bus::new()).await;
        if let Err(err) = regenerate_projection_from_timeline(
            &timeline_path,
            &public_path,
            axon.schematic(),
        ) {
            tracing::warn!("Projection refresh failed: {}", err);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn set_env_if_missing(key: &str, value: String) {
    if std::env::var_os(key).is_none() {
        // Safe in this single-threaded startup path before background worker spawn.
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

fn regenerate_projection_from_timeline(
    timeline_path: &PathBuf,
    public_path: &PathBuf,
    schematic: &Schematic,
) -> anyhow::Result<()> {
    if !timeline_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(timeline_path)?;
    let timeline = serde_json::from_str(&content)?;
    let mut options = TimelineProjectionOptions::new(schematic.name.clone(), schematic.id.clone());
    options.trace_id = "studio-demo-live".to_string();
    let artifacts = projections_from_timeline(&timeline, &options)?;
    let output_dir = public_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("public path has no parent directory"))?;
    write_projection_files(output_dir, &artifacts)?;
    Ok(())
}

//! Status Page CLI Commands
//!
//! `ranvier status build` - 정적 Status Page 생성

use anyhow::{Context, Result};
use ranvier_status::{HealthStatus, StatusData, StatusPageGenerator};
use std::path::Path;

/// Status Page 빌드 실행
pub fn run_status_build(
    output: Option<&str>,
    service_name: Option<&str>,
    status_file: Option<&str>,
) -> Result<()> {
    let output_dir = output.unwrap_or("./dist/status");
    let service = service_name.unwrap_or("Ranvier Service");

    println!("Building Status Page...");
    println!("  Output: {}", output_dir);

    // status.json 파일이 지정된 경우 로드, 아니면 기본 생성
    let status_data = if let Some(file) = status_file {
        let content = std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read status file: {}", file))?;
        serde_json::from_str(&content).context("Failed to parse status.json")?
    } else {
        // 기본 상태 데이터 생성
        create_default_status(service)
    };

    // Status Page 생성
    let generator = StatusPageGenerator::new(output_dir);
    let result = generator.generate(&status_data)?;

    println!("\n✓ Status Page generated successfully!");
    println!("  HTML: {}", result.html_path);
    println!("  JSON: {}", result.status_json_path);
    println!("\nOpen {} in a browser to preview.", result.html_path);

    Ok(())
}

/// Schematic으로부터 Status 빌드 실행
pub fn run_status_from_schematic(
    example: &str,
    output: Option<&str>,
    service_name: Option<&str>,
) -> Result<()> {
    let output_dir = output.unwrap_or("./dist/status");
    let service = service_name.unwrap_or(example);

    println!("Building Status Page from schematic...");
    println!("  Example: {}", example);
    println!("  Output: {}", output_dir);

    // 예제 실행하여 schematic 추출
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_dir = Path::new(manifest_dir).parent().unwrap();

    let output_result = std::process::Command::new("cargo")
        .args(["run", "-q", "-p", example])
        .env("RANVIER_SCHEMATIC", "1")
        .current_dir(workspace_dir)
        .output()
        .context("Failed to run example")?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        anyhow::bail!("Example failed: {}", stderr);
    }

    let schematic_json = String::from_utf8_lossy(&output_result.stdout);

    // Schematic 파싱
    let schematic: ranvier_core::schematic::Schematic =
        serde_json::from_str(&schematic_json).context("Failed to parse schematic JSON")?;

    // StatusData 생성
    let mut status_data = StatusData::new(service);
    status_data.add_circuit(&schematic.name, HealthStatus::Operational);

    // Status Page 생성
    let generator = StatusPageGenerator::new(output_dir);
    let result = generator.generate(&status_data)?;

    // circuit.json도 저장
    let circuit_path = Path::new(output_dir).join("circuit.json");
    std::fs::write(&circuit_path, schematic_json.as_bytes())
        .context("Failed to write circuit.json")?;

    println!("\n✓ Status Page generated successfully!");
    println!("  HTML: {}", result.html_path);
    println!("  Status: {}", result.status_json_path);
    println!("  Circuit: {}", circuit_path.display());
    println!("\nOpen {} in a browser to preview.", result.html_path);

    Ok(())
}

/// 기본 상태 데이터 생성 (데모용)
fn create_default_status(service_name: &str) -> StatusData {
    let mut status = StatusData::new(service_name);

    // 기본 circuit 추가 (데모)
    status.add_circuit("API Gateway", HealthStatus::Operational);
    status.add_circuit("Authentication", HealthStatus::Operational);
    status.add_circuit("Database", HealthStatus::Operational);

    status
}

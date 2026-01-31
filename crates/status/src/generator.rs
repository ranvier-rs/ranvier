//! Status Page Generator
//!
//! StatusData를 기반으로 정적 HTML 파일을 생성합니다.

use crate::data::{CircuitStatus, HealthStatus, Incident, StatusData};
use crate::templates;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Status Page 생성기
pub struct StatusPageGenerator {
    /// 출력 디렉토리
    output_dir: String,
}

impl StatusPageGenerator {
    /// 새 생성기 생성
    pub fn new(output_dir: impl Into<String>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// StatusData로부터 Status Page 생성
    pub fn generate(&self, status: &StatusData) -> Result<GeneratedFiles> {
        // 출력 디렉토리 생성
        let output_path = Path::new(&self.output_dir);
        fs::create_dir_all(output_path).context("Failed to create output directory")?;

        // 1. status.json 저장
        let status_json =
            serde_json::to_string_pretty(status).context("Failed to serialize status data")?;
        let status_file = output_path.join("status.json");
        fs::write(&status_file, &status_json).context("Failed to write status.json")?;

        // 2. HTML 생성
        let html = self.render_html(status);
        let html_file = output_path.join("index.html");
        fs::write(&html_file, &html).context("Failed to write index.html")?;

        Ok(GeneratedFiles {
            html_path: html_file.to_string_lossy().to_string(),
            status_json_path: status_file.to_string_lossy().to_string(),
        })
    }

    /// HTML 렌더링
    fn render_html(&self, status: &StatusData) -> String {
        let circuits_html = self.render_circuits(&status.circuits);
        let incidents_html = self.render_incidents(&status.incidents);

        templates::generate_html(
            &status.service_name,
            status.status.display_text(),
            status.status.css_class(),
            status.status.icon(),
            &status.last_updated.to_rfc3339(),
            &circuits_html,
            &incidents_html,
        )
    }

    /// Circuit 목록 렌더링
    fn render_circuits(&self, circuits: &[CircuitStatus]) -> String {
        if circuits.is_empty() {
            return r#"<div class="circuit-item"><span class="circuit-name">No circuits configured</span></div>"#.to_string();
        }

        circuits
            .iter()
            .map(|c| self.render_circuit(c))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 단일 Circuit 렌더링
    fn render_circuit(&self, circuit: &CircuitStatus) -> String {
        let dot_class = match circuit.status {
            HealthStatus::Operational => "operational",
            HealthStatus::Degraded => "degraded",
            HealthStatus::PartialOutage => "partial",
            HealthStatus::MajorOutage => "major",
            HealthStatus::Maintenance => "maintenance",
        };

        let latency_info = circuit
            .latency_ms
            .map(|l| format!(" · {:.0}ms", l))
            .unwrap_or_default();

        format!(
            r#"<div class="circuit-item">
  <span class="circuit-name">{name}</span>
  <div class="circuit-status">
    <span class="status-dot {dot_class}"></span>
    <span>{status_text}{latency}</span>
  </div>
</div>"#,
            name = circuit.name,
            dot_class = dot_class,
            status_text = circuit.status.display_text(),
            latency = latency_info,
        )
    }

    /// Incident 목록 렌더링
    fn render_incidents(&self, incidents: &[Incident]) -> String {
        let active: Vec<_> = incidents
            .iter()
            .filter(|i| i.resolved_at.is_none())
            .collect();

        if active.is_empty() {
            return r#"<div class="no-incidents">
  <p>✓ All systems operational</p>
  <p>No incidents reported.</p>
</div>"#
                .to_string();
        }

        let incident_cards: String = active
            .iter()
            .map(|i| self.render_incident(i))
            .collect::<Vec<_>>()
            .join("\n");

        format!(r#"<div class="incident-list">{}</div>"#, incident_cards)
    }

    /// 단일 Incident 렌더링
    fn render_incident(&self, incident: &Incident) -> String {
        let timeline: String = incident
            .updates
            .iter()
            .map(|u| {
                format!(
                    r#"<div class="timeline-item">
  <div class="timeline-time" data-time="{}">{}</div>
  <div class="timeline-message">{}</div>
</div>"#,
                    u.timestamp.to_rfc3339(),
                    u.timestamp.format("%Y-%m-%d %H:%M UTC"),
                    u.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let affected = if incident.affected_circuits.is_empty() {
            String::new()
        } else {
            format!(" · Affects: {}", incident.affected_circuits.join(", "))
        };

        format!(
            r#"<div class="incident-card">
  <div class="incident-header">
    <span class="incident-title">{title}</span>
    <span class="incident-status">{status}</span>
  </div>
  <div class="incident-meta">
    <span data-time="{created}">{created_display}</span>{affected}
  </div>
  <div class="incident-timeline">
    {timeline}
  </div>
</div>"#,
            title = incident.title,
            status = incident.status.display_text(),
            created = incident.created_at.to_rfc3339(),
            created_display = incident.created_at.format("%Y-%m-%d %H:%M UTC"),
            affected = affected,
            timeline = timeline,
        )
    }
}

/// 생성된 파일 경로
pub struct GeneratedFiles {
    pub html_path: String,
    pub status_json_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_generate_status_page() {
        let temp_dir = tempdir().unwrap();
        let generator = StatusPageGenerator::new(temp_dir.path().to_string_lossy().to_string());

        let mut status = StatusData::new("Test Service");
        status.add_circuit("UserAuth", HealthStatus::Operational);
        status.add_circuit("Payment", HealthStatus::Degraded);

        let result = generator.generate(&status).unwrap();

        // 파일 생성 확인
        assert!(Path::new(&result.html_path).exists());
        assert!(Path::new(&result.status_json_path).exists());

        // HTML 내용 확인
        let html = fs::read_to_string(&result.html_path).unwrap();
        assert!(html.contains("Test Service"));
        assert!(html.contains("UserAuth"));
        assert!(html.contains("Payment"));
    }
}

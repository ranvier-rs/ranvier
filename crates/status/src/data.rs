//! Status Page 데이터 모델
//!
//! circuit.json과 status.json 형식을 정의합니다.

use chrono::{DateTime, Utc};
use ranvier_core::schematic::Schematic;
use serde::{Deserialize, Serialize};

// ============================================================================
// Circuit Data (circuit.json)
// ============================================================================

/// circuit.json 형식 - Schematic 기반 Circuit 구조
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitData {
    /// 데이터 형식 버전
    pub version: String,
    /// 생성 시각
    pub generated_at: DateTime<Utc>,
    /// Circuit 목록 (Schematic 배열)
    pub circuits: Vec<Schematic>,
}

impl CircuitData {
    /// 새 CircuitData 생성
    pub fn new(circuits: Vec<Schematic>) -> Self {
        Self {
            version: "1.0".to_string(),
            generated_at: Utc::now(),
            circuits,
        }
    }

    /// Schematic 하나로부터 생성
    pub fn from_schematic(schematic: Schematic) -> Self {
        Self::new(vec![schematic])
    }
}

// ============================================================================
// Status Data (status.json)
// ============================================================================

/// status.json 형식 - 서비스 상태 정보
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusData {
    /// 서비스 이름
    pub service_name: String,
    /// 전체 상태
    pub status: HealthStatus,
    /// 마지막 업데이트 시각
    pub last_updated: DateTime<Utc>,
    /// Circuit별 상태
    pub circuits: Vec<CircuitStatus>,
    /// Incident 목록
    pub incidents: Vec<Incident>,
}

impl StatusData {
    /// 기본 Operational 상태로 새 StatusData 생성
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            status: HealthStatus::Operational,
            last_updated: Utc::now(),
            circuits: Vec::new(),
            incidents: Vec::new(),
        }
    }

    /// Circuit 상태 추가
    pub fn add_circuit(&mut self, name: impl Into<String>, status: HealthStatus) {
        self.circuits.push(CircuitStatus {
            name: name.into(),
            status,
            latency_ms: None,
            error_rate: None,
            description: None,
        });
    }
}

/// 서비스/Circuit 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// 정상 운영
    Operational,
    /// 성능 저하
    Degraded,
    /// 부분 장애
    PartialOutage,
    /// 전면 장애
    MajorOutage,
    /// 점검 중
    Maintenance,
}

impl HealthStatus {
    /// CSS 클래스명 반환
    pub fn css_class(&self) -> &'static str {
        match self {
            HealthStatus::Operational => "status-operational",
            HealthStatus::Degraded => "status-degraded",
            HealthStatus::PartialOutage => "status-partial",
            HealthStatus::MajorOutage => "status-major",
            HealthStatus::Maintenance => "status-maintenance",
        }
    }

    /// 표시용 텍스트
    pub fn display_text(&self) -> &'static str {
        match self {
            HealthStatus::Operational => "Operational",
            HealthStatus::Degraded => "Degraded Performance",
            HealthStatus::PartialOutage => "Partial Outage",
            HealthStatus::MajorOutage => "Major Outage",
            HealthStatus::Maintenance => "Under Maintenance",
        }
    }

    /// 아이콘 이모지
    pub fn icon(&self) -> &'static str {
        match self {
            HealthStatus::Operational => "✓",
            HealthStatus::Degraded => "⚠",
            HealthStatus::PartialOutage => "⚠",
            HealthStatus::MajorOutage => "✕",
            HealthStatus::Maintenance => "🔧",
        }
    }
}

/// Circuit 상태 정보
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitStatus {
    /// Circuit 이름
    pub name: String,
    /// 현재 상태
    pub status: HealthStatus,
    /// 평균 레이턴시 (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    /// 에러율 (0.0 ~ 1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_rate: Option<f64>,
    /// 추가 설명
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ============================================================================
// Incident Data
// ============================================================================

/// Incident 정보
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    /// 고유 ID
    pub id: String,
    /// 제목
    pub title: String,
    /// 현재 상태
    pub status: IncidentStatus,
    /// 영향 받는 Circuit 목록
    pub affected_circuits: Vec<String>,
    /// 생성 시각
    pub created_at: DateTime<Utc>,
    /// 해결 시각
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// 업데이트 타임라인
    pub updates: Vec<IncidentUpdate>,
}

/// Incident 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncidentStatus {
    /// 조사 중
    Investigating,
    /// 확인됨
    Identified,
    /// 모니터링 중
    Monitoring,
    /// 해결됨
    Resolved,
}

impl IncidentStatus {
    /// 표시용 텍스트
    pub fn display_text(&self) -> &'static str {
        match self {
            IncidentStatus::Investigating => "Investigating",
            IncidentStatus::Identified => "Identified",
            IncidentStatus::Monitoring => "Monitoring",
            IncidentStatus::Resolved => "Resolved",
        }
    }
}

/// Incident 업데이트
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentUpdate {
    /// 업데이트 시각
    pub timestamp: DateTime<Utc>,
    /// 상태
    pub status: IncidentStatus,
    /// 메시지
    pub message: String,
}

/// 전체 서비스 상태 (circuit.json + status.json 통합)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub circuit_data: CircuitData,
    pub status_data: StatusData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_status_serialization() {
        let status = HealthStatus::Operational;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"operational\"");
    }

    #[test]
    fn test_status_data_creation() {
        let mut status = StatusData::new("Test Service");
        status.add_circuit("UserAuth", HealthStatus::Operational);
        status.add_circuit("Payment", HealthStatus::Degraded);

        assert_eq!(status.circuits.len(), 2);
        assert_eq!(status.circuits[0].name, "UserAuth");
    }
}

//! # Ranvier Status
//!
//! 정적 Status Page 생성기. Schematic 데이터를 기반으로
//! HTML/CSS/JS 파일을 생성하여 서비스 상태를 공용으로 공개합니다.
//!
//! ## 핵심 개념
//!
//! - **circuit.json**: Schematic 기반 Circuit 구조 데이터
//! - **status.json**: 서비스/Circuit 상태 및 Incident 정보
//! - **index.html**: 정적 Status Page (다크모드 지원)

pub mod data;
pub mod generator;
mod templates;

pub use data::{
    CircuitData, CircuitStatus, HealthStatus, Incident, IncidentStatus, IncidentUpdate,
    ServiceStatus, StatusData,
};
pub use generator::StatusPageGenerator;

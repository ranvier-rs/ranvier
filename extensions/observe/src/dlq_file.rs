use async_trait::async_trait;
use ranvier_core::event::DlqSink;
use std::path::{Path, PathBuf};
use tokio::fs::{OpenOptions, create_dir_all};
use tokio::io::AsyncWriteExt;
use chrono::Utc;

/// A file-based DLQ sink that stores failed events as JSON lines.
pub struct FileDlqSink {
    storage_dir: PathBuf,
}

impl FileDlqSink {
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let storage_dir = path.as_ref().to_path_buf();
        if !storage_dir.exists() {
            create_dir_all(&storage_dir).await?;
        }
        Ok(Self { storage_dir })
    }
}

#[async_trait]
impl DlqSink for FileDlqSink {
    async fn store_dead_letter(
        &self,
        workflow_id: &str,
        circuit_label: &str,
        node_id: &str,
        error_msg: &str,
        payload: &[u8],
    ) -> Result<(), String> {
        let timestamp = Utc::now().to_rfc3339();
        
        let entry = serde_json::json!({
            "timestamp": timestamp,
            "workflow_id": workflow_id,
            "circuit_label": circuit_label,
            "node_id": node_id,
            "error": error_msg,
            "payload_base64": base64::encode(payload),
        });

        let mut file_path = self.storage_dir.clone();
        file_path.push(format!("dlq_{}.jsonl", Utc::now().format("%Y%m%d")));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)
            .await
            .map_err(|e| e.to_string())?;

        let mut line = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
        line.push(b'\n');

        file.write_all(&line).await.map_err(|e| e.to_string())?;
        file.flush().await.map_err(|e| e.to_string())?;

        Ok(())
    }
}

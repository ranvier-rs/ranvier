use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use ranvier_core::event::{DeadLetter, DlqReader, DlqSink};
use std::path::{Path, PathBuf};
use tokio::fs::{self, OpenOptions, create_dir_all};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
            "payload_base64": base64::engine::general_purpose::STANDARD.encode(payload),
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

#[async_trait]
impl DlqReader for FileDlqSink {
    async fn list_dead_letters(
        &self,
        workflow_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DeadLetter>, String> {
        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(&self.storage_dir)
            .await
            .map_err(|e| e.to_string())?;

        while let Some(entry) = read_dir.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let file = fs::File::open(&path).await.map_err(|e| e.to_string())?;
            let reader = BufReader::new(file);
            let mut lines = reader.lines();

            while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
                if let Ok(dl) = serde_json::from_str::<DeadLetter>(&line) {
                    if let Some(filter) = workflow_filter
                        && dl.workflow_id != filter
                    {
                        continue;
                    }
                    entries.push(dl);
                    if entries.len() >= limit {
                        return Ok(entries);
                    }
                }
            }
        }

        // Sort newest first
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(entries)
    }

    async fn count_dead_letters(&self) -> Result<u64, String> {
        let mut count = 0u64;
        let mut read_dir = fs::read_dir(&self.storage_dir)
            .await
            .map_err(|e| e.to_string())?;

        while let Some(entry) = read_dir.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let file = fs::File::open(&path).await.map_err(|e| e.to_string())?;
            let reader = BufReader::new(file);
            let mut lines = reader.lines();

            while lines
                .next_line()
                .await
                .map_err(|e| e.to_string())?
                .is_some()
            {
                count += 1;
            }
        }

        Ok(count)
    }
}

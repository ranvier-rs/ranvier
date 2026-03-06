use crate::{AuditError, AuditEvent, AuditQuery, AuditSink, RetentionPolicy};
use async_trait::async_trait;
use ring::hmac;
use std::path::{Path, PathBuf};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A sink that appends structured audit events to a JSON Lines file,
/// computing a cryptographic HMAC over each entry for tamper evidence.
///
/// Supports querying events from the file and applying retention policies.
pub struct FileAuditSink {
    path: PathBuf,
    key: hmac::Key,
}

impl FileAuditSink {
    pub async fn new(path: impl AsRef<Path>, secret_key: &[u8]) -> Result<Self, AuditError> {
        let p = path.as_ref().to_path_buf();

        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AuditError::Internal(e.to_string()))?;
        }

        let key = hmac::Key::new(hmac::HMAC_SHA256, secret_key);

        Ok(Self { path: p, key })
    }

    /// Helper to compute the HMAC signature of a serialized event
    fn sign(&self, payload: &str) -> String {
        let tag = hmac::sign(&self.key, payload.as_bytes());
        hex::encode(tag.as_ref())
    }

    /// Read all events from the file.
    async fn read_all_events(&self) -> Result<Vec<AuditEvent>, AuditError> {
        let contents = fs::read_to_string(&self.path)
            .await
            .unwrap_or_default();

        if contents.is_empty() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.path)
            .await
            .map_err(|e| AuditError::Internal(e.to_string()))?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut events = Vec::new();

        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| AuditError::Internal(e.to_string()))?
        {
            if line.trim().is_empty() {
                continue;
            }
            let envelope: serde_json::Value = serde_json::from_str(&line)?;
            if let Some(event_val) = envelope.get("event") {
                let event: AuditEvent = serde_json::from_value(event_val.clone())?;
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Write a list of events back to the file, replacing all content.
    async fn write_all_events(&self, events: &[AuditEvent]) -> Result<(), AuditError> {
        let mut content = String::new();
        for event in events {
            let payload = serde_json::to_string(event)?;
            let signature = self.sign(&payload);
            let envelope = serde_json::json!({
                "event": event,
                "signature": signature,
            });
            content.push_str(&serde_json::to_string(&envelope)?);
            content.push('\n');
        }

        fs::write(&self.path, content.as_bytes())
            .await
            .map_err(|e| AuditError::AppendFailed(e.to_string()))?;

        Ok(())
    }
}

#[async_trait]
impl AuditSink for FileAuditSink {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let payload = serde_json::to_string(event)?;
        let signature = self.sign(&payload);

        let envelope = serde_json::json!({
            "event": event,
            "signature": signature,
        });

        let line = format!("{}\n", serde_json::to_string(&envelope)?);

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| AuditError::AppendFailed(e.to_string()))?;

        file.write_all(line.as_bytes())
            .await
            .map_err(|e| AuditError::AppendFailed(e.to_string()))?;

        Ok(())
    }

    async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        let events = self.read_all_events().await?;
        Ok(query.filter(&events).into_iter().cloned().collect())
    }

    async fn apply_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        let events = self.read_all_events().await?;
        let (retained, expired) = policy.apply(&events);
        self.write_all_events(&retained).await?;
        Ok(expired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_file_audit_sink_signing() {
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();

        let sink = FileAuditSink::new(&path, b"super-secret-key")
            .await
            .unwrap();

        let event = AuditEvent::new(
            "sig_test".into(),
            "admin".into(),
            "CONFIG_CHANGE".into(),
            "system_properties".into(),
        );

        sink.append(&event).await.unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(contents.contains("sig_test"));
        assert!(contents.contains("signature"));

        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(parsed.get("signature").is_some());
        assert!(parsed.get("event").is_some());
    }

    #[tokio::test]
    async fn test_file_sink_query() {
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();

        let sink = FileAuditSink::new(&path, b"key").await.unwrap();

        sink.append(&AuditEvent::new("1".into(), "alice".into(), "CREATE".into(), "doc".into()))
            .await
            .unwrap();
        sink.append(&AuditEvent::new("2".into(), "bob".into(), "READ".into(), "doc".into()))
            .await
            .unwrap();

        let results = sink.query(&AuditQuery::new().actor("alice")).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "1");
    }

    #[tokio::test]
    async fn test_file_sink_retention() {
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();

        let sink = FileAuditSink::new(&path, b"key").await.unwrap();

        for i in 0..5 {
            sink.append(&AuditEvent::new(
                format!("evt_{i}"),
                "sys".into(),
                "LOG".into(),
                "svc".into(),
            ))
            .await
            .unwrap();
        }

        let expired = sink
            .apply_retention(&RetentionPolicy::max_count(3))
            .await
            .unwrap();
        assert_eq!(expired.len(), 2);

        let remaining = sink.read_all_events().await.unwrap();
        assert_eq!(remaining.len(), 3);
    }
}

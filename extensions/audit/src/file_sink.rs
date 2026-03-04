use crate::{AuditError, AuditEvent, AuditSink};
use async_trait::async_trait;
use ring::hmac;
use std::path::{Path, PathBuf};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

/// A sink that appends structured audit events to a file,
/// computing a cryptographic HMAC over each entry for tamper evidence.
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
}

#[async_trait]
impl AuditSink for FileAuditSink {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let payload = serde_json::to_string(event)?;

        let signature = self.sign(&payload);

        // Wrap with signature
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

        // Basic verification
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(parsed.get("signature").is_some());
        assert!(parsed.get("event").is_some());
    }
}

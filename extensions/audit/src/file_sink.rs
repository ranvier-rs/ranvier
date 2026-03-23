use crate::{AuditError, AuditEvent, AuditQuery, AuditSink, RetentionPolicy};
use async_trait::async_trait;
use ring::hmac;
use std::path::{Path, PathBuf};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Rotation policy for audit log files.
#[derive(Debug, Clone)]
pub enum RotationPolicy {
    /// No rotation (default).
    None,
    /// Rotate when file exceeds this size in bytes.
    BySize(u64),
    /// Rotate daily (check date on each append).
    ByDate,
    /// Rotate by whichever condition triggers first.
    ByBoth(u64),
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self::None
    }
}

/// A sink that appends structured audit events to a JSON Lines file,
/// computing a cryptographic HMAC over each entry for tamper evidence.
///
/// Supports querying events from the file, applying retention policies,
/// and automatic log rotation by size or date.
pub struct FileAuditSink {
    path: PathBuf,
    key: hmac::Key,
    rotation: RotationPolicy,
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

        Ok(Self {
            path: p,
            key,
            rotation: RotationPolicy::None,
        })
    }

    /// Set the rotation policy for this sink.
    pub fn with_rotation(mut self, policy: RotationPolicy) -> Self {
        self.rotation = policy;
        self
    }

    /// Check if rotation is needed and perform it before the next write.
    async fn maybe_rotate(&self) -> Result<(), AuditError> {
        match &self.rotation {
            RotationPolicy::None => Ok(()),
            RotationPolicy::BySize(max_bytes) => {
                self.rotate_if_size_exceeded(*max_bytes).await
            }
            RotationPolicy::ByDate => {
                self.rotate_if_date_changed().await
            }
            RotationPolicy::ByBoth(max_bytes) => {
                self.rotate_if_size_exceeded(*max_bytes).await?;
                self.rotate_if_date_changed().await
            }
        }
    }

    async fn rotate_if_size_exceeded(&self, max_bytes: u64) -> Result<(), AuditError> {
        let metadata = match fs::metadata(&self.path).await {
            Ok(m) => m,
            Err(_) => return Ok(()), // File doesn't exist yet
        };
        if metadata.len() >= max_bytes {
            self.rotate_file().await?;
        }
        Ok(())
    }

    async fn rotate_if_date_changed(&self) -> Result<(), AuditError> {
        let metadata = match fs::metadata(&self.path).await {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        let modified = metadata
            .modified()
            .map_err(|e| AuditError::Internal(e.to_string()))?;
        let modified_date = chrono::DateTime::<chrono::Utc>::from(modified)
            .format("%Y-%m-%d")
            .to_string();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if modified_date != today {
            self.rotate_file().await?;
        }
        Ok(())
    }

    /// Rename the current log file with a date + sequence suffix.
    async fn rotate_file(&self) -> Result<(), AuditError> {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let stem = self.path.to_string_lossy().to_string();

        // Find next available sequence number
        for seq in 1u32.. {
            let rotated = format!("{stem}.{date}.{seq}");
            let rotated_path = PathBuf::from(&rotated);
            if !rotated_path.exists() {
                fs::rename(&self.path, &rotated_path)
                    .await
                    .map_err(|e| AuditError::Internal(format!("rotation rename failed: {e}")))?;
                tracing::info!(
                    rotated_to = %rotated,
                    "Audit log rotated"
                );
                return Ok(());
            }
        }
        Ok(())
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
        // Check rotation before writing
        self.maybe_rotate().await?;

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

    // --- Rotation tests ---

    #[tokio::test]
    async fn rotation_by_size_creates_rotated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let sink = FileAuditSink::new(&path, b"key")
            .await
            .unwrap()
            .with_rotation(RotationPolicy::BySize(200)); // rotate at 200 bytes

        // Write enough events to exceed 200 bytes
        for i in 0..3 {
            sink.append(&AuditEvent::new(
                format!("evt_{i}"),
                "actor".into(),
                "ACTION".into(),
                "resource".into(),
            ))
            .await
            .unwrap();
        }

        // After exceeding 200 bytes, rotation should have kicked in.
        // The rotated file should exist with a date+sequence suffix.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        // Should have at least 2 files: the current log + at least one rotated
        assert!(
            entries.len() >= 2,
            "expected at least 2 files after rotation, found {}",
            entries.len()
        );

        // Current log should still be writable
        sink.append(&AuditEvent::new(
            "after_rotation".into(),
            "actor".into(),
            "ACTION".into(),
            "resource".into(),
        ))
        .await
        .unwrap();

        let current = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(current.contains("after_rotation"));
    }

    #[tokio::test]
    async fn rotation_none_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let sink = FileAuditSink::new(&path, b"key")
            .await
            .unwrap()
            .with_rotation(RotationPolicy::None);

        for i in 0..5 {
            sink.append(&AuditEvent::new(
                format!("evt_{i}"),
                "actor".into(),
                "ACTION".into(),
                "resource".into(),
            ))
            .await
            .unwrap();
        }

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        // Only 1 file — no rotation
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn rotation_policy_default_is_none() {
        assert!(matches!(RotationPolicy::default(), RotationPolicy::None));
    }
}

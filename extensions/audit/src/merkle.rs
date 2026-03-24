//! Merkle tree audit verification for tamper-proof batch integrity.
//!
//! Builds a binary SHA-256 Merkle tree over batches of `AuditEvent`s,
//! producing a single root hash that can be anchored to an external store.
//! Membership proofs allow any individual event to be verified against
//! the anchored root without replaying the entire batch.

use crate::{AuditError, AuditEvent, AuditQuery, AuditSink, RetentionPolicy};
use async_trait::async_trait;
use ring::digest;
use std::sync::Arc;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Position of a sibling hash in a Merkle proof step.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SiblingPosition {
    Left,
    Right,
}

/// One step of a Merkle inclusion proof.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProofSibling {
    pub hash: String,
    pub position: SiblingPosition,
}

/// A Merkle inclusion proof for a single leaf.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MerkleProof {
    pub leaf_index: usize,
    pub leaf_hash: String,
    pub siblings: Vec<ProofSibling>,
    pub root: String,
}

// ---------------------------------------------------------------------------
// Pure Merkle functions
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hash of an audit event's canonical JSON.
pub fn hash_event(event: &AuditEvent) -> String {
    let payload = serde_json::to_string(event).unwrap_or_default();
    let d = digest::digest(&digest::SHA256, payload.as_bytes());
    hex::encode(d.as_ref())
}

/// Combine two hex-encoded hashes into one SHA-256 hash.
pub fn hash_pair(left: &str, right: &str) -> String {
    let combined = format!("{left}{right}");
    let d = digest::digest(&digest::SHA256, combined.as_bytes());
    hex::encode(d.as_ref())
}

/// Build a binary Merkle tree from leaf hashes.
///
/// Returns `(root_hash, layers)` where `layers[0]` is the leaf layer.
/// Odd layers are padded by duplicating the last element.
pub fn build_merkle_tree(leaves: &[String]) -> (String, Vec<Vec<String>>) {
    if leaves.is_empty() {
        return (String::new(), vec![]);
    }
    if leaves.len() == 1 {
        return (leaves[0].clone(), vec![leaves.to_vec()]);
    }

    let mut layers: Vec<Vec<String>> = vec![leaves.to_vec()];
    let mut current = leaves.to_vec();

    while current.len() > 1 {
        // Pad odd layer
        if current.len() % 2 != 0 {
            current.push(current.last().unwrap().clone());
        }
        let mut next = Vec::with_capacity(current.len() / 2);
        for pair in current.chunks(2) {
            next.push(hash_pair(&pair[0], &pair[1]));
        }
        layers.push(next.clone());
        current = next;
    }

    (current[0].clone(), layers)
}

/// Generate a Merkle inclusion proof for the leaf at `index`.
pub fn generate_proof(index: usize, layers: &[Vec<String>]) -> Option<MerkleProof> {
    if layers.is_empty() || index >= layers[0].len() {
        return None;
    }

    let leaf_hash = layers[0][index].clone();
    let mut siblings = Vec::new();
    let mut idx = index;

    for layer in &layers[..layers.len() - 1] {
        // Pad the layer view for odd lengths
        let mut padded = layer.clone();
        if padded.len() % 2 != 0 {
            padded.push(padded.last().unwrap().clone());
        }

        let sibling_idx = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        let position = if idx % 2 == 0 {
            SiblingPosition::Right
        } else {
            SiblingPosition::Left
        };

        siblings.push(ProofSibling {
            hash: padded[sibling_idx].clone(),
            position,
        });

        idx /= 2;
    }

    let root = layers.last().unwrap()[0].clone();

    Some(MerkleProof {
        leaf_index: index,
        leaf_hash,
        siblings,
        root,
    })
}

/// Verify a Merkle inclusion proof.
pub fn verify_proof(proof: &MerkleProof) -> bool {
    let mut current = proof.leaf_hash.clone();

    for sibling in &proof.siblings {
        current = match sibling.position {
            SiblingPosition::Left => hash_pair(&sibling.hash, &current),
            SiblingPosition::Right => hash_pair(&current, &sibling.hash),
        };
    }

    current == proof.root
}

// ---------------------------------------------------------------------------
// AnchorService trait
// ---------------------------------------------------------------------------

/// External anchor for Merkle root hashes.
///
/// Implementations may store roots in a database, blockchain, or any
/// append-only medium that provides independent tamper evidence.
#[async_trait]
pub trait AnchorService: Send + Sync {
    /// Record the Merkle root for a batch.
    async fn anchor(
        &self,
        batch_id: u64,
        root_hash: &str,
        event_count: usize,
    ) -> Result<(), AuditError>;

    /// Retrieve a previously anchored root hash.
    async fn get_anchor(&self, batch_id: u64) -> Result<Option<String>, AuditError>;
}

/// In-memory anchor for testing and development.
#[derive(Default)]
pub struct InMemoryAnchorService {
    store: Mutex<std::collections::HashMap<u64, String>>,
}

impl InMemoryAnchorService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_all(&self) -> std::collections::HashMap<u64, String> {
        self.store.lock().await.clone()
    }
}

#[async_trait]
impl AnchorService for InMemoryAnchorService {
    async fn anchor(
        &self,
        batch_id: u64,
        root_hash: &str,
        _event_count: usize,
    ) -> Result<(), AuditError> {
        self.store
            .lock()
            .await
            .insert(batch_id, root_hash.to_string());
        Ok(())
    }

    async fn get_anchor(&self, batch_id: u64) -> Result<Option<String>, AuditError> {
        Ok(self.store.lock().await.get(&batch_id).cloned())
    }
}

// ---------------------------------------------------------------------------
// MerkleAuditSink
// ---------------------------------------------------------------------------

/// A decorator sink that accumulates events into batches, builds a Merkle tree
/// per batch, anchors the root hash, then delegates to an inner `AuditSink`.
pub struct MerkleAuditSink<S: AuditSink> {
    inner: Arc<S>,
    anchor: Arc<dyn AnchorService>,
    batch_size: usize,
    buffer: Arc<Mutex<Vec<AuditEvent>>>,
    batch_counter: Arc<Mutex<u64>>,
}

impl<S: AuditSink> MerkleAuditSink<S> {
    pub fn new(inner: S, anchor: Arc<dyn AnchorService>, batch_size: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            anchor,
            batch_size,
            buffer: Arc::new(Mutex::new(Vec::with_capacity(batch_size))),
            batch_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Flush all buffered events regardless of batch size.
    pub async fn flush_remaining(&self) -> Result<(), AuditError> {
        let events = {
            let mut buf = self.buffer.lock().await;
            if buf.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *buf)
        };
        self.flush_batch(events).await
    }

    /// Generate a Merkle inclusion proof for a specific event within a set.
    pub fn prove(events: &[AuditEvent], index: usize) -> Option<MerkleProof> {
        if index >= events.len() {
            return None;
        }
        let leaves: Vec<String> = events.iter().map(hash_event).collect();
        let (_root, layers) = build_merkle_tree(&leaves);
        generate_proof(index, &layers)
    }

    async fn flush_batch(&self, events: Vec<AuditEvent>) -> Result<(), AuditError> {
        let leaves: Vec<String> = events.iter().map(hash_event).collect();
        let (root, _layers) = build_merkle_tree(&leaves);

        let batch_id = {
            let mut counter = self.batch_counter.lock().await;
            *counter += 1;
            *counter
        };

        self.anchor
            .anchor(batch_id, &root, events.len())
            .await?;

        for event in &events {
            self.inner.append(event).await?;
        }

        Ok(())
    }
}

#[async_trait]
impl<S: AuditSink + 'static> AuditSink for MerkleAuditSink<S> {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let should_flush = {
            let mut buf = self.buffer.lock().await;
            buf.push(event.clone());
            buf.len() >= self.batch_size
        };

        if should_flush {
            let events = {
                let mut buf = self.buffer.lock().await;
                std::mem::take(&mut *buf)
            };
            self.flush_batch(events).await?;
        }

        Ok(())
    }

    async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        self.inner.query(query).await
    }

    async fn apply_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        self.inner.apply_retention(policy).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryAuditSink;

    fn make_event(id: &str, action: &str) -> AuditEvent {
        AuditEvent::new(
            id.to_string(),
            "test-actor".to_string(),
            action.to_string(),
            "test-target".to_string(),
        )
    }

    #[test]
    fn hash_event_deterministic() {
        let e1 = AuditEvent {
            id: "ev1".into(),
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            actor: "alice".into(),
            action: "create".into(),
            target: "resource/1".into(),
            intent: None,
            metadata: Default::default(),
            prev_hash: None,
        };
        let e2 = e1.clone();
        assert_eq!(hash_event(&e1), hash_event(&e2));
        assert!(!hash_event(&e1).is_empty());
    }

    #[test]
    fn build_tree_single_leaf() {
        let leaves = vec!["abc123".to_string()];
        let (root, layers) = build_merkle_tree(&leaves);
        assert_eq!(root, "abc123");
        assert_eq!(layers.len(), 1);
    }

    #[test]
    fn build_tree_even_leaves() {
        let leaves: Vec<String> = (0..4).map(|i| format!("leaf_{i}")).collect();
        let (root, layers) = build_merkle_tree(&leaves);
        assert_eq!(layers.len(), 3); // 4 leaves → 2 nodes → 1 root
        assert!(!root.is_empty());

        // Verify manually
        let h01 = hash_pair("leaf_0", "leaf_1");
        let h23 = hash_pair("leaf_2", "leaf_3");
        let expected_root = hash_pair(&h01, &h23);
        assert_eq!(root, expected_root);
    }

    #[test]
    fn build_tree_odd_leaves_pads() {
        let leaves: Vec<String> = (0..3).map(|i| format!("leaf_{i}")).collect();
        let (root, layers) = build_merkle_tree(&leaves);
        assert_eq!(layers.len(), 3); // 3→4(padded) leaves → 2 nodes → 1 root
        assert!(!root.is_empty());

        // 3 leaves: pad last → [leaf_0, leaf_1, leaf_2, leaf_2]
        let h01 = hash_pair("leaf_0", "leaf_1");
        let h22 = hash_pair("leaf_2", "leaf_2");
        let expected_root = hash_pair(&h01, &h22);
        assert_eq!(root, expected_root);
    }

    #[test]
    fn generate_and_verify_proof() {
        let leaves: Vec<String> = (0..8).map(|i| format!("leaf_{i}")).collect();
        let (_root, layers) = build_merkle_tree(&leaves);

        for i in 0..8 {
            let proof = generate_proof(i, &layers).expect("proof should exist");
            assert!(verify_proof(&proof), "proof failed for index {i}");
        }
    }

    #[test]
    fn verify_proof_rejects_tampered_leaf() {
        let leaves: Vec<String> = (0..4).map(|i| format!("leaf_{i}")).collect();
        let (_root, layers) = build_merkle_tree(&leaves);

        let mut proof = generate_proof(0, &layers).unwrap();
        proof.leaf_hash = "tampered_hash".to_string();
        assert!(!verify_proof(&proof));
    }

    #[tokio::test]
    async fn merkle_sink_flushes_at_batch_size() {
        let inner = InMemoryAuditSink::new();
        let anchor = Arc::new(InMemoryAnchorService::new());
        let sink = MerkleAuditSink::new(inner.clone(), anchor.clone(), 3);

        // Append 3 events — should trigger flush
        for i in 0..3 {
            let event = make_event(&format!("ev{i}"), "create");
            sink.append(&event).await.unwrap();
        }

        assert_eq!(inner.len().await, 3);
        let anchors = anchor.get_all().await;
        assert_eq!(anchors.len(), 1);
        assert!(anchors.contains_key(&1));
    }

    #[tokio::test]
    async fn merkle_sink_flush_remaining() {
        let inner = InMemoryAuditSink::new();
        let anchor = Arc::new(InMemoryAnchorService::new());
        let sink = MerkleAuditSink::new(inner.clone(), anchor.clone(), 10);

        // Append fewer than batch_size
        for i in 0..4 {
            let event = make_event(&format!("ev{i}"), "update");
            sink.append(&event).await.unwrap();
        }

        assert_eq!(inner.len().await, 0); // not yet flushed
        sink.flush_remaining().await.unwrap();
        assert_eq!(inner.len().await, 4);

        let anchors = anchor.get_all().await;
        assert_eq!(anchors.len(), 1);
    }

    #[tokio::test]
    async fn merkle_sink_delegates_query() {
        let inner = InMemoryAuditSink::new();
        let anchor = Arc::new(InMemoryAnchorService::new());

        // Pre-populate inner sink directly
        let event = make_event("direct", "read");
        inner.append(&event).await.unwrap();

        let sink = MerkleAuditSink::new(inner, anchor, 100);
        let results = sink.query(&AuditQuery::new().action("read")).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "direct");
    }

    #[tokio::test]
    async fn in_memory_anchor_stores_and_retrieves() {
        let anchor = InMemoryAnchorService::new();
        anchor.anchor(42, "root_abc", 10).await.unwrap();

        let retrieved = anchor.get_anchor(42).await.unwrap();
        assert_eq!(retrieved, Some("root_abc".to_string()));

        let missing = anchor.get_anchor(99).await.unwrap();
        assert!(missing.is_none());
    }
}

use crate::models::{ChatMessage, HistoryEntry, Room, RoomInfo, ServerMessage};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Manages chat rooms, membership, and message broadcasting.
#[derive(Clone)]
pub struct RoomManager {
    inner: Arc<Mutex<RoomManagerInner>>,
}

struct RoomManagerInner {
    rooms: HashMap<String, Room>,
    /// room_id → set of user_ids
    members: HashMap<String, HashSet<String>>,
    /// room_id → message history
    history: HashMap<String, Vec<ChatMessage>>,
    /// user_id → sender channel for ServerMessage
    senders: HashMap<String, tokio::sync::mpsc::UnboundedSender<ServerMessage>>,
}

impl RoomManager {
    pub fn new() -> Self {
        let mut inner = RoomManagerInner {
            rooms: HashMap::new(),
            members: HashMap::new(),
            history: HashMap::new(),
            senders: HashMap::new(),
        };
        // Create default "general" room
        inner.rooms.insert(
            "general".to_string(),
            Room {
                id: "general".to_string(),
                name: "General".to_string(),
                is_public: true,
                created_by: "system".to_string(),
            },
        );
        inner.members.insert("general".to_string(), HashSet::new());
        inner.history.insert("general".to_string(), Vec::new());

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Register a user's message sender channel.
    pub fn register_sender(
        &self,
        user_id: &str,
        sender: tokio::sync::mpsc::UnboundedSender<ServerMessage>,
    ) {
        self.inner
            .lock()
            .unwrap()
            .senders
            .insert(user_id.to_string(), sender);
    }

    /// Remove a user's sender channel.
    pub fn unregister_sender(&self, user_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.senders.remove(user_id);
        // Remove from all rooms
        let rooms: Vec<String> = inner.members.keys().cloned().collect();
        for room_id in rooms {
            if let Some(members) = inner.members.get_mut(&room_id) {
                members.remove(user_id);
            }
        }
    }

    /// Join a room. Returns member count.
    pub fn join_room(&self, room_id: &str, user_id: &str, username: &str) -> Result<usize, String> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.rooms.contains_key(room_id) {
            return Err(format!("Room '{}' not found", room_id));
        }
        let members = inner.members.entry(room_id.to_string()).or_default();
        members.insert(user_id.to_string());
        let count = members.len();
        let other_members: Vec<String> = members
            .iter()
            .filter(|m| m.as_str() != user_id)
            .cloned()
            .collect();

        // Broadcast join to other members
        let msg = ServerMessage::Joined {
            room: room_id.to_string(),
            user: username.to_string(),
            count,
        };
        for member_id in &other_members {
            if let Some(sender) = inner.senders.get(member_id) {
                let _ = sender.send(msg.clone());
            }
        }

        Ok(count)
    }

    /// Leave a room. Returns member count.
    pub fn leave_room(&self, room_id: &str, user_id: &str, username: &str) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let count = if let Some(members) = inner.members.get_mut(room_id) {
            members.remove(user_id);
            let count = members.len();
            let remaining: Vec<String> = members.iter().cloned().collect();
            // Broadcast leave
            let msg = ServerMessage::Left {
                room: room_id.to_string(),
                user: username.to_string(),
                count,
            };
            for member_id in &remaining {
                if let Some(sender) = inner.senders.get(member_id) {
                    let _ = sender.send(msg.clone());
                }
            }
            count
        } else {
            0
        };
        count
    }

    /// Broadcast a chat message to all members in a room.
    pub fn broadcast_message(
        &self,
        room_id: &str,
        user_id: &str,
        username: &str,
        content: &str,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.rooms.contains_key(room_id) {
            return Err(format!("Room '{}' not found", room_id));
        }
        let members = inner.members.get(room_id).cloned().unwrap_or_default();
        if !members.contains(user_id) {
            return Err(format!("Not a member of room '{}'", room_id));
        }

        let now = Utc::now();
        let msg = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
            username: username.to_string(),
            content: content.to_string(),
            timestamp: now,
        };

        // Store in history
        inner
            .history
            .entry(room_id.to_string())
            .or_default()
            .push(msg.clone());

        // Broadcast to all members (including sender)
        let server_msg = ServerMessage::Message {
            room: room_id.to_string(),
            user: username.to_string(),
            message: content.to_string(),
            timestamp: now.to_rfc3339(),
        };
        for member_id in &members {
            if let Some(sender) = inner.senders.get(member_id) {
                let _ = sender.send(server_msg.clone());
            }
        }

        Ok(())
    }

    /// Get recent message history for a room.
    pub fn get_history(&self, room_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let inner = self.inner.lock().unwrap();
        inner
            .history
            .get(room_id)
            .map(|msgs| {
                msgs.iter()
                    .rev()
                    .take(limit)
                    .rev()
                    .map(|m| HistoryEntry {
                        user: m.username.clone(),
                        message: m.content.clone(),
                        timestamp: m.timestamp.to_rfc3339(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Create a new room.
    pub fn create_room(&self, name: &str, created_by: &str, is_public: bool) -> Room {
        let mut inner = self.inner.lock().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let room = Room {
            id: id.clone(),
            name: name.to_string(),
            is_public,
            created_by: created_by.to_string(),
        };
        inner.rooms.insert(id.clone(), room.clone());
        inner.members.insert(id, HashSet::new());
        room
    }

    /// List all public rooms.
    pub fn list_rooms(&self) -> Vec<RoomInfo> {
        let inner = self.inner.lock().unwrap();
        inner
            .rooms
            .values()
            .filter(|r| r.is_public)
            .map(|r| RoomInfo {
                id: r.id.clone(),
                name: r.name.clone(),
                member_count: inner.members.get(&r.id).map(|m| m.len()).unwrap_or(0),
            })
            .collect()
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub is_public: bool,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub room_id: String,
    pub user_id: String,
    pub username: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// WebSocket message types: Client → Server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Leave { room: String },
    Chat { room: String, message: String },
    Typing { room: String },
}

/// WebSocket message types: Server → Client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome { user: String },
    Joined { room: String, user: String, count: usize },
    Left { room: String, user: String, count: usize },
    Message { room: String, user: String, message: String, timestamp: String },
    History { room: String, messages: Vec<HistoryEntry> },
    Error { code: String, detail: String },
    RoomList { rooms: Vec<RoomInfo> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub user: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub member_count: usize,
}

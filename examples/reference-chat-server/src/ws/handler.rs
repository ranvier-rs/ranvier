use crate::auth::{self, TokenStore};
use crate::models::{ClientMessage, ServerMessage};
use crate::ws::room_manager::RoomManager;
use ranvier_http::prelude::*;
use std::sync::Arc;

/// WebSocket connection handler for the chat server.
pub async fn handle_ws(mut ws: WebSocketConnection, _resources: Arc<()>, bus: ranvier_core::Bus) {
    let room_manager = bus.get_cloned::<RoomManager>().unwrap();
    let token_store = bus.get_cloned::<TokenStore>().unwrap();

    // Extract token from query string: ?token=tok_xxx
    let token = ws.session().query().and_then(|q| {
        q.split('&').find_map(|pair| match pair.split_once('=') {
            Some(("token", value)) => Some(value.to_string()),
            _ => None,
        })
    });

    let claims = match token.and_then(|t| auth::verify_token(&token_store, &t)) {
        Some(c) => c,
        None => {
            let _ = ws
                .send_json(&ServerMessage::Error {
                    code: "auth_failed".to_string(),
                    detail: "Invalid or missing token. Connect with ?token=<your_token>"
                        .to_string(),
                })
                .await;
            return;
        }
    };

    let user_id = claims.user_id.clone();
    let username = claims.username.clone();

    // Register sender channel for this user
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerMessage>();
    room_manager.register_sender(&user_id, tx);

    // Send welcome message
    let _ = ws
        .send_json(&ServerMessage::Welcome {
            user: username.clone(),
        })
        .await;

    // Send room list
    let rooms = room_manager.list_rooms();
    let _ = ws.send_json(&ServerMessage::RoomList { rooms }).await;

    // Single event loop: handle both incoming WS messages and outgoing channel messages
    loop {
        tokio::select! {
            // Forward server messages (from other users via RoomManager) to this WebSocket
            Some(msg) = rx.recv() => {
                if ws.send_json(&msg).await.is_err() {
                    break;
                }
            }
            // Process incoming client messages from this WebSocket
            result = ws.next_json::<ClientMessage>() => {
                match result {
                    Ok(Some(msg)) => {
                        handle_client_message(&room_manager, &ws, &user_id, &username, msg).await;
                    }
                    Ok(None) => break, // Connection closed
                    Err(_) => break,
                }
            }
        }
    }

    // Cleanup
    room_manager.unregister_sender(&user_id);
    tracing::info!(user = %username, "WebSocket disconnected");
}

async fn handle_client_message(
    room_manager: &RoomManager,
    ws: &WebSocketConnection,
    user_id: &str,
    username: &str,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Join { room } => {
            match room_manager.join_room(&room, user_id, username) {
                Ok(count) => {
                    // Send history to the joining user
                    let history = room_manager.get_history(&room, 50);
                    let _ = ws
                        .send_json(&ServerMessage::History {
                            room: room.clone(),
                            messages: history,
                        })
                        .await;
                    let _ = ws
                        .send_json(&ServerMessage::Joined {
                            room,
                            user: username.to_string(),
                            count,
                        })
                        .await;
                }
                Err(detail) => {
                    let _ = ws
                        .send_json(&ServerMessage::Error {
                            code: "room_not_found".to_string(),
                            detail,
                        })
                        .await;
                }
            }
        }
        ClientMessage::Leave { room } => {
            let count = room_manager.leave_room(&room, user_id, username);
            let _ = ws
                .send_json(&ServerMessage::Left {
                    room,
                    user: username.to_string(),
                    count,
                })
                .await;
        }
        ClientMessage::Chat { room, message } => {
            if let Err(detail) = room_manager.broadcast_message(&room, user_id, username, &message)
            {
                let _ = ws
                    .send_json(&ServerMessage::Error {
                        code: "send_failed".to_string(),
                        detail,
                    })
                    .await;
            }
        }
        ClientMessage::Typing { room: _ } => {
            // Typing indicators are fire-and-forget; no response needed
        }
    }
}

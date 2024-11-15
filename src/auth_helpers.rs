// Handle a new WebSocket connection
use crate::{
    responses::JWTError,
    shared::{Channel, Channels},
};
use futures::StreamExt;
use ginger_shared_rs::rocket_utils::Claims;
use jsonwebtoken::{decode, DecodingKey, Validation};
use tokio::sync::{broadcast, Mutex};
use warp::{
    reject::Rejection,
    ws::{Message, WebSocket},
};

use futures::sink::SinkExt;

pub async fn user_connected(ws: WebSocket, channel_name: String, channels: Channels) {
    let (mut tx, mut rx) = ws.split();

    let (channel_tx, mut channel_rx) = {
        let mut channels_lock = channels.lock().await;
        let channel = channels_lock
            .entry(channel_name.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100);
                Channel {
                    name: channel_name.clone(),
                    tx,
                }
            });

        (channel.tx.clone(), channel.tx.subscribe())
    };

    tokio::spawn(async move {
        while let Some(result) = rx.next().await {
            if let Ok(msg) = result {
                if let Ok(text) = msg.to_str() {
                    let _ = channel_tx.send(text.to_string());
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Ok(message) = channel_rx.recv().await {
            if tx.send(Message::text(message)).await.is_err() {
                break;
            }
        }
    });
}

pub async fn handle_ws_upgrade(
    (ws, channel_name, channels): (warp::ws::Ws, String, Channels),
) -> Result<impl warp::Reply, Rejection> {
    Ok(ws.on_upgrade(move |socket| user_connected(socket, channel_name, channels)))
}
pub async fn user_authenticated(
    channel_name: String,
    ws: warp::ws::Ws,
    channels: Channels,
    token: Option<String>, // Extract token from query parameters
) -> Result<(warp::ws::Ws, String, Channels), Rejection> {
    if let Some(token) = token {
        // No need to trim "Bearer " since the token is expected to be plain
        let secret = "1234";

        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        match decode::<Claims>(&token, &decoding_key, &validation) {
            Ok(token_data) => {
                println!("Authenticated user: {:?}", token_data.claims.user_id);
                Ok((ws, channel_name, channels))
            }
            Err(e) => {
                println!("Unauthorized access attempt : {}", e);
                Err(warp::reject::custom(JWTError))
            }
        }
    } else {
        println!("Token query parameter missing");
        Err(warp::reject::custom(JWTError))
    }
}

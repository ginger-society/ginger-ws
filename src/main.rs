use futures::sink::SinkExt;
use futures::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use warp::ws::{Message, WebSocket};
use warp::Filter; // Add this to bring the `send` method into scope

#[derive(Debug, Clone)]
struct Channel {
    name: String,
    tx: broadcast::Sender<String>,
}

type Channels = Arc<Mutex<HashMap<String, Channel>>>;

#[derive(Deserialize)]
struct PublishRequest {
    message: String,
}

#[tokio::main]
async fn main() {
    let channels: Channels = Arc::new(Mutex::new(HashMap::new()));

    // WebSocket endpoint to subscribe to channels
    let channels_ws = channels.clone();
    let websocket_route = warp::path("ws")
        .and(warp::path::param::<String>())
        .and(warp::ws())
        .and(with_channels(channels_ws))
        .map(|channel_name: String, ws: warp::ws::Ws, channels| {
            ws.on_upgrade(move |socket| user_connected(socket, channel_name, channels))
        });

    // REST API to publish messages to a channel
    let channels_rest = channels.clone();
    let publish_route = warp::path!("channels" / String / "publish")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_channels(channels_rest))
        .and_then(publish_message);

    let routes = websocket_route.or(publish_route);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

// Filter to inject channels into the route handlers
fn with_channels(
    channels: Channels,
) -> impl Filter<Extract = (Channels,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || channels.clone())
}

// Handle a new WebSocket connection
async fn user_connected(ws: WebSocket, channel_name: String, channels: Channels) {
    let (mut tx, mut rx) = ws.split();

    let (channel_tx, mut channel_rx) = {
        let mut channels_lock = channels.lock().unwrap();
        let channel = channels_lock
            .entry(channel_name.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100); // Create a new broadcast channel
                Channel {
                    name: channel_name.clone(),
                    tx, // Insert the sender part of the channel into the struct
                }
            });

        // Clone the channel's tx (sender) and create a new receiver for the WebSocket
        (channel.tx.clone(), channel.tx.subscribe())
    };

    // Spawn task to send incoming WebSocket messages to the channel
    tokio::spawn(async move {
        while let Some(result) = rx.next().await {
            match result {
                Ok(msg) => {
                    if let Ok(text) = msg.to_str() {
                        // Send the message to all subscribers in the channel
                        if let Err(_) = channel_tx.send(text.to_string()) {
                            eprintln!("Error sending message to channel");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("WebSocket error: {}", e);
                    break;
                }
            }
        }
    });

    // Spawn task to forward messages from the channel to the WebSocket client
    tokio::spawn(async move {
        while let Ok(message) = channel_rx.recv().await {
            if tx.send(Message::text(message)).await.is_err() {
                eprintln!("WebSocket send error");
                break;
            }
        }
    });
}

// Publish a message to a channel using the REST endpoint
async fn publish_message(
    channel_name: String,
    publish_request: PublishRequest,
    channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut channels_lock = channels.lock().unwrap();
    if let Some(channel) = channels_lock.get(&channel_name) {
        let _ = channel.tx.send(publish_request.message);
        Ok(warp::reply::json(&"Message sent"))
    } else {
        Ok(warp::reply::json(&"Channel not found"))
    }
}

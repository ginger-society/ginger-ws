use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use lapin::{Channel as RabbitChannel, Connection as LapinConnection, ConnectionProperties};
use uuid::Uuid;
use warp::Filter;
use lapin::options::BasicPublishOptions;
use lapin::BasicProperties;

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub tx: broadcast::Sender<String>,
}

/// Per-websocket-connection state for disconnect tracking
#[derive(Debug, Clone)]
pub struct WsConnection {          // ← renamed from Connection
    pub id: Uuid,
    pub tx: broadcast::Sender<String>,
}

#[derive(Debug, Clone)]
pub struct PendingCall {
    pub correlation_id: String,
    pub reply_to: String,
    pub callee_channel: String,        // ← channel the call was sent to
    pub caller_connection_id: Uuid,    // ← who sent the call (to avoid self-notification)
}

pub type Channels      = Arc<Mutex<HashMap<String, Channel>>>;
pub type Connections   = Arc<Mutex<HashMap<Uuid, WsConnection>>>; 
pub type PendingCalls  = Arc<Mutex<HashMap<String, PendingCall>>>;

pub fn with_channels(
    channels: Channels,
) -> impl Filter<Extract = (Channels,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || channels.clone())
}

pub fn with_connections(
    connections: Connections,
) -> impl Filter<Extract = (Connections,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || connections.clone())
}

pub fn with_pending_calls(
    pending_calls: PendingCalls,
) -> impl Filter<Extract = (PendingCalls,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || pending_calls.clone())
}

pub async fn connect_rabbitmq() -> Result<RabbitChannel, lapin::Error> {
    let addr = std::env::var("AMPQ_URI")
        .unwrap_or_else(|_| "amqp://user:password@localhost:5672/%2f".to_string());

    let conn = LapinConnection::connect(&addr, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    channel
        .exchange_declare(
            "real-time-updates",
            lapin::ExchangeKind::Fanout,
            Default::default(),
            Default::default(),
        )
        .await?;

    channel
        .queue_declare(
            "real-time-updates-queue",
            Default::default(),
            Default::default(),
        )
        .await?;

    channel
        .queue_bind(
            "real-time-updates-queue",
            "real-time-updates",
            "",
            Default::default(),
            Default::default(),
        )
        .await?;

    Ok(channel)
}



pub async fn publish_to_rabbitmq(channel_id: &str, message: &str) {
    let rabbit_message = serde_json::json!({
        "channel_id": channel_id,
        "message": message,
    });

    match connect_rabbitmq().await {
        Ok(rabbit_channel) => {
            let _ = rabbit_channel
                .basic_publish(
                    "real-time-updates",
                    "",
                    BasicPublishOptions::default(),
                    rabbit_message.to_string().as_bytes(),
                    BasicProperties::default(),
                )
                .await;
        }
        Err(e) => {
            eprintln!("[rabbitmq] failed to publish to '{}': {:?}", channel_id, e);
        }
    }
}
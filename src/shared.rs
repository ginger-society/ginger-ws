use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use lapin::{Channel as RabbitChannel, Connection as LapinConnection, ConnectionProperties};
use uuid::Uuid;
use warp::Filter;

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
    pub callee_connection_id: Uuid,
}

pub type Channels      = Arc<Mutex<HashMap<String, Channel>>>;
pub type Connections   = Arc<Mutex<HashMap<Uuid, WsConnection>>>;  // ← WsConnection
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
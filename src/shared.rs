use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, Mutex};

use lapin::{Channel as RabbitChannel, Connection, ConnectionProperties};
use warp::Filter;

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub tx: broadcast::Sender<String>,
}

// Filter to inject channels into the route handlers
pub fn with_channels(
    channels: Channels,
) -> impl Filter<Extract = (Channels,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || channels.clone())
}

pub type Channels = Arc<Mutex<HashMap<String, Channel>>>;

pub async fn connect_rabbitmq() -> Result<RabbitChannel, lapin::Error> {
    let addr = std::env::var("AMPQ_URI")
        .unwrap_or_else(|_| "amqp://user:password@localhost:5672/%2f".to_string());

    let conn = Connection::connect(&addr, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    // Declare an exchange if needed
    channel
        .exchange_declare(
            "real-time-updates",
            lapin::ExchangeKind::Fanout,
            Default::default(),
            Default::default(),
        )
        .await?;

    // Declare a queue for consuming messages
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

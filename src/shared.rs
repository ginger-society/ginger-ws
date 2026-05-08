use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use lapin::{
    Channel as RabbitChannel, Connection as LapinConnection, ConnectionProperties,
    options::{
        BasicPublishOptions, ExchangeDeclareOptions,
        QueueDeclareOptions, QueueBindOptions,
    },
    BasicProperties, ExchangeKind,
};
use uuid::Uuid;
use warp::Filter;
use redis::AsyncCommands;
use futures::StreamExt;

// ── types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub tx: broadcast::Sender<String>,
}

#[derive(Debug, Clone)]
pub struct WsConnection {
    pub id: Uuid,
    pub tx: broadcast::Sender<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingCall {
    pub correlation_id: String,
    pub reply_to: String,
    pub callee_channel: String,
    pub caller_connection_id: String,
}

pub type Channels    = Arc<Mutex<HashMap<String, Channel>>>;
pub type Connections = Arc<Mutex<HashMap<Uuid, WsConnection>>>;
pub type RedisPool   = Arc<redis::aio::ConnectionManager>;

pub const PENDING_CALL_TTL_SECS: u64 = 20;
const PENDING_KEY: &str = "pending_call:";

// ── warp filters ──────────────────────────────────────────────────────────────

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

pub fn with_redis(
    redis: RedisPool,
) -> impl Filter<Extract = (RedisPool,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || redis.clone())
}

pub fn with_rabbit(
    pool: RabbitPoolRef,
) -> impl Filter<Extract = (RabbitPoolRef,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || pool.clone())
}

// ── RabbitMQ pool ─────────────────────────────────────────────────────────────

pub struct RabbitPool {
    pub channel: Arc<Mutex<RabbitChannel>>,
}

pub type RabbitPoolRef = Arc<RabbitPool>;

impl RabbitPool {
    pub async fn new() -> Self {
        loop {
            match connect_rabbitmq_publisher().await {
                Ok(channel) => {
                    println!("[rabbitmq] persistent publish pool established");
                    return Self {
                        channel: Arc::new(Mutex::new(channel)),
                    };
                }
                Err(e) => {
                    eprintln!("[rabbitmq] pool init failed: {:?} — retrying in 5s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }
}

// ── RabbitMQ connections ──────────────────────────────────────────────────────

/// Publisher connection — plain channel, no queue needed
pub async fn connect_rabbitmq_publisher() -> Result<RabbitChannel, lapin::Error> {
    let addr = std::env::var("AMPQ_URI")
        .unwrap_or_else(|_| "amqp://user:password@localhost:5672/%2f".to_string());

    let conn = LapinConnection::connect(&addr, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    // just declare the exchange — no queue needed for publishing
    channel
        .exchange_declare(
            "real-time-updates",
            ExchangeKind::Fanout,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            Default::default(),
        )
        .await?;

    Ok(channel)
}

/// Consumer connection — exclusive queue per broker instance
/// Every instance gets its own queue → fanout delivers to ALL instances
pub async fn connect_rabbitmq_consumer() -> Result<(RabbitChannel, String), lapin::Error> {
    let addr = std::env::var("AMPQ_URI")
        .unwrap_or_else(|_| "amqp://user:password@localhost:5672/%2f".to_string());

    let conn = LapinConnection::connect(&addr, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    // declare the fanout exchange
    channel
        .exchange_declare(
            "real-time-updates",
            ExchangeKind::Fanout,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            Default::default(),
        )
        .await?;

    // exclusive auto-named queue — unique per broker instance
    // auto_delete: true  → deleted when this connection closes
    // exclusive: true    → only this connection can consume from it
    let queue = channel
        .queue_declare(
            "",
            QueueDeclareOptions {
                exclusive: true,
                auto_delete: true,
                ..Default::default()
            },
            Default::default(),
        )
        .await?;

    let queue_name = queue.name().to_string();
    println!("[rabbitmq] exclusive queue declared: {}", queue_name);

    // bind this instance's queue to the fanout exchange
    channel
        .queue_bind(
            &queue_name,
            "real-time-updates",
            "",
            QueueBindOptions::default(),
            Default::default(),
        )
        .await?;

    println!("[rabbitmq] queue {} bound to fanout exchange", queue_name);

    Ok((channel, queue_name))
}

// ── RabbitMQ publish ──────────────────────────────────────────────────────────

pub async fn publish_to_rabbitmq(pool: &RabbitPoolRef, channel_id: &str, message: &str) {
    let rabbit_message = serde_json::json!({
        "channel_id": channel_id,
        "message": message,
    });

    let ch = pool.channel.lock().await;
    if let Err(e) = ch
        .basic_publish(
            "real-time-updates",
            "",
            BasicPublishOptions::default(),
            rabbit_message.to_string().as_bytes(),
            BasicProperties::default(),
        )
        .await
    {
        eprintln!("[rabbitmq] publish failed: {:?}", e);
    }
}

// ── Redis connections ─────────────────────────────────────────────────────────

pub async fn connect_redis() -> RedisPool {
    let url = std::env::var("REDIS_URI")
        .unwrap_or_else(|_| "redis://localhost:6380".to_string());
    let client = redis::Client::open(url).expect("Invalid Redis URL");
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    Arc::new(manager)
}

pub async fn connect_redis_pubsub_pool() -> RedisPool {
    let url = std::env::var("REDIS_PUBSUB_URI")
        .unwrap_or_else(|_| "redis://localhost:6381".to_string());
    let client = redis::Client::open(url).expect("Invalid Redis pubsub URL");
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis pubsub");
    Arc::new(manager)
}

// ── Redis pub/sub publish ─────────────────────────────────────────────────────

pub async fn redis_publish(redis: &RedisPool, channel: &str, message: &str) {
    let mut conn = (**redis).clone();
    let _: Result<(), _> = conn.publish(channel, message).await;
}

// ── Redis pub/sub bridge ──────────────────────────────────────────────────────
// Subscribes to ALL channels via pattern "*"
// When a message arrives, delivers it to local broadcast::Sender
// This is how WebSocket-originated publishes reach all broker instances

pub async fn start_redis_pubsub_bridge(channels: Channels) {
    let url = std::env::var("REDIS_PUBSUB_URI")
        .unwrap_or_else(|_| "redis://localhost:6381".to_string());

    tokio::spawn(async move {
        loop {
            let client = match redis::Client::open(url.clone()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[redis-pubsub] client error: {:?} — retrying in 2s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            let mut pubsub = match client.get_async_pubsub().await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[redis-pubsub] connect failed: {:?} — retrying in 2s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            // psubscribe to all channels
            if let Err(e) = pubsub.psubscribe("*").await {
                eprintln!("[redis-pubsub] psubscribe failed: {:?} — retrying in 2s", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }

            println!("[redis-pubsub] bridge active — subscribed to pattern *");

            let mut stream = pubsub.into_on_message();

            while let Some(msg) = stream.next().await {
                let channel_name: String = msg.get_channel_name().to_string();
                let payload: String = match msg.get_payload() {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let channels_lock = channels.lock().await;
                if let Some(ch) = channels_lock.get(&channel_name) {
                    if ch.tx.receiver_count() > 0 {
                        let _ = ch.tx.send(payload);
                    }
                }
                // no local subscribers — another instance will handle it
            }

            eprintln!("[redis-pubsub] stream ended — reconnecting in 2s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    });
}

// ── Redis pending call helpers ────────────────────────────────────────────────

pub async fn pending_call_insert(redis: &RedisPool, pc: &PendingCall) {
    let key = format!("{}{}", PENDING_KEY, pc.correlation_id);
    let value = serde_json::to_string(pc).unwrap();
    let mut conn = (**redis).clone();
    let _: Result<(), _> = conn
        .set_ex(&key, value, PENDING_CALL_TTL_SECS.try_into().unwrap())
        .await;
    println!("[redis] pending call inserted corr={}", pc.correlation_id);
}

pub async fn pending_call_remove(redis: &RedisPool, correlation_id: &str) -> Option<PendingCall> {
    let key = format!("{}{}", PENDING_KEY, correlation_id);
    let mut conn = (**redis).clone();
    let value: Option<String> = conn.get_del(&key).await.ok().flatten();
    value.and_then(|v| serde_json::from_str(&v).ok())
}

pub async fn pending_calls_for_channel(
    redis: &RedisPool,
    callee_channel: &str,
    exclude_caller_id: &str,
) -> Vec<PendingCall> {
    let mut conn = (**redis).clone();
    let pattern = format!("{}*", PENDING_KEY);

    let keys: Vec<String> = match conn.keys(&pattern).await {
        Ok(k) => k,
        Err(e) => {
            eprintln!("[redis] KEYS scan failed: {:?}", e);
            return vec![];
        }
    };

    let mut results = vec![];
    for key in keys {
        let value: Option<String> = conn.get(&key).await.ok().flatten();
        if let Some(v) = value {
            if let Ok(pc) = serde_json::from_str::<PendingCall>(&v) {
                if pc.callee_channel == callee_channel
                    && pc.caller_connection_id != exclude_caller_id
                {
                    results.push(pc);
                }
            }
        }
    }
    results
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
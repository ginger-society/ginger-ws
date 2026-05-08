use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use lapin::{Channel as RabbitChannel, Connection as LapinConnection, ConnectionProperties};
use lapin::options::BasicPublishOptions;
use lapin::BasicProperties;
use uuid::Uuid;
use warp::Filter;
use redis::AsyncCommands;

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

// PendingCall is still a struct — just stored in Redis now instead of HashMap
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingCall {
    pub correlation_id: String,
    pub reply_to: String,
    pub callee_channel: String,
    pub caller_connection_id: String,  // String instead of Uuid for easy Redis serialisation
}

pub type Channels    = Arc<Mutex<HashMap<String, Channel>>>;
pub type Connections = Arc<Mutex<HashMap<Uuid, WsConnection>>>;
pub type RedisPool   = Arc<redis::aio::ConnectionManager>;

// TTL for pending calls in Redis — 5 minutes
pub const PENDING_CALL_TTL_SECS: u64 = 300;
// Redis key prefix
const PENDING_KEY: &str = "pending_call:";

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

// ── RabbitMQ ──────────────────────────────────────────────────────────────────

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

// ── Redis connection ──────────────────────────────────────────────────────────

pub async fn connect_redis() -> RedisPool {
    let url = std::env::var("REDIS_URI")
        .unwrap_or_else(|_| "redis://localhost:6380".to_string());

    let client = redis::Client::open(url).expect("Invalid Redis URL");
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    Arc::new(manager)
}
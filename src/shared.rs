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
    pub caller_channel: String,
}

pub type Channels    = Arc<Mutex<HashMap<String, Channel>>>;
pub type Connections = Arc<Mutex<HashMap<Uuid, WsConnection>>>;
pub type RedisPool   = Arc<redis::aio::ConnectionManager>;

pub const PENDING_CALL_TTL_SECS: u64 = 20;
const PENDING_KEY: &str = "pending_call:";
const PENDING_INDEX_KEY: &str = "pending_call_index:";
pub const BROKER_HEARTBEAT_KEY: &str = "broker:heartbeat:";
pub const BROKER_SET_KEY: &str = "brokers:active";
pub const MISS_COUNT_KEY: &str = "callee_miss:";
pub const MISS_COUNT_TTL_SECS: u64 = 3;
pub const BROKER_HEARTBEAT_TTL_SECS: u64 = 15;
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
    let broker_id = std::env::var("BROKER_ID")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    let queue_name = format!("broker_{}", broker_id);

    let queue = channel
        .queue_declare(
            &queue_name,   // ← stable name, survives reconnect
            QueueDeclareOptions {
                durable: true,       // survives RabbitMQ restart
                auto_delete: false,  // not deleted on disconnect
                exclusive: false,    // other connections can reconnect to it
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
    
    // enable keyspace notifications for expired events
    let mut conn = manager.clone();
    let _: Result<(), _> = redis::cmd("CONFIG")
        .arg("SET")
        .arg("notify-keyspace-events")
        .arg("Ex")  // E = keyspace events, x = expired events
        .query_async(&mut conn)
        .await;
    
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

    // main pending call key
    let _: Result<(), _> = conn
        .set_ex(&key, value, PENDING_CALL_TTL_SECS.try_into().unwrap())
        .await;

    // secondary index — Set of correlation_ids per callee channel
    let index_key = format!("{}{}", PENDING_INDEX_KEY, pc.callee_channel);
    let _: Result<(), _> = conn
        .sadd(&index_key, &pc.correlation_id)
        .await;
    // expire the index key in line with the TTL — refreshed on each insert
    let _: Result<(), _> = conn
        .expire(&index_key, PENDING_CALL_TTL_SECS.try_into().unwrap())
        .await;

    // shadow key for expiry watcher
    let shadow_key = format!("pending_call_caller:{}", pc.correlation_id);
    let shadow_value = serde_json::json!({
        "caller_channel": pc.caller_channel,
        "correlation_id": pc.correlation_id,
    }).to_string();
    let _: Result<(), _> = conn
        .set_ex(
            &shadow_key,
            shadow_value,
            (PENDING_CALL_TTL_SECS + 5).try_into().unwrap(),
        )
        .await;

    println!("[redis] pending call inserted corr={}", pc.correlation_id);
}

pub async fn pending_call_remove(redis: &RedisPool, correlation_id: &str) -> Option<PendingCall> {
    let key = format!("{}{}", PENDING_KEY, correlation_id);
    let mut conn = (**redis).clone();

    let value: Option<String> = conn.get_del(&key).await.ok().flatten();
    let pc = value.and_then(|v| serde_json::from_str::<PendingCall>(&v).ok());

    // remove from secondary index
    if let Some(ref pc) = pc {
        let index_key = format!("{}{}", PENDING_INDEX_KEY, pc.callee_channel);
        let _: Result<(), _> = conn.srem(&index_key, &pc.correlation_id).await;
    }

    pc
}

pub async fn pending_calls_for_channel(
    redis: &RedisPool,
    callee_channel: &str,
    exclude_caller_id: &str,
) -> Vec<PendingCall> {
    let mut conn = (**redis).clone();

    // read the index — O(1) instead of KEYS *
    let index_key = format!("{}{}", PENDING_INDEX_KEY, callee_channel);
    let correlation_ids: Vec<String> = match conn.smembers(&index_key).await {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("[redis] index read failed: {:?}", e);
            return vec![];
        }
    };

    let mut results = vec![];
    for corr_id in correlation_ids {
        let key = format!("{}{}", PENDING_KEY, corr_id);
        let value: Option<String> = conn.get(&key).await.ok().flatten();

        if let Some(v) = value {
            if let Ok(pc) = serde_json::from_str::<PendingCall>(&v) {
                if pc.caller_connection_id != exclude_caller_id {
                    results.push(pc);
                }
            }
        } else {
            // main key expired but index not cleaned up yet — remove stale entry
            let _: Result<(), _> = conn.srem(&index_key, &corr_id).await;
        }
    }

    results
}

pub async fn start_pending_call_expiry_watcher(
    rabbit_pool: RabbitPoolRef,
    redis_pool: RedisPool,
) {
    let url = std::env::var("REDIS_URI")
        .unwrap_or_else(|_| "redis://localhost:6380".to_string());

    tokio::spawn(async move {
        loop {
            let client = match redis::Client::open(url.clone()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[redis-expiry] client error: {:?} — retrying in 2s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            let mut pubsub = match client.get_async_pubsub().await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[redis-expiry] connect failed: {:?} — retrying in 2s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            if let Err(e) = pubsub.psubscribe("__keyevent@0__:expired").await {
                eprintln!("[redis-expiry] psubscribe failed: {:?} — retrying in 2s", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }

            println!("[redis-expiry] watcher active");

            let mut stream = pubsub.into_on_message();

            while let Some(msg) = stream.next().await {
                let expired_key: String = match msg.get_payload() {
                    Ok(k) => k,
                    Err(_) => continue,
                };

                if !expired_key.starts_with(PENDING_KEY) {
                    continue;
                }

                let correlation_id = expired_key
                    .trim_start_matches(PENDING_KEY)
                    .to_string();

                println!("[redis-expiry] pending call expired corr={}", correlation_id);

                // read shadow key — still alive for 5 more seconds
                let shadow_key = format!("pending_call_caller:{}", correlation_id);
                let mut conn = (*redis_pool).clone();
                let shadow: Option<String> = conn.get(&shadow_key).await.unwrap_or(None);

                if let Some(shadow_str) = shadow {
                    if let Ok(shadow_val) = serde_json::from_str::<serde_json::Value>(&shadow_str) {
                        if let Some(caller_channel) = shadow_val["caller_channel"].as_str() {
                            
                            // only one broker should fire the timeout
                            let lock_key = format!("timeout_fired:{}", correlation_id);
                            let acquired: bool = redis::cmd("SET")
                                .arg(&lock_key)
                                .arg("1")
                                .arg("NX")
                                .arg("EX")
                                .arg(10u64)
                                .query_async(&mut conn)
                                .await
                                .unwrap_or(false);

                            if !acquired {
                                println!(
                                    "[redis-expiry] timeout already fired by another broker corr={} — skipping",
                                    correlation_id
                                );
                                continue;
                            }

                            let timeout = serde_json::json!({
                                "message_type": 0,
                                "error": "callee_timeout",
                                "correlation_id": correlation_id,
                            });

                            publish_to_rabbitmq(
                                &rabbit_pool,
                                caller_channel,
                                &timeout.to_string(),
                            ).await;

                            println!(
                                "[redis-expiry] callee_timeout sent to '{}' corr={}",
                                caller_channel, correlation_id
                            );

                            // clean up shadow key
                            let _: Result<(), _> = conn.del(&shadow_key).await;
                        }
                    }
                } else {
                    println!(
                        "[redis-expiry] shadow key not found for corr={} — caller may have already been notified",
                        correlation_id
                    );
                }
            }

            eprintln!("[redis-expiry] stream ended — reconnecting in 2s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    });
}



// ── broker registration ───────────────────────────────────────────────────────
// Call once at startup, then refresh every ~5 s from a background task.
 
pub async fn register_broker(redis: &RedisPool, broker_id: &str) {
    let mut conn = (**redis).clone();
    let key = format!("{}{}", BROKER_HEARTBEAT_KEY, broker_id);
    // heartbeat key — if this broker dies the key expires and it falls out
    let _: Result<(), _> = conn
        .set_ex(&key, "1", BROKER_HEARTBEAT_TTL_SECS.try_into().unwrap())
        .await;
    // add to set of known brokers
    let _: Result<(), _> = conn.sadd(BROKER_SET_KEY, broker_id).await;
}
 
pub async fn unregister_broker(redis: &RedisPool, broker_id: &str) {
    let mut conn = (**redis).clone();
    let key = format!("{}{}", BROKER_HEARTBEAT_KEY, broker_id);
    let _: Result<(), _> = conn.del(&key).await;
    let _: Result<(), _> = conn.srem(BROKER_SET_KEY, broker_id).await;
}
 
/// Returns the count of currently alive brokers.
/// A broker is "alive" if its heartbeat key still exists in Redis.
pub async fn live_broker_count(redis: &RedisPool) -> u64 {
    let mut conn = (**redis).clone();
 
    // Read all broker IDs from the set
    let ids: Vec<String> = match conn.smembers::<_, Vec<String>>(BROKER_SET_KEY).await {
        Ok(v) => v,
        Err(_) => return 1, // safe fallback — don't fire false positive
    };
 
    let mut alive = 0u64;
    for id in ids {
        let hb_key = format!("{}{}", BROKER_HEARTBEAT_KEY, id);
        let exists: bool = conn.exists(&hb_key).await.unwrap_or(false);
        if exists {
            alive += 1;
        } else {
            // heartbeat expired — clean up the set
            let _: Result<(), _> = conn.srem(BROKER_SET_KEY, &id).await;
        }
    }
 
    alive.max(1) // never return 0 — prevents divide-by-zero / false positives
}
 
/// Atomically increment the miss counter for a correlation_id.
/// Returns the new count.  The key is set with MISS_COUNT_TTL_SECS TTL on
/// first increment so stale counters self-clean if a broker crashes mid-flow.
pub async fn increment_miss_count(redis: &RedisPool, correlation_id: &str) -> u64 {
    let key = format!("{}{}", MISS_COUNT_KEY, correlation_id);
    let mut conn = (**redis).clone();
 
    // INCR is atomic in Redis
    let count: u64 = conn.incr(&key, 1u64).await.unwrap_or(1);
 
    // Set TTL only on the first increment (count == 1); subsequent INCR calls
    // on an existing key don't reset the TTL — that's intentional.
    if count == 1 {
        let _: Result<(), _> = conn
            .expire(&key, MISS_COUNT_TTL_SECS.try_into().unwrap())
            .await;
    }
 
    count
}
 
/// Clean up the miss counter once we've decided to fire callee_offline.
pub async fn clear_miss_count(redis: &RedisPool, correlation_id: &str) {
    let key = format!("{}{}", MISS_COUNT_KEY, correlation_id);
    let mut conn = (**redis).clone();
    let _: Result<(), _> = conn.del(&key).await;
}
 
// ── broker heartbeat task ─────────────────────────────────────────────────────
// Spawn once at startup.  Keeps the heartbeat key alive so other brokers can
// count us as a peer.
 
pub async fn start_broker_heartbeat(redis: RedisPool, broker_id: String) {
    tokio::spawn(async move {
        loop {
            register_broker(&redis, &broker_id).await;
            // refresh every 5 s — well within the 15 s TTL
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });
}
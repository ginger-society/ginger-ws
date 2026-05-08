// src/miss_consumer.rs
//
// Consumes "callee miss" notices from the `__callee_miss__` virtual channel
// (delivered via the existing RabbitMQ fanout → local broadcast pipeline).
//
// Algorithm
// ---------
// 1. Every broker that finds receiver_count == 0 publishes a CalleeMissNotice
//    to the `__callee_miss__` channel.
// 2. Because the exchange is a *fanout*, every broker instance receives every
//    notice (including the one it published itself).  Only ONE instance should
//    act on it — we achieve this via the Redis atomic counter:
//
//      miss_count = INCR callee_miss:<correlation_id>
//      (key TTL = 3 s, set on first INCR)
//
//    The instance that increments the counter to `live_broker_count` wins and
//    sends callee_offline.  All others just increment and move on.
//
// 3. If correlation_id is None the message is not RPC-style; we treat it as
//    a plain broadcast miss and skip the counting logic entirely (no caller to
//    notify).
//
// NOTE: `__callee_miss__` is a *reserved* channel name.  No real client should
// subscribe to or publish on it.  It is purely internal broker traffic.

use crate::{
    auth_helpers::CalleeMissNotice,
    requests::WampCalleeOffline,
    shared::{
        clear_miss_count, increment_miss_count, live_broker_count, Channels, RabbitPoolRef,
        RedisPool, publish_to_rabbitmq,
    },
};

/// Spawn the miss-consumer loop.  Call once from main, after the channels map
/// and Redis/Rabbit pools are initialised.
pub async fn start_miss_consumer(
    channels: Channels,
    redis: RedisPool,
    rabbit_pool: RabbitPoolRef,
) {
    // We piggyback on the existing broadcast::Sender that the RabbitMQ fanout
    // consumer already delivers messages into.  We subscribe to the reserved
    // `__callee_miss__` channel's sender here.

    // First make sure the channel entry exists so we can get a Receiver.
    let rx = {
        let mut lock = channels.lock().await;
        let ch = lock
            .entry("__callee_miss__".to_string())
            .or_insert_with(|| crate::shared::Channel {
                name: "__callee_miss__".to_string(),
                tx: tokio::sync::broadcast::channel(256).0,
            });
        ch.tx.subscribe()
    };

    tokio::spawn(async move {
        handle_miss_notices(rx, redis, rabbit_pool).await;
    });
}

async fn handle_miss_notices(
    mut rx: tokio::sync::broadcast::Receiver<String>,
    redis: RedisPool,
    rabbit_pool: RabbitPoolRef,
) {
    loop {
        match rx.recv().await {
            Ok(payload) => {
                // The RabbitMQ consumer wraps the inner message in a RabbitMessage
                // envelope: { "channel_id": "...", "message": "<inner json>" }
                // Extract the inner message.
                let inner = if let Ok(envelope) =
                    serde_json::from_str::<serde_json::Value>(&payload)
                {
                    envelope["message"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or(payload.clone())
                } else {
                    payload.clone()
                };

                let notice = match serde_json::from_str::<CalleeMissNotice>(&inner) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("[miss-consumer] deserialize error: {:?} payload={}", e, inner);
                        continue;
                    }
                };

                process_miss_notice(notice, &redis, &rabbit_pool).await;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("[miss-consumer] lagged by {} messages — continuing", n);
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                eprintln!("[miss-consumer] channel closed — exiting");
                break;
            }
        }
    }
}

async fn process_miss_notice(
    notice: CalleeMissNotice,
    redis: &RedisPool,
    rabbit_pool: &RabbitPoolRef,
) {
    // Plain broadcast with no RPC context — nothing to report back.
    let corr_id = match &notice.correlation_id {
        Some(id) => id.clone(),
        None => {
            println!(
                "[miss-consumer] ignoring miss for '{}' — no correlation_id",
                notice.topic
            );
            return;
        }
    };

    let reply_to = match &notice.reply_to {
        Some(r) => r.clone(),
        None => {
            println!(
                "[miss-consumer] ignoring miss for corr={} — no reply_to",
                corr_id
            );
            return;
        }
    };

    // How many brokers are alive right now?
    let broker_count = live_broker_count(redis).await;

    // Atomically record this broker's miss.
    let miss_count = increment_miss_count(redis, &corr_id).await;

    println!(
        "[miss-consumer] miss {}/{} for corr={} topic='{}' broker={}",
        miss_count, broker_count, corr_id, notice.topic, notice.broker_id
    );

    if miss_count >= broker_count {
        // Every live broker reported a miss — callee is truly offline.
        clear_miss_count(redis, &corr_id).await;

        let offline = WampCalleeOffline {
            message_type: 0,
            error: "callee_offline".to_string(),
            correlation_id: Some(corr_id.clone()),
            topic: notice.topic.clone(),
        };

        publish_to_rabbitmq(
            rabbit_pool,
            &reply_to,
            &serde_json::to_string(&offline).unwrap(),
        )
        .await;

        println!(
            "[miss-consumer] callee_offline confirmed — notified '{}' corr={}",
            reply_to, corr_id
        );
    }
    // else: still waiting for other brokers to report — do nothing.
}
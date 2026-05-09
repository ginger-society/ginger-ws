use futures::StreamExt;
use lapin::Error as LapinError;
use lapin::options::{BasicAckOptions, BasicConsumeOptions};
use lapin::Channel as RabbitChannel;
use tokio::time::{sleep, Duration};

use crate::requests::RabbitMessage;
use crate::shared::{connect_rabbitmq_consumer, Channels, RabbitPoolRef};
use crate::auth_helpers::{CalleeMissNotice, publish_callee_miss};

pub async fn consume_messages(channels: Channels, broker_id: String, rabbit_pool: RabbitPoolRef) {
    loop {
        match connect_rabbitmq_consumer(broker_id.clone()).await {
            Ok((rabbit_channel, queue_name)) => {
                println!("[rabbitmq] consumer started on queue {}", queue_name);
                if let Err(e) = process_rabbitmq_messages(
                    rabbit_channel,
                    queue_name,
                    channels.clone(),
                    broker_id.clone(),
                    rabbit_pool.clone(),
                )
                .await
                {
                    eprintln!("[rabbitmq] consumer error: {:?}", e);
                }
            }
            Err(e) => {
                eprintln!("[rabbitmq] consumer connect failed: {:?}", e);
            }
        }
        eprintln!("[rabbitmq] reconnecting consumer in 5s...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

pub async fn process_rabbitmq_messages(
    rabbit_channel: lapin::Channel,
    queue_name: String,
    channels: Channels,
    broker_id: String,
    rabbit_pool: RabbitPoolRef,
) -> Result<(), lapin::Error> {
    let mut consumer = rabbit_channel
        .basic_consume(
            &queue_name,
            &format!("consumer_{}", uuid::Uuid::new_v4()),
            BasicConsumeOptions::default(),
            Default::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                let message = String::from_utf8_lossy(&delivery.data).to_string();
                println!("[rabbitmq] RAW received on queue: {}", message);

                if let Ok(rabbit_message) = serde_json::from_str::<RabbitMessage>(&message) {
                    // skip internal miss channel — handled by miss_consumer
                    if rabbit_message.channel_id == "__callee_miss__" {
                        let channels_lock = channels.lock().await;
                        if let Some(channel) = channels_lock.get(&rabbit_message.channel_id) {
                            if channel.tx.receiver_count() > 0 {
                                let _ = channel.tx.send(rabbit_message.message.clone());
                            }
                        }
                        delivery.ack(BasicAckOptions::default()).await?;
                        continue;
                    }

                    let channels_lock = channels.lock().await;
                    if let Some(channel) = channels_lock.get(&rabbit_message.channel_id) {
                        println!(
                            "[rabbitmq] found channel '{}' sender_id={:p} receiver_count={}",
                            rabbit_message.channel_id,
                            &channel.tx,
                            channel.tx.receiver_count()
                        );
                        if channel.tx.receiver_count() > 0 {
                            println!(
                                "[rabbitmq] delivering to '{}' receiver_count={} msg={}",
                                rabbit_message.channel_id,
                                channel.tx.receiver_count(),
                                rabbit_message.message
                            );
                            let _ = channel.tx.send(rabbit_message.message.clone());
                        } else {
                            // channel entry exists but no receivers — report miss
                            drop(channels_lock);
                            publish_miss_from_consumer(
                                &rabbit_message.channel_id,
                                &rabbit_message.message,
                                &broker_id,
                                &rabbit_pool,
                            ).await;
                        }
                    } else {
                        // no channel entry — report miss
                        drop(channels_lock);
                        publish_miss_from_consumer(
                            &rabbit_message.channel_id,
                            &rabbit_message.message,
                            &broker_id,
                            &rabbit_pool,
                        ).await;
                        println!(
                            "[rabbitmq] no local subscribers for '{}' — miss notice sent",
                            rabbit_message.channel_id
                        );
                    }
                    delivery.ack(BasicAckOptions::default()).await?;
                } else {
                    println!("[rabbitmq] failed to deserialize message");
                    delivery.ack(BasicAckOptions::default()).await?;
                }
            }
            Err(e) => {
                eprintln!("[rabbitmq] delivery error: {:?}", e);
                return Err(e);
            }
        }
    }
    Ok(())
}

async fn publish_miss_from_consumer(
    channel_id: &str,
    message: &str,
    broker_id: &str,
    rabbit_pool: &RabbitPoolRef,
) {
    // extract correlation_id and reply_to from the WampEvent message
    let (correlation_id, reply_to) = if let Ok(event) =
        serde_json::from_str::<serde_json::Value>(message)
    {
        (
            event["details"]["correlation_id"]
                .as_str()
                .map(|s| s.to_string()),
            event["details"]["reply_to"]
                .as_str()
                .map(|s| s.to_string()),
        )
    } else {
        (None, None)
    };

    let miss = CalleeMissNotice {
        correlation_id,
        topic: channel_id.to_string(),
        reply_to,
        broker_id: broker_id.to_string(),
    };

    publish_callee_miss(rabbit_pool, &miss).await;
}
use futures::StreamExt;
use lapin::Error as LapinError;
use lapin::options::{BasicAckOptions, BasicConsumeOptions};
use lapin::Channel as RabbitChannel;
use tokio::time::{sleep, Duration};

use crate::requests::RabbitMessage;
use crate::shared::{connect_rabbitmq_consumer, Channels};

pub async fn consume_messages(channels: Channels) {
    loop {
        match connect_rabbitmq_consumer().await {
            Ok((rabbit_channel, queue_name)) => {
                println!("[rabbitmq] consumer started on queue {}", queue_name);
                if let Err(e) = process_rabbitmq_messages(
                    rabbit_channel,
                    queue_name,
                    channels.clone(),
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
        sleep(Duration::from_secs(5)).await;
    }
}

pub async fn process_rabbitmq_messages(
    rabbit_channel: RabbitChannel,
    queue_name: String,
    channels: Channels,
) -> Result<(), LapinError> {
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

                if let Ok(rabbit_message) = serde_json::from_str::<RabbitMessage>(&message) {
                    let channels_lock = channels.lock().await;
                    if let Some(channel) = channels_lock.get(&rabbit_message.channel_id) {
                        println!("[rabbitmq] delivering to '{}' receiver_count={} msg={}", 
                            rabbit_message.channel_id, 
                            channel.tx.receiver_count(),
                            rabbit_message.message
                        );
                        if channel.tx.receiver_count() > 0 {
                            let _ = channel.tx.send(rabbit_message.message.clone());
                        }
                        delivery.ack(BasicAckOptions::default()).await?;
                    } else {
                        // no local subscribers — ack anyway, another instance
                        // will have delivered it via their own exclusive queue
                        delivery.ack(BasicAckOptions::default()).await?;
                        println!(
                            "[rabbitmq] no local subscribers for '{}' — acked and discarded",
                            rabbit_message.channel_id
                        );
                    }
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
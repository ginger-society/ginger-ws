use futures::StreamExt;
use lapin::Error as LapinError;
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions},
    Channel as RabbitChannel,
};
use tokio::time::{sleep, Duration};

use crate::requests::RabbitMessage;
use crate::shared::{connect_rabbitmq, Channels};

pub async fn consume_messages(channels: Channels) {
    loop {
        match connect_rabbitmq().await {
            Ok(rabbit_channel) => {
                if let Err(e) = process_rabbitmq_messages(rabbit_channel, channels.clone()).await {
                    eprintln!("Error processing messages: {:?}", e);
                }
            }
            Err(e) => {
                eprintln!("Error connecting to RabbitMQ: {:?}", e);
            }
        }

        // Wait before retrying
        eprintln!("Reconnecting to RabbitMQ in 5 seconds...");
        sleep(Duration::from_secs(5)).await;
    }
}

pub async fn process_rabbitmq_messages(
    rabbit_channel: RabbitChannel,
    channels: Channels,
) -> Result<(), LapinError> {
    let mut consumer = rabbit_channel
        .basic_consume(
            "real-time-updates-queue",
            "consumer_tag",
            BasicConsumeOptions::default(),
            Default::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                // Handle message processing and acknowledgment
                let message = String::from_utf8_lossy(&delivery.data).to_string();
                println!("Received message from RabbitMQ: {}", message);

                if let Ok(rabbit_message) = serde_json::from_str::<RabbitMessage>(&message) {
                    let channels_lock = channels.lock().await;
                    if let Some(channel) = channels_lock.get(&rabbit_message.channel_id) {
                        let _ = channel.tx.send(rabbit_message.message.clone());
                        delivery.ack(BasicAckOptions::default()).await?;
                    } else {
                        println!(
                            "Message for non-existent channel: {}",
                            rabbit_message.channel_id
                        );
                    }
                } else {
                    println!("Failed to deserialize message from RabbitMQ");
                }
            }
            Err(e) => {
                eprintln!("Error receiving message: {:?}", e);
                return Err(e); // Return error to trigger reconnection
            }
        }
    }

    Ok(())
}

use ginger_shared_rs::rocket_utils::{APIClaims, Claims};

use crate::{
    requests::{PublishRequest, RabbitMessage},
    shared::{connect_rabbitmq, Channels},
};
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions},
    BasicProperties, Channel as RabbitChannel, Connection, ConnectionProperties,
}; // Renaming lapin::Channel to RabbitChannel
use IAMService::{
    apis::default_api::{identity_get_group_members_ids, IdentityGetGroupMembersIdsParams},
    get_configuration,
};

#[utoipa::path(
    post,
    path = "/notification/channels/{channel_name}/publish",
    params(
        ("channel_name" = String, Path, description = "The name of the channel to publish to")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent"),
        (status = 404, description = "Channel not found")
    ),
    security(("bearerAuth" = []))  // Referencing the security scheme
)]
pub async fn publish_message(
    channel_name: String,
    publish_request: PublishRequest,
    claims: Claims, // Add claims from JWT here
    auth_header: String,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Ok(rabbit_channel) = connect_rabbitmq().await {
        // let payload = publish_request.message.clone().into_bytes();

        let rabbit_message = RabbitMessage {
            channel_id: channel_name.clone(), // channel_id from the path
            message: publish_request.message.clone(),
        };

        match rabbit_channel
            .basic_publish(
                "real-time-updates", // Exchange name
                "",                  // Routing key
                BasicPublishOptions::default(),
                &serde_json::to_string(&rabbit_message).unwrap().into_bytes(),
                BasicProperties::default(),
            )
            .await
        {
            Ok(_) => {
                println!("Message successfully sent to RabbitMQ");
                Ok(warp::reply::json(&"Message sent"))
            }
            Err(e) => {
                println!("Failed to send message to RabbitMQ: {:?}", e);
                Ok(warp::reply::json(&"Failed to send message to RabbitMQ"))
            }
        }
    } else {
        println!("Unable to connect to RabbitMQ");
        Ok(warp::reply::json(&"Unable to connect to RabbitMQ"))
    }
}

#[utoipa::path(
    post,
    path = "/notification/groups/{group_id}/publish",
    params(
        ("group_id" = String, Path, description = "The id of the group to publish to")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent"),
        (status = 404, description = "Channel not found")
    ),
    security(("apiBearerAuth" = []))  // Referencing the security scheme
)]
pub async fn publish_message_to_group(
    group_id: String,
    publish_request: PublishRequest,
    _claims: APIClaims, // Add claims from JWT here
    auth_header: String,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Connect to RabbitMQ using the connection pool
    let rabbit_channel_result = connect_rabbitmq().await;

    match rabbit_channel_result {
        Ok(rabbit_channel) => {
            println!("{:?}", auth_header);

            let iam_config = get_configuration(Some(auth_header));

            // Fetch group member IDs
            match identity_get_group_members_ids(
                &iam_config,
                IdentityGetGroupMembersIdsParams {
                    group_identifier: group_id,
                },
            )
            .await
            {
                Ok(ids) => {
                    let mut publish_results = vec![]; // Collect publish results

                    for id in ids {
                        let rabbit_message = RabbitMessage {
                            channel_id: id.clone().to_string(), // Use the ID as the channel ID
                            message: publish_request.message.clone(),
                        };

                        // Publish the message to the corresponding channel
                        let publish_result = rabbit_channel
                            .basic_publish(
                                "real-time-updates", // Exchange name
                                "",                  // Routing key
                                BasicPublishOptions::default(),
                                &serde_json::to_string(&rabbit_message).unwrap().into_bytes(),
                                BasicProperties::default(),
                            )
                            .await;

                        // Store the result of the publish attempt
                        match publish_result {
                            Ok(_) => {
                                println!("Message successfully sent to RabbitMQ for ID: {}", id);
                                publish_results.push(format!("Message sent for ID: {}", id));
                            }
                            Err(e) => {
                                println!(
                                    "Failed to send message to RabbitMQ for ID: {}: {:?}",
                                    id, e
                                );
                                publish_results.push(format!(
                                    "Failed to send message for ID: {}: {:?}",
                                    id, e
                                ));
                            }
                        }
                    }

                    // Respond with the results of the publish attempts
                    Ok(warp::reply::json(&publish_results))
                }
                Err(e) => {
                    println!("{:?}", e);
                    Ok(warp::reply::json(&"Failed to get group members"))
                }
            }
        }
        Err(e) => {
            println!("Unable to connect to RabbitMQ: {:?}", e);
            Ok(warp::reply::json(&"Unable to connect to RabbitMQ"))
        }
    }
}

use ginger_shared_rs::{rocket_utils::{APIClaims, Claims}, ISCClaims};

use crate::{
    requests::{PublishRequest, RabbitMessage},
    shared::{connect_rabbitmq, Channels},
};
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions},
    BasicProperties, Channel as RabbitChannel, Connection, ConnectionProperties,
}; // Renaming lapin::Channel to RabbitChannel


use IAMService::{
    apis::default_api::{
        identity_get_group_members_ids, 
        identity_get_group_members_ids_api_land,
        IdentityGetGroupMembersIdsParams,
        IdentityGetGroupMembersIdsApiLandParams
    },
    get_configuration,
};


// For API Land
async fn publish_message_to_group_api_land_internal(
    group_id: String,
    publish_request: PublishRequest,
    auth_header: String,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Connect to RabbitMQ using the connection pool
    let rabbit_channel_result = connect_rabbitmq().await;

    match rabbit_channel_result {
        Ok(rabbit_channel) => {
            println!("{:?}", auth_header);

            let iam_config = get_configuration(Some(auth_header.clone()));

            // For API land
            match identity_get_group_members_ids_api_land(
                &iam_config,
                IdentityGetGroupMembersIdsApiLandParams {
                    group_identifier: group_id,
                },
            ).await {
                Ok(ids) => {
                    let mut publish_results = vec![];

                    for id in ids {
                        let rabbit_message = RabbitMessage {
                            channel_id: format!("{}_{}", publish_request.prefix,  id.clone().to_string()),
                            message: publish_request.message.clone(),
                        };

                        let publish_result = rabbit_channel
                            .basic_publish(
                                "real-time-updates",
                                "",
                                BasicPublishOptions::default(),
                                &serde_json::to_string(&rabbit_message).unwrap().into_bytes(),
                                BasicProperties::default(),
                            )
                            .await;

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

                    Ok(warp::reply::json(&publish_results))
                }
                Err(e) => {
                    println!("Failed to get group members (API Land): {:?}", e);
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


// For ISC
async fn publish_message_to_group_isc_internal(
    group_id: String,
    publish_request: PublishRequest,
    auth_header: String,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Connect to RabbitMQ using the connection pool
    let rabbit_channel_result = connect_rabbitmq().await;

    match rabbit_channel_result {
        Ok(rabbit_channel) => {
            println!("{:?}", auth_header);

            let iam_config = get_configuration(Some(auth_header.clone()));

            // For ISC
            match identity_get_group_members_ids(
                &iam_config,
                IdentityGetGroupMembersIdsParams {
                    group_identifier: group_id,
                },
            ).await {
                Ok(ids) => {
                    let mut publish_results = vec![];

                    for id in ids {
                        let rabbit_message = RabbitMessage {
                            channel_id: format!("{}_{}", publish_request.prefix,  id.clone().to_string()),
                            message: publish_request.message.clone(),
                        };

                        let publish_result = rabbit_channel
                            .basic_publish(
                                "real-time-updates",
                                "",
                                BasicPublishOptions::default(),
                                &serde_json::to_string(&rabbit_message).unwrap().into_bytes(),
                                BasicProperties::default(),
                            )
                            .await;

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

                    Ok(warp::reply::json(&publish_results))
                }
                Err(e) => {
                    println!("Failed to get group members (ISC): {:?}", e);
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


async fn publish_message_internal(
    channel_name: String,
    publish_request: PublishRequest,
) -> Result<impl warp::Reply, warp::Rejection> {
    if let Ok(rabbit_channel) = connect_rabbitmq().await {
        let rabbit_message = RabbitMessage {
            channel_id: channel_name,
            message: publish_request.message,
        };

        match rabbit_channel
            .basic_publish(
                "real-time-updates",
                "",
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
    path = "/notification/channels/{channel_name}/publish",
    params(
        ("channel_name" = String, Path, description = "The name of the channel to publish to")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent"),
        (status = 404, description = "Channel not found")
    ),
    security(("apiISCBearerAuth" = [])),  // Referencing the security scheme
    tag = "default"
)]
pub async fn publish_message(
    channel_name: String,
    publish_request: PublishRequest,
    _claims: ISCClaims,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_internal(channel_name, publish_request).await
}


#[utoipa::path(
    post,
    path = "/notification/user-land/channels/{channel_name}/publish",
    params(
        ("channel_name" = String, Path, description = "The name of the channel to publish to")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent"),
        (status = 404, description = "Channel not found")
    ),
    security(("bearerAuth" = [])),  // Referencing the security scheme
    tag = "default"
)]
pub async fn publish_message_userland(
    channel_name: String,
    publish_request: PublishRequest,
    _claims: Claims,
    _auth_header: String,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_internal(channel_name, publish_request).await
}


#[utoipa::path(
    post,
    path = "/notification/api-land/groups/{group_id}/publish",
    params(
        ("group_id" = String, Path, description = "The id of the group to publish to")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent"),
        (status = 404, description = "Channel not found")
    ),
    security(("apiBearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message_to_group_api_land(
    group_id: String,
    publish_request: PublishRequest,
    _claims: APIClaims,
    auth_header: String,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Call API-land specific implementation
    publish_message_to_group_api_land_internal(group_id, publish_request, auth_header).await
}


#[utoipa::path(
    post,
    path = "/notification/groups/{group_id}/publish",
    params(
        ("group_id" = String, Path, description = "The id of the group to publish to (ISC)")
    ),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "Message sent (ISC)"),
        (status = 404, description = "Channel not found (ISC)")
    ),
    security(("apiISCBearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message_to_group(
    group_id: String,
    publish_request: PublishRequest,
    _claims: ISCClaims,
    auth_header: String,
    _channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Call ISC specific implementation
    publish_message_to_group_isc_internal(group_id, publish_request, auth_header).await
}
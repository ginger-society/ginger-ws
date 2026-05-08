use ginger_shared_rs::{rocket_utils::{APIClaims, Claims}, ISCClaims};

use crate::{
    requests::{PublishRequest, PublishType, RabbitMessage},
    shared::{Channels, RabbitPoolRef, publish_to_rabbitmq},
};

use IAMService::{
    apis::default_api::{
        identity_get_group_members_ids,
        identity_get_group_members_ids_api_land,
        IdentityGetGroupMembersIdsParams,
        IdentityGetGroupMembersIdsApiLandParams,
    },
    get_configuration,
};

// ─── internal helpers ─────────────────────────────────────────────────────────

async fn publish_message_internal(
    channel_name: String,
    publish_request: PublishRequest,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    let rabbit_message = serde_json::json!({
        "channel_id": channel_name,
        "message": publish_request.message,
    })
    .to_string();

    publish_to_rabbitmq(&rabbit_pool, &channel_name, &rabbit_message).await;

    Ok(warp::reply::json(&"Message sent"))
}

async fn publish_message_to_group_api_land_internal(
    group_id: String,
    publish_request: PublishRequest,
    auth_header: String,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    let iam_config = get_configuration(Some(auth_header.clone()));

    match publish_request.pubType {
        PublishType::Group => {
            let channel_id = format!("{}_{}", publish_request.prefix, group_id);
            let msg = serde_json::json!({
                "channel_id": channel_id,
                "message": publish_request.message,
            })
            .to_string();

            publish_to_rabbitmq(&rabbit_pool, &channel_id, &msg).await;

            println!("Message sent to RabbitMQ for Group: {}", group_id);
            Ok(warp::reply::json(&vec![format!("Message sent for Group: {}", group_id)]))
        }
        PublishType::Members => {
            match identity_get_group_members_ids_api_land(
                &iam_config,
                IdentityGetGroupMembersIdsApiLandParams {
                    group_identifier: group_id.clone(),
                },
            )
            .await
            {
                Ok(ids) => {
                    let mut results = vec![];
                    for id in ids {
                        let channel_id = format!("{}_{}", publish_request.prefix, id);
                        let msg = serde_json::json!({
                            "channel_id": channel_id,
                            "message": publish_request.message,
                        })
                        .to_string();

                        publish_to_rabbitmq(&rabbit_pool, &channel_id, &msg).await;
                        results.push(format!("Message sent for ID: {}", id));
                    }
                    Ok(warp::reply::json(&results))
                }
                Err(e) => {
                    eprintln!("Failed to get group members (API Land): {:?}", e);
                    Ok(warp::reply::json(&vec!["Failed to get group members".to_string()]))
                }
            }
        }
    }
}

async fn publish_message_to_group_isc_internal(
    group_id: String,
    publish_request: PublishRequest,
    auth_header: String,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    let iam_config = get_configuration(Some(auth_header.clone()));

    match publish_request.pubType {
        PublishType::Group => {
            let channel_id = format!("{}_{}", publish_request.prefix, group_id);
            let msg = serde_json::json!({
                "channel_id": channel_id,
                "message": publish_request.message,
            })
            .to_string();

            publish_to_rabbitmq(&rabbit_pool, &channel_id, &msg).await;

            println!("Message sent to RabbitMQ for Group: {}", group_id);
            Ok(warp::reply::json(&vec![format!("Message sent for Group: {}", group_id)]))
        }
        PublishType::Members => {
            match identity_get_group_members_ids(
                &iam_config,
                IdentityGetGroupMembersIdsParams {
                    group_identifier: group_id.clone(),
                },
            )
            .await
            {
                Ok(ids) => {
                    let mut results = vec![];
                    for id in ids {
                        let channel_id = format!("{}_{}", publish_request.prefix, id);
                        let msg = serde_json::json!({
                            "channel_id": channel_id,
                            "message": publish_request.message,
                        })
                        .to_string();

                        publish_to_rabbitmq(&rabbit_pool, &channel_id, &msg).await;
                        results.push(format!("Message sent for ID: {}", id));
                    }
                    Ok(warp::reply::json(&results))
                }
                Err(e) => {
                    eprintln!("Failed to get group members (ISC): {:?}", e);
                    Ok(warp::reply::json(&vec!["Failed to get group members".to_string()]))
                }
            }
        }
    }
}

// ─── public handlers ──────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/notification/channels/{channel_name}/publish",
    params(("channel_name" = String, Path, description = "Channel to publish to")),
    request_body = PublishRequest,
    responses((status = 200, description = "Message sent")),
    security(("apiISCBearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message(
    channel_name: String,
    publish_request: PublishRequest,
    _claims: ISCClaims,
    _channels: Channels,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_internal(channel_name, publish_request, rabbit_pool).await
}

#[utoipa::path(
    post,
    path = "/notification/user-land/channels/{channel_name}/publish",
    params(("channel_name" = String, Path, description = "Channel to publish to")),
    request_body = PublishRequest,
    responses((status = 200, description = "Message sent")),
    security(("bearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message_userland(
    channel_name: String,
    publish_request: PublishRequest,
    _claims: Claims,
    _auth_header: String,
    _channels: Channels,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_internal(channel_name, publish_request, rabbit_pool).await
}

#[utoipa::path(
    post,
    path = "/notification/api-land/groups/{group_id}/publish",
    params(("group_id" = String, Path, description = "Group ID to publish to")),
    request_body = PublishRequest,
    responses((status = 200, description = "Message sent")),
    security(("apiBearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message_to_group_api_land(
    group_id: String,
    publish_request: PublishRequest,
    _claims: APIClaims,
    auth_header: String,
    _channels: Channels,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_to_group_api_land_internal(group_id, publish_request, auth_header, rabbit_pool).await
}

#[utoipa::path(
    post,
    path = "/notification/groups/{group_id}/publish",
    params(("group_id" = String, Path, description = "Group ID to publish to (ISC)")),
    request_body = PublishRequest,
    responses((status = 200, description = "Message sent (ISC)")),
    security(("apiISCBearerAuth" = [])),
    tag = "default"
)]
pub async fn publish_message_to_group(
    group_id: String,
    publish_request: PublishRequest,
    _claims: ISCClaims,
    auth_header: String,
    _channels: Channels,
    rabbit_pool: RabbitPoolRef,
) -> Result<impl warp::Reply, warp::Rejection> {
    publish_message_to_group_isc_internal(group_id, publish_request, auth_header, rabbit_pool).await
}
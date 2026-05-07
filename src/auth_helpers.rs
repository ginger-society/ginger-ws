use crate::{
    requests::{WampCalleeDisconnected, WampCalleeOffline, WampEvent, WampPublish},
    responses::{InvalidTokenError, JWTError},
    shared::{
        publish_to_rabbitmq, Channel, Channels, Connections, PendingCall, PendingCalls,
        WsConnection,
    },
};
use futures::{sink::SinkExt, StreamExt};
use ginger_shared_rs::{rocket_utils::{APIClaims, Claims}, ISCClaims};
use jsonwebtoken::{decode, DecodingKey, Validation};
use tokio::sync::broadcast;
use uuid::Uuid;
use warp::{reject::Rejection, ws::{Message, WebSocket}, Filter};

pub async fn user_connected(
    ws: WebSocket,
    channel_name: String,
    channels: Channels,
    connections: Connections,
    pending_calls: PendingCalls,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let connection_id = Uuid::new_v4();

    // ── register this connection ──────────────────────────────────────────────
    let (conn_tx, _) = broadcast::channel::<String>(32);
    {
        let mut conns = connections.lock().await;
        conns.insert(connection_id, WsConnection {
            id: connection_id,
            tx: conn_tx.clone(),
        });
    }

    println!(
        "[ws] connection {} registered on channel '{}'",
        connection_id, channel_name
    );

    // ── join or create the named channel ──────────────────────────────────────
    let (channel_tx, mut channel_rx) = {
        let mut channels_lock = channels.lock().await;
        let channel = channels_lock
            .entry(channel_name.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100);
                Channel { name: channel_name.clone(), tx }
            });
        (channel.tx.clone(), channel.tx.subscribe())
    };

    // ── inbound: client → broker ──────────────────────────────────────────────
    let channels_inbound      = channels.clone();
    let pending_calls_inbound = pending_calls.clone();
    let connections_inbound   = connections.clone();
    let channel_name_inbound  = channel_name.clone();

    tokio::spawn(async move {
        while let Some(result) = ws_rx.next().await {
            if let Ok(msg) = result {
                if let Ok(text) = msg.to_str() {

                    if let Ok(publish) = serde_json::from_str::<WampPublish>(text) {
                        let publication_id = rand::random::<u64>();
                        let target_channel = publish.topic.clone();

                        // ── check if this is a result coming back from callee ─
                        // if kwargs contains is_result: true, resolve the pending call
                        let is_result = publish.kwargs
                            .as_ref()
                            .and_then(|kw| kw.get("is_result"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        if is_result {
                            if let Some(corr_id) = &publish.options.correlation_id {
                                let mut pending = pending_calls_inbound.lock().await;
                                if pending.remove(corr_id).is_some() {
                                    println!(
                                        "[ws] pending call resolved corr={}",
                                        corr_id
                                    );
                                }
                            }
                        }

                        // ── check receiver count ──────────────────────────────
                        let receiver_count = {
                            let channels_lock = channels_inbound.lock().await;
                            channels_lock
                                .get(&target_channel)
                                .map(|ch| ch.tx.receiver_count())
                                .unwrap_or(0)
                        };

                        if receiver_count == 0 {
                            // callee offline — notify caller via RabbitMQ
                            if let Some(reply_to) = &publish.options.reply_to {
                                let offline = WampCalleeOffline {
                                    message_type: 0,
                                    error: "callee_offline".to_string(),
                                    correlation_id: publish.options.correlation_id.clone(),
                                    topic: target_channel.clone(),
                                };
                                publish_to_rabbitmq(
                                    reply_to,
                                    &serde_json::to_string(&offline).unwrap(),
                                )
                                .await;
                                println!(
                                    "[ws] callee_offline on '{}' — notified caller on '{}'",
                                    target_channel, reply_to
                                );
                            } else {
                                println!(
                                    "[ws] PUBLISH to offline channel '{}' — no reply_to set",
                                    target_channel
                                );
                            }

                        } else {
                            // callee online — deliver via RabbitMQ
                            let event = WampEvent::from_publish(&publish, publication_id);
                            let event_str = serde_json::to_string(&event).unwrap();
                            publish_to_rabbitmq(&target_channel, &event_str).await;

                            println!(
                                "[ws] PUBLISH → '{}' pub_id={} receivers={}",
                                target_channel, publication_id, receiver_count
                            );

                            // track as pending call if RPC-style and not a result
                            if !is_result {
                                if let (Some(corr_id), Some(reply_to)) = (
                                    publish.options.correlation_id.clone(),
                                    publish.options.reply_to.clone(),
                                ) {
                                    let mut pending = pending_calls_inbound.lock().await;
                                    pending.insert(
                                        corr_id.clone(),
                                        PendingCall {
                                            correlation_id: corr_id.clone(),
                                            reply_to,
                                            callee_channel: target_channel.clone(),
                                            caller_connection_id: connection_id,
                                            created_at: std::time::Instant::now(),
                                        },
                                    );
                                    println!(
                                        "[ws] pending call tracked corr={}",
                                        corr_id
                                    );
                                }
                            }
                        }

                    } else {
                        // legacy raw string — broadcast to connected channel as-is
                        let _ = channel_tx.send(text.to_string());
                    }
                }
            }
        }

        // ── client disconnected ───────────────────────────────────────────────
        println!(
            "[ws] connection {} disconnected — running cleanup",
            connection_id
        );

        // Step 1: collect orphaned calls where this connection was the callee
        let orphaned: Vec<PendingCall> = {
            let pending = pending_calls_inbound.lock().await;
            pending
                .values()
                .filter(|pc| {
                    pc.callee_channel == channel_name_inbound
                        && pc.caller_connection_id != connection_id
                })
                .cloned()
                .collect()
        };

        // Step 2: notify callers via RabbitMQ and remove orphaned calls
        if !orphaned.is_empty() {
            let mut pending = pending_calls_inbound.lock().await;

            for pc in &orphaned {
                let error = WampCalleeDisconnected {
                    message_type: 0,
                    error: "callee_disconnected".to_string(),
                    correlation_id: Some(pc.correlation_id.clone()),
                };

                publish_to_rabbitmq(
                    &pc.reply_to,
                    &serde_json::to_string(&error).unwrap(),
                )
                .await;

                println!(
                    "[ws] callee_disconnected — notified caller on '{}' corr={}",
                    pc.reply_to, pc.correlation_id
                );

                pending.remove(&pc.correlation_id);
            }
        }

        // Step 3: remove from connection registry
        {
            let mut conns = connections_inbound.lock().await;
            conns.remove(&connection_id);
        }

        println!("[ws] connection {} fully cleaned up", connection_id);
    });

    // ── outbound: broker → this client ───────────────────────────────────────
    tokio::spawn(async move {
        while let Ok(message) = channel_rx.recv().await {
            if ws_tx.send(Message::text(message)).await.is_err() {
                break;
            }
        }
    });
}

pub async fn handle_ws_upgrade(
    (ws, channel_name, channels, connections, pending_calls): (
        warp::ws::Ws,
        String,
        Channels,
        Connections,
        PendingCalls,
    ),
) -> Result<impl warp::Reply, Rejection> {
    Ok(ws.on_upgrade(move |socket| {
        user_connected(socket, channel_name, channels, connections, pending_calls)
    }))
}

pub async fn user_authenticated(
    channel_name: String,
    ws: warp::ws::Ws,
    channels: Channels,
    connections: Connections,
    pending_calls: PendingCalls,
    token: Option<String>,
) -> Result<(warp::ws::Ws, String, Channels, Connections, PendingCalls), Rejection> {
    if let Some(token) = token {
        let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "1234".to_string());
        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        if let Ok(token_data) = decode::<Claims>(&token, &decoding_key, &validation) {
            println!("Authenticated user: {:?}", token_data.claims.user_id);
            return Ok((ws, channel_name, channels, connections, pending_calls));
        }

        if let Ok(token_data) = decode::<APIClaims>(&token, &decoding_key, &validation) {
            println!("Authenticated API user: {:?}", token_data.claims.sub);
            return Ok((ws, channel_name, channels, connections, pending_calls));
        }

        println!("Unauthorized access attempt");
        Err(warp::reject::custom(JWTError))
    } else {
        println!("Token query parameter missing");
        Err(warp::reject::custom(JWTError))
    }
}

pub async fn authenticate_token(token: Option<String>) -> Result<Claims, warp::Rejection> {
    if let Some(token) = token {
        let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "1234".to_string());
        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        match decode::<Claims>(&token, &decoding_key, &validation) {
            Ok(token_data) => Ok(token_data.claims),
            Err(_) => Err(warp::reject::custom(JWTError)),
        }
    } else {
        Err(warp::reject::custom(JWTError))
    }
}

pub fn with_auth() -> impl Filter<Extract = (Claims,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("Authorization").and_then(
        |auth_header: Option<String>| async move {
            if let Some(token) = auth_header {
                let token = token.trim_start_matches("Bearer ").to_string();
                authenticate_token(Some(token)).await
            } else {
                Err(warp::reject::custom(JWTError))
            }
        },
    )
}

pub async fn authenticate_isc_api_token(
    token: Option<String>,
) -> Result<ISCClaims, warp::Rejection> {
    if let Some(token) = token {
        let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "1234".to_string());
        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        match decode::<ISCClaims>(&token, &decoding_key, &validation) {
            Ok(token_data) => Ok(token_data.claims),
            Err(_) => Err(warp::reject::custom(JWTError)),
        }
    } else {
        Err(warp::reject::custom(JWTError))
    }
}

pub async fn authenticate_api_token(
    token: Option<String>,
) -> Result<APIClaims, warp::Rejection> {
    if let Some(token) = token {
        let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "1234".to_string());
        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        match decode::<APIClaims>(&token, &decoding_key, &validation) {
            Ok(token_data) => Ok(token_data.claims),
            Err(_) => Err(warp::reject::custom(JWTError)),
        }
    } else {
        Err(warp::reject::custom(JWTError))
    }
}

pub fn with_api_auth() -> impl Filter<Extract = (APIClaims,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("X-API-Authorization").and_then(
        |auth_header: Option<String>| async move {
            if let Some(token) = auth_header {
                let token = token.trim_start_matches("Bearer ").to_string();
                authenticate_api_token(Some(token)).await
            } else {
                Err(warp::reject::custom(JWTError))
            }
        },
    )
}

pub fn with_isc_api_auth(
) -> impl Filter<Extract = (ISCClaims,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("X-ISC-API-Authorization").and_then(
        |auth_header: Option<String>| async move {
            if let Some(token) = auth_header {
                let token = token.trim_start_matches("Bearer ").to_string();
                authenticate_isc_api_token(Some(token)).await
            } else {
                Err(warp::reject::custom(JWTError))
            }
        },
    )
}

pub fn with_get_auth_header(
) -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
    warp::header::<String>("Authorization").and_then(|auth_header: String| async move {
        let token = auth_header
            .strip_prefix("Bearer ")
            .or(Some(auth_header.as_str()))
            .unwrap_or("");
        if !token.is_empty() {
            Ok(token.to_string())
        } else {
            Err(warp::reject::custom(InvalidTokenError))
        }
    })
}

pub fn with_get_api_auth_header(
) -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
    warp::header::<String>("X-API-Authorization").and_then(|auth_header: String| async move {
        let token = auth_header
            .strip_prefix("Bearer ")
            .or(Some(auth_header.as_str()))
            .unwrap_or("");
        if !token.is_empty() {
            Ok(token.to_string())
        } else {
            Err(warp::reject::custom(InvalidTokenError))
        }
    })
}

pub fn with_get_isc_auth_header(
) -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
    warp::header::<String>("X-ISC-API-Authorization").and_then(
        |auth_header: String| async move {
            let token = auth_header
                .strip_prefix("Bearer ")
                .or(Some(auth_header.as_str()))
                .unwrap_or("");
            if !token.is_empty() {
                Ok(token.to_string())
            } else {
                Err(warp::reject::custom(InvalidTokenError))
            }
        },
    )
}
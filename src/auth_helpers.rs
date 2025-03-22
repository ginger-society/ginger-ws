// Handle a new WebSocket connection
use crate::{
    responses::JWTError,
    shared::{Channel, Channels},
};
use futures::StreamExt;
use ginger_shared_rs::rocket_utils::Claims;
use jsonwebtoken::{decode, DecodingKey, Validation};
use tokio::sync::{broadcast, Mutex};
use warp::{
    reject::Rejection,
    ws::{Message, WebSocket},
};

use futures::sink::SinkExt;

pub async fn user_connected(ws: WebSocket, channel_name: String, channels: Channels) {
    let (mut tx, mut rx) = ws.split();

    let (channel_tx, mut channel_rx) = {
        let mut channels_lock = channels.lock().await;
        let channel = channels_lock
            .entry(channel_name.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100);
                Channel {
                    name: channel_name.clone(),
                    tx,
                }
            });

        (channel.tx.clone(), channel.tx.subscribe())
    };

    tokio::spawn(async move {
        while let Some(result) = rx.next().await {
            if let Ok(msg) = result {
                if let Ok(text) = msg.to_str() {
                    let _ = channel_tx.send(text.to_string());
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Ok(message) = channel_rx.recv().await {
            if tx.send(Message::text(message)).await.is_err() {
                break;
            }
        }
    });
}

pub async fn handle_ws_upgrade(
    (ws, channel_name, channels): (warp::ws::Ws, String, Channels),
) -> Result<impl warp::Reply, Rejection> {
    Ok(ws.on_upgrade(move |socket| user_connected(socket, channel_name, channels)))
}
pub async fn user_authenticated(
    channel_name: String,
    ws: warp::ws::Ws,
    channels: Channels,
    token: Option<String>, // Extract token from query parameters
) -> Result<(warp::ws::Ws, String, Channels), Rejection> {
    if let Some(token) = token {
        // No need to trim "Bearer " since the token is expected to be plain
        let secret = "1234";

        let decoding_key = DecodingKey::from_secret(secret.as_ref());
        let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

        // Try decoding as `Claims`
        if let Ok(token_data) = decode::<Claims>(&token, &decoding_key, &validation) {
            println!("Authenticated user: {:?}", token_data.claims.user_id);
            return Ok((ws, channel_name, channels));
        }

        // Try decoding as `APIClaims`
        if let Ok(token_data) = decode::<APIClaims>(&token, &decoding_key, &validation) {
            println!("Authenticated API user: {:?}", token_data.claims.sub);
            return Ok((ws, channel_name, channels));
        }

        println!("Unauthorized access attempt");
        Err(warp::reject::custom(JWTError))
    } else {
        println!("Token query parameter missing");
        Err(warp::reject::custom(JWTError))
    }
}

use ginger_shared_rs::{rocket_utils::APIClaims, ISCClaims};
use warp::Filter;

use crate::responses::InvalidTokenError;

pub async fn authenticate_token(token: Option<String>) -> Result<Claims, warp::Rejection> {
    if let Some(token) = token {
        let secret = "1234"; // Use environment variable in production

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
        let secret = "1234"; // Use environment variable in production

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

pub async fn authenticate_api_token(token: Option<String>) -> Result<APIClaims, warp::Rejection> {
    if let Some(token) = token {
        let secret = "1234"; // Use environment variable in production

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

pub fn with_isc_api_auth() -> impl Filter<Extract = (ISCClaims,), Error = warp::Rejection> + Clone {
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

pub fn with_get_auth_header() -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
    warp::header::<String>("Authorization").and_then(|auth_header: String| async move {
        // Extract the token from the header "Authorization: token"
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
        // Extract the token from the header "Authorization: token"
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

use crate::mailer::__path_send_email;
use crate::rest_bridge::{__path_publish_message, __path_publish_message_to_group};
use aws_config::meta::region::RegionProviderChain;
use aws_config::Region;
use aws_sdk_ses::types::Body as EmailBody;
use aws_sdk_ses::types::Content as EmailContent;
use aws_sdk_ses::types::Destination;
use aws_sdk_ses::types::Message as EmailMessage;
use aws_sdk_ses::Client;
use futures::sink::SinkExt;
use futures::StreamExt;
use ginger_shared_rs::rocket_utils::APIClaims;
use ginger_shared_rs::rocket_utils::Claims;
use ginger_shared_rs::ISCClaims;
use jsonwebtoken::{decode, DecodingKey, Validation};
use lapin::Error as LapinError;
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions},
    BasicProperties, Channel as RabbitChannel, Connection, ConnectionProperties,
}; // Renaming lapin::Channel to RabbitChannel
use prometheus::{Encoder, IntCounter, Opts, Registry, TextEncoder};
use requests::EmailRequest;
use requests::PublishRequest;
use requests::RabbitMessage;
use rest_bridge::publish_message;
use rest_bridge::publish_message_to_group;
use serde::{Deserialize, Serialize};
use shared::connect_rabbitmq;
use shared::Channels;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio::time::{sleep, Duration};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{openapi, Modify};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::Config;
use warp::reject::Reject;
use warp::Filter;
use warp::{
    reject::Rejection,
    ws::{Message, WebSocket},
};
use IAMService::apis::default_api::{
    identity_get_group_members_ids, identity_get_group_members_ids_user_land,
    IdentityGetGroupMembersIdsParams, IdentityGetGroupMembersIdsUserLandParams,
};
mod mailer;
mod requests;
mod rest_bridge;
mod shared;
use crate::mailer::send_email;
use crate::shared::Channel;

// Create a registry to hold the metrics
lazy_static::lazy_static! {
    static ref REGISTRY: Registry = Registry::new();
}

// Define metrics
lazy_static::lazy_static! {
    static ref REQUEST_COUNTER: IntCounter = IntCounter::with_opts(Opts::new("request_count", "Total number of requests"))
        .expect("Counter can be created");
}

#[derive(Debug)]
struct EncodeError;
impl Reject for EncodeError {}

async fn metrics_handler() -> Result<impl warp::Reply, warp::Rejection> {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();

    // Gather metrics from the global registry
    REGISTRY.gather();

    // Encode the metrics
    encoder
        .encode(&REGISTRY.gather(), &mut buffer)
        .map_err(|e| warp::reject::custom(EncodeError))?;

    Ok(warp::reply::with_header(
        String::from_utf8(buffer).unwrap(),
        "Content-Type",
        "text/plain; version=0.0.4",
    ))
}

async fn authenticate_token(token: Option<String>) -> Result<Claims, warp::Rejection> {
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

fn with_auth() -> impl Filter<Extract = (Claims,), Error = warp::Rejection> + Clone {
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

async fn authenticate_isc_api_token(token: Option<String>) -> Result<ISCClaims, warp::Rejection> {
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

async fn authenticate_api_token(token: Option<String>) -> Result<APIClaims, warp::Rejection> {
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

fn with_api_auth() -> impl Filter<Extract = (APIClaims,), Error = warp::Rejection> + Clone {
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

fn with_isc_api_auth() -> impl Filter<Extract = (ISCClaims,), Error = warp::Rejection> + Clone {
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

// Define the custom rejection error
#[derive(Debug)]
struct InvalidTokenError;
impl Reject for InvalidTokenError {}

fn with_get_auth_header() -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
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

fn with_get_api_auth_header() -> impl Filter<Extract = (String,), Error = warp::Rejection> + Clone {
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

// Swagger configuration for the REST endpoints
#[derive(OpenApi)]
#[openapi(
    paths(publish_message, publish_message_to_group, send_email),
    components(
        schemas(PublishRequest, EmailRequest)
    ),
    tags((name = "channels", description = "Channel publishing and WebSocket subscription")),
    modifiers(&SecurityAddon),
)]
struct ApiDoc;

struct SecurityAddon;
impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // Safely get the components
        let components = openapi.components.as_mut().unwrap();

        // Add a Bearer token security scheme globally
        components.add_security_scheme(
            "bearerAuth", // Name of the security scheme
            SecurityScheme::Http(
                Http::new(HttpAuthScheme::Bearer), // Define it as a Bearer token type
            ),
        );
        // Create the ApiKeyValue instance using the non-exhaustive struct's field(s)
        let api_key_value = ApiKeyValue::new("X-API-Authorization".to_string());

        // Using `ApiKey` enum to specify it as a header
        let api_key_scheme = SecurityScheme::ApiKey(ApiKey::Header(api_key_value));

        // Add the API key security scheme to the components
        components.add_security_scheme("apiBearerAuth", api_key_scheme);

        let api_isc_key_value = ApiKeyValue::new("X-ISC-API-Authorization".to_string());
        let api_isc_key_scheme = SecurityScheme::ApiKey(ApiKey::Header(api_isc_key_value));
        components.add_security_scheme("apiISCBearerAuth", api_isc_key_scheme);
    }
}

#[tokio::main]
async fn main() {
    // Register metrics with the global registry
    REGISTRY
        .register(Box::new(REQUEST_COUNTER.clone()))
        .unwrap();

    // Define the metrics route
    let metrics_route = warp::path("notification")
        .and(warp::path("metrics"))
        .and(warp::get())
        .and_then(metrics_handler);

    let channels: Channels = Arc::new(Mutex::new(HashMap::new()));

    // Start RabbitMQ consumer
    let channels_clone = channels.clone();
    tokio::spawn(async move {
        consume_messages(channels_clone).await
        // Ensure the block returns `()`
    });

    // WebSocket endpoint to subscribe to channels
    let channels_ws = channels.clone();
    // Modify the websocket_route to extract token from query parameters
    let websocket_route = warp::path("notification")
        .and(warp::path("ws"))
        .and(warp::path::param::<String>()) // Channel name
        .and(warp::ws()) // WebSocket instance
        .and(warp::query::<HashMap<String, String>>()) // Extract query parameters
        .and(with_channels(channels_ws)) // Channels
        .and_then(
            |channel_name, ws, query_params: HashMap<String, String>, channels| {
                let token = query_params.get("token").cloned(); // Get token from query params
                user_authenticated(channel_name, ws, channels, token) // Pass the token
            },
        )
        .and_then(handle_ws_upgrade); // Handle WebSocket upgrade

    let channels_rest = channels.clone();
    let publish_route = warp::path("notification")
        .and(warp::path!("channels" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_auth()) // Add authentication here
        .and(with_get_auth_header())
        .and(with_channels(channels_rest))
        .and_then(publish_message);

    let channels_rest = channels.clone();
    let group_publish_route = warp::path("notification")
        .and(warp::path!("groups" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_api_auth()) // Add authentication here
        .and(with_get_api_auth_header())
        .and(with_channels(channels_rest))
        .and_then(publish_message_to_group);

    let send_email_route = warp::path("notification")
        .and(warp::path!("send-email"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_isc_api_auth()) // Add authentication here
        .and_then(send_email);

    // Serve OpenAPI spec
    let api_doc = warp::path("notification")
        .and(warp::path("api-doc.json"))
        .and(warp::get())
        .map(|| warp::reply::json(&ApiDoc::openapi()));

    // Serve Swagger UI
    let config = Arc::new(Config::from("/notification/api-doc.json"));
    let swagger_ui = warp::path("notification")
        .and(warp::path("swagger-ui"))
        .and(warp::get())
        .and(warp::path::full())
        .and(warp::path::tail())
        .and(warp::any().map(move || config.clone()))
        .and_then(serve_swagger);

    // Combine all routes
    let routes = websocket_route
        .or(publish_route)
        .or(group_publish_route)
        .or(api_doc)
        .or(send_email_route)
        .or(swagger_ui)
        .or(metrics_route);

    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}

// Filter to inject channels into the route handlers
fn with_channels(
    channels: Channels,
) -> impl Filter<Extract = (Channels,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || channels.clone())
}

// Handle a new WebSocket connection
async fn user_connected(ws: WebSocket, channel_name: String, channels: Channels) {
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

// Custom JWT Error
#[derive(Debug)]
struct JWTError;

impl warp::reject::Reject for JWTError {}

async fn handle_ws_upgrade(
    (ws, channel_name, channels): (warp::ws::Ws, String, Channels),
) -> Result<impl warp::Reply, Rejection> {
    Ok(ws.on_upgrade(move |socket| user_connected(socket, channel_name, channels)))
}
async fn user_authenticated(
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

        match decode::<Claims>(&token, &decoding_key, &validation) {
            Ok(token_data) => {
                println!("Authenticated user: {:?}", token_data.claims.user_id);
                Ok((ws, channel_name, channels))
            }
            Err(e) => {
                println!("Unauthorized access attempt : {}", e);
                Err(warp::reject::custom(JWTError))
            }
        }
    } else {
        println!("Token query parameter missing");
        Err(warp::reject::custom(JWTError))
    }
}

async fn consume_messages(channels: Channels) {
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

async fn process_rabbitmq_messages(
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

// Serve Swagger UI assets
async fn serve_swagger(
    full_path: warp::path::FullPath,
    tail: warp::path::Tail,
    config: Arc<Config<'static>>,
) -> Result<Box<dyn warp::Reply + 'static>, warp::Rejection> {
    if full_path.as_str() == "/swagger-ui" {
        return Ok(Box::new(warp::redirect::found(
            warp::http::Uri::from_static("/swagger-ui/"),
        )));
    }

    let path = tail.as_str();
    match utoipa_swagger_ui::serve(path, config) {
        Ok(file) => {
            if let Some(file) = file {
                Ok(Box::new(
                    warp::http::Response::builder()
                        .header("Content-Type", file.content_type)
                        .body(file.bytes),
                ))
            } else {
                Ok(Box::new(warp::http::StatusCode::NOT_FOUND))
            }
        }
        Err(error) => Ok(Box::new(
            warp::http::Response::builder()
                .status(warp::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(error.to_string()),
        )),
    }
}

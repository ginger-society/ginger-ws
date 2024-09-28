use futures::sink::SinkExt;
use futures::StreamExt;
use jsonwebtoken::{decode, DecodingKey, Validation};
use lapin::{
    message::Delivery,
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions},
    BasicProperties, Channel as RabbitChannel, Connection, ConnectionProperties,
}; // Renaming lapin::Channel to RabbitChannel
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
    Modify, OpenApi, ToSchema,
};
use utoipa_swagger_ui::Config;
use warp::{
    reject::Rejection,
    ws::{Message, WebSocket},
};
use warp::{reply::Reply, Filter};

#[derive(Debug, Clone)]
struct Channel {
    name: String,
    tx: broadcast::Sender<String>,
}

type Channels = Arc<Mutex<HashMap<String, Channel>>>;

#[derive(Deserialize, Serialize, ToSchema)]
struct PublishRequest {
    message: String, // Only the message is in the body
}

#[derive(Deserialize, Serialize)]
struct RabbitMessage {
    channel_id: String,
    message: String,
}

// Swagger configuration for the REST endpoints
#[derive(OpenApi)]
#[openapi(
    paths(publish_message),
    components(schemas(PublishRequest)),
    tags((name = "channels", description = "Channel publishing and WebSocket subscription"))
)]
struct ApiDoc;

#[tokio::main]
async fn main() {
    let channels: Channels = Arc::new(Mutex::new(HashMap::new()));

    // Start RabbitMQ consumer
    let channels_clone = channels.clone();
    tokio::spawn(async move {
        if let Err(e) = consume_messages(channels_clone).await {
            eprintln!("Error consuming messages: {:?}", e);
        }
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
        .and(with_channels(channels_rest))
        .and_then(publish_message);

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
    let routes = websocket_route.or(publish_route).or(api_doc).or(swagger_ui);

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

#[derive(Deserialize, Serialize)]
struct Claims {
    sub: String,
    exp: usize,
    user_id: String,
    token_type: String, // Add token_type to distinguish between access and refresh tokens
    client_id: String,
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

async fn connect_rabbitmq() -> Result<RabbitChannel, lapin::Error> {
    let addr = std::env::var("AMPQ_URI")
        .unwrap_or_else(|_| "amqp://user:password@localhost:5672/%2f".to_string());

    let conn = Connection::connect(&addr, ConnectionProperties::default()).await?;
    let channel = conn.create_channel().await?;

    // Declare an exchange if needed
    channel
        .exchange_declare(
            "real-time-updates",
            lapin::ExchangeKind::Fanout,
            Default::default(),
            Default::default(),
        )
        .await?;

    // Declare a queue for consuming messages
    channel
        .queue_declare(
            "real-time-updates-queue",
            Default::default(),
            Default::default(),
        )
        .await?;

    channel
        .queue_bind(
            "real-time-updates-queue",
            "real-time-updates",
            "",
            Default::default(),
            Default::default(),
        )
        .await?;

    Ok(channel)
}

async fn consume_messages(channels: Channels) -> Result<(), lapin::Error> {
    let rabbit_channel = connect_rabbitmq().await?;

    let mut consumer = rabbit_channel
        .basic_consume(
            "real-time-updates-queue", // Queue name
            "consumer_tag",            // Consumer tag
            BasicConsumeOptions::default(),
            Default::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        match delivery {
            Ok(delivery) => {
                let message = String::from_utf8_lossy(&delivery.data).to_string();
                println!("Received message from RabbitMQ: {}", message);

                // Deserialize the message to extract `channel_id` and `message`
                if let Ok(rabbit_message) = serde_json::from_str::<RabbitMessage>(&message) {
                    let channels_lock = channels.lock().await;

                    // Check if the `channel_id` exists in the WebSocket channels
                    if let Some(channel) = channels_lock.get(&rabbit_message.channel_id) {
                        let _ = channel.tx.send(rabbit_message.message.clone());

                        // Acknowledge the RabbitMQ message after sending it to WebSocket
                        if let Err(e) = delivery.ack(BasicAckOptions::default()).await {
                            eprintln!("Failed to acknowledge message: {:?}", e);
                        }
                    } else {
                        println!(
                            "Received message for non-existent channel: {}",
                            rabbit_message.channel_id
                        );
                    }
                } else {
                    println!("Failed to deserialize message from RabbitMQ");
                }
            }
            Err(e) => {
                eprintln!("Error receiving message: {:?}", e);
            }
        }
    }

    Ok(())
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
    )
)]
async fn publish_message(
    channel_name: String,
    publish_request: PublishRequest,
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

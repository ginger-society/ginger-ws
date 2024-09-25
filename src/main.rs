use futures::sink::SinkExt;
use futures::StreamExt;
use lapin::{
    options::BasicPublishOptions, BasicProperties, Channel as RabbitChannel, Connection,
    ConnectionProperties,
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
use warp::ws::{Message, WebSocket};
use warp::Filter;

#[derive(Debug, Clone)]
struct Channel {
    name: String,
    tx: broadcast::Sender<String>,
}

type Channels = Arc<Mutex<HashMap<String, Channel>>>;

#[derive(Deserialize, ToSchema)]
struct PublishRequest {
    message: String,
}

// Swagger configuration for the REST endpoints
#[derive(OpenApi)]
#[openapi(
    paths(
        publish_message,
    ),
    components(
        schemas(PublishRequest)
    ),
    tags(
        (name = "channels", description = "Channel publishing and WebSocket subscription")
    )
)]
struct ApiDoc;

#[tokio::main]
async fn main() {
    let channels: Channels = Arc::new(Mutex::new(HashMap::new()));

    // WebSocket endpoint to subscribe to channels
    let channels_ws = channels.clone();
    let websocket_route = warp::path("ws")
        .and(warp::path::param::<String>())
        .and(warp::ws())
        .and(with_channels(channels_ws))
        .map(|channel_name: String, ws: warp::ws::Ws, channels| {
            ws.on_upgrade(move |socket| user_connected(socket, channel_name, channels))
        });

    // REST API to publish messages to a channel
    let channels_rest = channels.clone();
    let publish_route = warp::path!("channels" / String / "publish")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_channels(channels_rest))
        .and_then(publish_message);

    // Serve OpenAPI spec
    let api_doc = warp::path("api-doc.json")
        .and(warp::get())
        .map(|| warp::reply::json(&ApiDoc::openapi()));

    // Serve Swagger UI
    let config = Arc::new(Config::from("/api-doc.json"));
    let swagger_ui = warp::path("swagger-ui")
        .and(warp::get())
        .and(warp::path::full())
        .and(warp::path::tail())
        .and(warp::any().map(move || config.clone()))
        .and_then(serve_swagger);

    // Combine all routes
    let routes = websocket_route.or(publish_route).or(api_doc).or(swagger_ui);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
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

async fn connect_rabbitmq() -> Result<RabbitChannel, Box<dyn std::error::Error + Send + Sync>> {
    let addr = std::env::var("RABBITMQ_URL")
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

    Ok(channel)
}

// Publish a message to RabbitMQ and to a channel using the REST endpoint
#[utoipa::path(
    post,
    path = "/channels/{channel_name}/publish",
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
    channels: Channels,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut channels_lock = channels.lock().await;
    if let Some(channel) = channels_lock.get(&channel_name) {
        let _ = channel.tx.send(publish_request.message.clone());

        // Publish the message to RabbitMQ
        if let Ok(rabbit_channel) = connect_rabbitmq().await {
            let payload = publish_request.message.clone().into_bytes();
            match rabbit_channel
                .basic_publish(
                    "real-time-updates", // Exchange name
                    "",                  // Routing key
                    BasicPublishOptions::default(),
                    &payload,
                    BasicProperties::default(),
                )
                .await
            {
                Ok(_) => {
                    println!("Message successfully sent to RabbitMQ");
                }
                Err(e) => {
                    println!("Failed to send message to RabbitMQ: {:?}", e);
                }
            }
        } else {
            println!("unable to get rabbit mq")
        }

        Ok(warp::reply::json(&"Message sent"))
    } else {
        println!("Channel '{}' not found", channel_name);
        Ok(warp::reply::json(&"Channel not found"))
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

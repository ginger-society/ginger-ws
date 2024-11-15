use crate::mailer::__path_send_email;
use crate::rest_bridge::{__path_publish_message, __path_publish_message_to_group};

use auth_helpers::{
    handle_ws_upgrade, user_authenticated, with_api_auth, with_auth, with_get_api_auth_header,
    with_get_auth_header, with_isc_api_auth,
};

use auth_schemas::SecurityAddon;

use message_queue_helpers::consume_messages;
use prom_helpers::{metrics_handler, REGISTRY, REQUEST_COUNTER};
// Renaming lapin::Channel to RabbitChannel
use requests::EmailRequest;
use requests::PublishRequest;
use rest_bridge::publish_message;
use rest_bridge::publish_message_to_group;
use shared::with_channels;
use shared::Channels;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use utoipa::OpenApi;
use utoipa_swagger_ui::Config;
use warp::Filter;

mod auth_helpers;
mod auth_schemas;
mod mailer;
mod message_queue_helpers;
mod prom_helpers;
mod requests;
mod responses;
mod rest_bridge;
mod shared;
use crate::mailer::send_email;

// Swagger configuration for the REST endpoints
#[derive(OpenApi)]
#[openapi(
    paths(publish_message, publish_message_to_group, send_email),
    components(
        schemas(PublishRequest, EmailRequest)
    ),
    modifiers(&SecurityAddon),
)]
struct ApiDoc;

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

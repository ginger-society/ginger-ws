use crate::mailer::__path_send_email;
use crate::miss_consumer::start_miss_consumer;
use crate::rest_bridge::{__path_publish_message, __path_publish_message_userland, __path_publish_message_to_group_api_land, __path_publish_message_to_group};
use crate::shared::{PendingCall, RabbitPool, RabbitPoolRef, connect_redis_pubsub_pool, publish_to_rabbitmq, start_pending_call_expiry_watcher, start_redis_pubsub_bridge, with_rabbit};

use auth_helpers::{
    handle_ws_upgrade, user_authenticated, with_api_auth, with_auth,
    with_get_api_auth_header, with_get_isc_auth_header,
    with_get_auth_header, with_isc_api_auth,
};
use auth_schemas::SecurityAddon;

use message_queue_helpers::consume_messages;
use prom_helpers::{metrics_handler, REGISTRY, REQUEST_COUNTER};
// Renaming lapin::Channel to RabbitChannel
use requests::EmailRequest;
use requests::PublishRequest;
use requests::PublishType;
use rest_bridge::publish_message;
use rest_bridge::publish_message_to_group;
use rest_bridge::publish_message_to_group_api_land;
use rest_bridge::publish_message_userland;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use shared::{with_channels, with_connections, with_redis, connect_redis, Channels, Connections, RedisPool, start_broker_heartbeat,};

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
mod miss_consumer;
use crate::mailer::send_email;

// Swagger configuration for the REST endpoints
#[derive(OpenApi)]
#[openapi(
    paths(publish_message,publish_message_userland, publish_message_to_group, publish_message_to_group_api_land, send_email),
    components(
        schemas(PublishRequest, EmailRequest, PublishType)
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
    let channels: Channels    = Arc::new(Mutex::new(HashMap::new()));
    let connections: Connections = Arc::new(Mutex::new(HashMap::new()));

    // Redis — pending calls
    let redis: RedisPool = connect_redis().await;

    // Redis — pub/sub fan-out
    let redis_pubsub: RedisPool = connect_redis_pubsub_pool().await;

    // RabbitMQ — persistent publish pool
    let rabbit_pool: RabbitPoolRef = Arc::new(RabbitPool::new().await);

    // Using a UUID means each pod/process is uniquely identified in Redis.
    let broker_id = std::env::var("BROKER_ID")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
 
    println!("[broker] starting as broker_id={}", broker_id);

    start_broker_heartbeat(redis.clone(), broker_id.clone()).await;

    // start Redis pub/sub bridge
    // listens on pushkar-redis, delivers to local broadcast::Sender
    start_redis_pubsub_bridge(channels.clone()).await;
    start_pending_call_expiry_watcher(rabbit_pool.clone(), redis.clone()).await;
    
    // start RabbitMQ consumer
    // exclusive queue per instance, fanout delivers to all instances
    let channels_mq = channels.clone();
    let broker_id_mq = broker_id.clone();
    let rabbit_mq = rabbit_pool.clone();
    tokio::spawn(async move {
        consume_messages(channels_mq, broker_id_mq, rabbit_mq).await;
    });

    start_miss_consumer(channels.clone(), redis.clone(), rabbit_pool.clone()).await;


    let broker_id_ws = broker_id.clone();
    let channels_ws     = channels.clone();
    let connections_ws  = connections.clone();
    let redis_ws        = redis.clone();
    let redis_pubsub_ws = redis_pubsub.clone();
    let rabbit_ws       = rabbit_pool.clone();

    let websocket_route = warp::path("notification")
        .and(warp::path("ws"))
        .and(warp::path::param::<String>())
        .and(warp::ws())
        .and(warp::query::<HashMap<String, String>>())
        .and(with_channels(channels_ws))
        .and(with_connections(connections_ws))
        .and(with_redis(redis_ws))
        .and(with_redis(redis_pubsub_ws))
        .and(with_rabbit(rabbit_ws))
        // ── inject broker_id into every WS request ──
        .and(warp::any().map(move || broker_id_ws.clone()))
        .and_then(
            |channel_name,
             ws,
             query_params: HashMap<String, String>,
             channels,
             connections,
             redis,
             redis_pubsub,
             rabbit_pool,
             broker_id: String| {           // NEW param
                let token = query_params.get("token").cloned();
                user_authenticated(
                    channel_name, ws, channels, connections,
                    redis, redis_pubsub, rabbit_pool, token,
                    broker_id,              // NEW arg
                )
            },
        )
        .and_then(handle_ws_upgrade);


    // ── publish via ISC ───────────────────────────────────────────────────────
    let channels_rest = channels.clone();
    let rabbit_rest = rabbit_pool.clone();
    let publish_via_isc_route = warp::path("notification")
        .and(warp::path!("channels" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_isc_api_auth())
        .and(with_channels(channels_rest))
        .and(with_rabbit(rabbit_rest))
        .and_then(publish_message);

    // ── user-land publish ─────────────────────────────────────────────────────
    let channels_rest = channels.clone();
    let rabbit_rest = rabbit_pool.clone();
    let publish_route = warp::path("notification")
        .and(warp::path!("user-land" / "channels" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_auth())
        .and(with_get_auth_header())
        .and(with_channels(channels_rest))
        .and(with_rabbit(rabbit_rest))
        .and_then(publish_message_userland);

    // ── group publish ISC ─────────────────────────────────────────────────────
    let channels_rest = channels.clone();
    let rabbit_rest = rabbit_pool.clone();
    let group_publish_route_isc = warp::path("notification")
        .and(warp::path!("groups" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_isc_api_auth())
        .and(with_get_isc_auth_header())
        .and(with_channels(channels_rest))
        .and(with_rabbit(rabbit_rest))
        .and_then(publish_message_to_group);

    // ── group publish API land ────────────────────────────────────────────────
    let channels_rest = channels.clone();
    let rabbit_rest = rabbit_pool.clone();
    let group_publish_route = warp::path("notification")
        .and(warp::path!("api-land" / "groups" / String / "publish"))
        .and(warp::post())
        .and(warp::body::json())
        .and(with_api_auth())
        .and(with_get_api_auth_header())
        .and(with_channels(channels_rest))
        .and(with_rabbit(rabbit_rest))
        .and_then(publish_message_to_group_api_land);

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
        .or(publish_via_isc_route)
        .or(group_publish_route_isc)
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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
pub enum PublishType {
    Group,
    Members,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct PublishRequest {
    pub message: String,
    pub prefix: String,
    pub pubType: PublishType,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct EmailRequest {
    pub message: String,
    pub to: String,
    pub reply_to: Option<String>,
    pub subject: String,
}

#[derive(Deserialize, Serialize)]
pub struct RabbitMessage {
    pub channel_id: String,
    pub message: String,
}

// ── WAMP-style framing ────────────────────────────────────────────────────────

/// PUBLISH [16, request_id, options, topic, args?, kwargs?]
/// Sent by a client over WebSocket to publish to a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampPublish {
    pub message_type: u8,         // always 16
    pub request_id: u64,
    pub options: WampPublishOptions,
    pub topic: String,            // the channel name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwargs: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampPublishOptions {
    #[serde(default)]
    pub acknowledge: bool,
    /// correlation_id for RPC-style pub/sub — caller sets this,
    /// callee echoes it back in their reply so caller can correlate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// reply_to — callee publishes result back to this channel
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// EVENT [36, subscription_id, publication_id, details, args?, kwargs?]
/// What all subscribers on a channel receive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampEvent {
    pub message_type: u8,         // always 36
    pub subscription_id: String,  // channel name
    pub publication_id: u64,
    pub details: WampEventDetails,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwargs: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampEventDetails {
    pub timestamp: String,
    pub topic: String,
    /// Echoed from publish options so callee knows where to send result back
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// Broker → caller: callee was not connected when PUBLISH was sent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampCalleeOffline {
    pub message_type: u8,         // use 0 as a custom broker error type
    pub error: String,            // "callee_offline"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub topic: String,
}

/// Broker → caller: callee disconnected mid-execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WampCalleeDisconnected {
    pub message_type: u8,         // use 0
    pub error: String,            // "callee_disconnected"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl WampEvent {
    pub fn from_publish(publish: &WampPublish, publication_id: u64) -> Self {
        Self {
            message_type: 36,
            subscription_id: publish.topic.clone(),
            publication_id,
            details: WampEventDetails {
                timestamp: chrono::Utc::now().to_rfc3339(),
                topic: publish.topic.clone(),
                correlation_id: publish.options.correlation_id.clone(),
                reply_to: publish.options.reply_to.clone(),
            },
            args: publish.args.clone(),
            kwargs: publish.kwargs.clone(),
        }
    }
}
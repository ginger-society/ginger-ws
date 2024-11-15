use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
pub struct PublishRequest {
    pub message: String, // Only the message is in the body
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

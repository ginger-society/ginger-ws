use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
pub enum PublishType{
    Group,
    Members
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct PublishRequest {
    pub message: String,
    pub prefix: String,
    pub pubType: PublishType
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

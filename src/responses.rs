use warp::reject::Reject;

// Custom JWT Error
#[derive(Debug)]
pub struct JWTError;

impl Reject for JWTError {}

// Define the custom rejection error
#[derive(Debug)]
pub struct InvalidTokenError;
impl Reject for InvalidTokenError {}

#[derive(Debug)]
pub struct EncodeError;
impl Reject for EncodeError {}

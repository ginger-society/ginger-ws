// Custom JWT Error
#[derive(Debug)]
pub struct JWTError;

impl warp::reject::Reject for JWTError {}

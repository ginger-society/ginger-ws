use prometheus::{Encoder, IntCounter, Opts, Registry, TextEncoder};

use crate::responses::EncodeError;

lazy_static::lazy_static! {
    pub static ref REGISTRY: Registry = Registry::new();
}

// Define metrics
lazy_static::lazy_static! {
    pub static ref REQUEST_COUNTER: IntCounter = IntCounter::with_opts(Opts::new("request_count", "Total number of requests"))
        .expect("Counter can be created");
}

pub async fn metrics_handler() -> Result<impl warp::Reply, warp::Rejection> {
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

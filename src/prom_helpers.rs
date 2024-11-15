use prometheus::{Encoder, IntCounter, Opts, Registry, TextEncoder};

lazy_static::lazy_static! {
    pub static ref REGISTRY: Registry = Registry::new();
}

// Define metrics
lazy_static::lazy_static! {
    pub static ref REQUEST_COUNTER: IntCounter = IntCounter::with_opts(Opts::new("request_count", "Total number of requests"))
        .expect("Counter can be created");
}

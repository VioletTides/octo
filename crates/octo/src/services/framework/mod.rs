//! General-purpose building blocks the services share: a sliding-window rate limiter, a
//! size-limited memory cache, single-flight fetches, an insertion-ordered dictionary with slot
//! reuse, and a buffered HTTP call.

pub mod dotnet_dictionary;
pub mod http;
pub mod in_flight;
pub mod memory_cache;
pub mod sliding_window_rate_limiter;

pub use dotnet_dictionary::DotnetDictionary;
pub use http::HttpAnswer;
pub use in_flight::InFlight;
pub use memory_cache::MemoryCache;
pub use sliding_window_rate_limiter::{
    RateLimitLease, SlidingWindowRateLimiter, SlidingWindowRateLimiterOptions,
};

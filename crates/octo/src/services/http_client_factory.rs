//! The default HTTP client, and the words its failures are told in: what happened, in plain
//! words, rather than a library's exception text.
//!
//! The default client: a 100 second timeout covering the whole buffered exchange, no automatic
//! decompression (so no `Accept-Encoding` is sent and a body arrives as the server wrote it), no
//! default request headers, and redirects followed.

use std::error::Error as _;
use std::time::Duration;

/// The default client's timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(100);

/// The default client, for buffered calls: the timeout covers reading the body too.
pub fn default_client() -> reqwest::Client {
    builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .expect("the default HTTP client builds")
}

/// The same client for calls whose body is streamed for as long as it lasts: only the wait for
/// the headers is bounded, which callers apply themselves with [`DEFAULT_TIMEOUT`].
pub fn streaming_client() -> reqwest::Client {
    builder().build().expect("the streaming HTTP client builds")
}

/// The builder every named client starts from: no decompression.
pub fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_gzip().no_deflate()
}

/// What a call that gave up waiting says.
pub fn timeout_message(timeout: Duration) -> String {
    let seconds = timeout.as_secs_f64();
    let shown = if seconds.fract() == 0.0 {
        format!("{}", seconds as u64)
    } else {
        format!("{seconds}")
    };
    let unit = if shown == "1" { "second" } else { "seconds" };
    format!("No answer came within {shown} {unit}, so Octo stopped waiting.")
}

/// What a call answered with a status that is not a success says.
pub fn status_failure_message(status: reqwest::StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("The server answered {} ({reason}).", status.as_u16()),
        None => format!("The server answered {}.", status.as_u16()),
    }
}

/// What a connection that failed says, as far as it can be told from reqwest's error: refused
/// (nothing listening at that address) or a name that could not be looked up, with the address
/// it tried. Anything else gets reqwest's description.
pub fn connect_failure_message(error: &reqwest::Error) -> String {
    let endpoint = error.url().map(|url| {
        format!(
            "{}:{}",
            url.host_str().unwrap_or(""),
            url.port_or_known_default().unwrap_or(0)
        )
    });
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::ConnectionRefused
        {
            return could_not_connect(endpoint.as_deref(), "the connection was refused");
        }
        if cause.to_string().contains("failed to lookup address information") {
            let host = error.url().and_then(|url| url.host_str()).unwrap_or("the server");
            return could_not_connect(endpoint.as_deref(), &format!("no address was found for {host}"));
        }
        source = cause.source();
    }
    error.to_string()
}

fn could_not_connect(endpoint: Option<&str>, why: &str) -> String {
    match endpoint {
        Some(endpoint) => format!("Could not connect to {endpoint}: {why}."),
        None => format!("Could not connect: {why}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_says_how_long_octo_waited() {
        assert_eq!(
            timeout_message(DEFAULT_TIMEOUT),
            "No answer came within 100 seconds, so Octo stopped waiting."
        );
        assert_eq!(
            timeout_message(Duration::from_millis(300)),
            "No answer came within 0.3 seconds, so Octo stopped waiting."
        );
        assert_eq!(
            timeout_message(Duration::from_secs(1)),
            "No answer came within 1 second, so Octo stopped waiting."
        );
    }

    #[test]
    fn a_status_that_is_not_a_success_names_the_status() {
        assert_eq!(
            status_failure_message(reqwest::StatusCode::NOT_FOUND),
            "The server answered 404 (Not Found)."
        );
        assert_eq!(
            status_failure_message(reqwest::StatusCode::from_u16(599).expect("a status")),
            "The server answered 599."
        );
    }

    #[tokio::test]
    async fn a_refused_connection_names_the_endpoint() {
        let address = crate::services::test_support::refused_address();
        let error = default_client()
            .get(format!("http://{address}/x"))
            .send()
            .await
            .expect_err("nothing listens there");
        assert_eq!(
            connect_failure_message(&error),
            format!("Could not connect to {address}: the connection was refused.")
        );
    }
}

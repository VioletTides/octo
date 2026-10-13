//! A buffered HTTP call: the whole body is read before the call returns, inside the client's
//! timeout. [`HttpAnswer`] is that buffered response, and the helpers here read it as text or
//! JSON.

use bytes::Bytes;
use reqwest::StatusCode;
use serde_json::Value;

/// A response with its body read.
#[derive(Debug, Clone)]
pub struct HttpAnswer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl HttpAnswer {
    /// Reads the whole body of a response.
    pub async fn read(response: reqwest::Response) -> reqwest::Result<HttpAnswer> {
        let status = response.status();
        let body = response.bytes().await?;
        Ok(HttpAnswer { status, body })
    }

    /// An answer the client made up itself, with no body (the limiters' 429).
    pub fn empty(status: StatusCode) -> HttpAnswer {
        HttpAnswer {
            status,
            body: Bytes::new(),
        }
    }

    /// A success status: 200-299.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// The body as UTF-8 text, a byte order mark dropped, bad bytes replaced.
    pub fn text(&self) -> String {
        let body = self.body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&self.body);
        String::from_utf8_lossy(body).into_owned()
    }

    /// The body parsed as JSON.
    pub fn json(&self) -> serde_json::Result<Value> {
        parse_json(&self.body)
    }
}

/// JSON parsed from bytes: a UTF-8 byte order mark is skipped.
pub fn parse_json(bytes: &[u8]) -> serde_json::Result<Value> {
    serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes))
}

/// A client builder with no automatic decompression (so no Accept-Encoding header); only the
/// AcoustID client turns it on.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_gzip().no_deflate()
}

/// A relative request resolved against a client's base address.
pub fn resolve(base: &url::Url, relative: &str) -> Result<url::Url, url::ParseError> {
    base.join(relative)
}

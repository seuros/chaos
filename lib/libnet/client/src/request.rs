use bytes::Bytes;
use rama::http::HeaderMap;
use rama::http::Method;
use rama::http::StatusCode;
use serde_json::Value;
use std::time::Duration;

use crate::error::TransportError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestCompression {
    #[default]
    None,
    Zstd,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Value>,
    pub compression: RequestCompression,
    pub timeout: Option<Duration>,
}

impl Request {
    pub fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
            compression: RequestCompression::None,
            timeout: None,
        }
    }

    /// Attach an already-serialized JSON value as the request body.
    pub fn with_json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn with_compression(mut self, compression: RequestCompression) -> Self {
        self.compression = compression;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub(crate) fn into_result(self, url: String) -> Result<Self, TransportError> {
        if self.status.is_success() {
            Ok(self)
        } else {
            Err(TransportError::Http {
                status: self.status,
                url: Some(url),
                headers: Some(self.headers),
                body: Some(String::from_utf8_lossy(&self.body).into_owned()),
            })
        }
    }
}

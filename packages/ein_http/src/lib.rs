// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Ergonomic HTTP client for `wasm32-wasip2` WASM components.
//!
//! Wraps [`wstd::http`] with a reqwest-style builder API. HTTP calls are
//! dispatched through `wasi:http/outgoing-handler` — the standard WASM
//! component HTTP interface — which the host satisfies via `wasmtime-wasi-http`.
//!
//! # Example
//!
//! ```rust,ignore
//! let resp = HttpRequest::post("https://api.example.com/v1/chat/completions")
//!     .bearer_auth(&api_key)
//!     .json(&body)?
//!     .send()?;
//!
//! if resp.is_success() {
//!     let value: MyType = resp.json()?;
//! }
//! ```

use anyhow::anyhow;
use serde::Serialize;
use std::collections::HashMap;
use wstd::http::{Body, Client, Request, Uri};
use wstd::runtime::block_on;

/// Returned by [`HttpRequest::send`] when the host allowlist blocks the
/// request (`ErrorCode::HttpRequestDenied` from `wasi:http/outgoing-handler`).
///
/// Callers can detect this with `err.is::<RequestDeniedError>()` or
/// `err.downcast_ref::<RequestDeniedError>()`.
#[derive(Debug)]
pub struct RequestDeniedError;

impl std::fmt::Display for RequestDeniedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HTTP request blocked by host allowlist (HttpRequestDenied)")
    }
}

impl std::error::Error for RequestDeniedError {}

pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// A pending HTTP request. Construct with one of the method helpers
/// ([`HttpRequest::get`], [`HttpRequest::post`], etc.) then chain builder
/// methods and call [`HttpRequest::send`] to dispatch it.
pub struct HttpRequest {
    method: HttpMethod,
    url: String,
    headers: HashMap<String, String>,
    body: String,
}

impl HttpRequest {
    fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HashMap::new(),
            body: String::new(),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Post, url)
    }

    pub fn put(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Put, url)
    }

    pub fn patch(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Patch, url)
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Delete, url)
    }

    /// Add an arbitrary header.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Add a `Content-Type: application/json` header.
    pub fn content_type_json(self) -> Self {
        self.header("Content-Type", "application/json")
    }

    /// Add an `Authorization: Bearer <token>` header.
    pub fn bearer_auth(self, token: impl Into<String>) -> Self {
        self.header("Authorization", format!("Bearer {}", token.into()))
    }

    /// Serialize `value` as JSON, set `Content-Type: application/json`, and
    /// use the result as the request body.
    pub fn json<T: Serialize>(mut self, value: &T) -> anyhow::Result<Self> {
        self.body = serde_json::to_string(value)?;
        Ok(self.content_type_json())
    }

    /// Set a raw string body without changing headers.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Dispatch the request via `wasi:http/outgoing-handler` and return the
    /// parsed [`HttpResponse`].
    ///
    /// Uses [`wstd::runtime::block_on`] to drive the async wstd client to
    /// completion from this synchronous context.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid, the request cannot be built,
    /// or the transport fails. Does **not** treat non-2xx status codes as
    /// errors — check [`HttpResponse::is_success`] or [`HttpResponse::status`].
    pub fn send(self) -> anyhow::Result<HttpResponse> {
        block_on(async {
            let uri: Uri = self
                .url
                .parse()
                .map_err(|e| anyhow!("invalid URL '{url}': {e}", url = self.url))?;

            let mut builder = match self.method {
                HttpMethod::Get => Request::get(uri),
                HttpMethod::Post => Request::post(uri),
                HttpMethod::Put => Request::put(uri),
                HttpMethod::Patch => Request::patch(uri),
                HttpMethod::Delete => Request::delete(uri),
            };

            for (key, value) in &self.headers {
                builder = builder.header(key.as_str(), value.as_str());
            }

            let request = builder
                .body(Body::from(self.body))
                .map_err(|e| anyhow!("failed to build request: {e}"))?;

            let response = Client::new().send(request).await.map_err(|e| {
                if e.to_string().contains("HttpRequestDenied") {
                    anyhow::Error::new(RequestDeniedError)
                } else {
                    anyhow!("HTTP request failed: {e}")
                }
            })?;

            let status = response.status().as_u16();
            let body_bytes = response
                .into_body()
                .str_contents()
                .await
                .map_err(|e| anyhow!("failed to read response body: {e}"))?
                .to_string();

            Ok(HttpResponse {
                status,
                body: body_bytes,
            })
        })
    }
}

/// An HTTP response returned by [`HttpRequest::send`].
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body as a UTF-8 string.
    pub body: String,
}

impl HttpResponse {
    /// Returns `true` for 2xx status codes.
    pub fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Deserialise the response body as JSON into `T`.
    pub fn json<T: for<'de> serde::Deserialize<'de>>(&self) -> anyhow::Result<T> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

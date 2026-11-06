//! Thin HTTP client over the lineup REST API.

use std::fmt;

use api_types::ErrorBody;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub enum CliError {
    /// API error with its stable code (not_found, conflict, invalid, internal).
    Api {
        code: String,
        message: String,
    },
    /// Could not reach the server.
    Unreachable(String),
    /// Bad CLI usage detected after parsing (ambiguous reference, bad value).
    Usage(String),
    Other(String),
    /// Errors were already printed (batch commands); exit with this code.
    Reported(i32),
}

impl CliError {
    pub fn code(&self) -> &str {
        match self {
            CliError::Api { code, .. } => code,
            CliError::Unreachable(_) => "unreachable",
            CliError::Usage(_) => "usage",
            CliError::Other(_) | CliError::Reported(_) => "error",
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Api { message, .. } => write!(f, "{message}"),
            CliError::Unreachable(m) | CliError::Usage(m) | CliError::Other(m) => write!(f, "{m}"),
            CliError::Reported(code) => write!(f, "failed (exit {code})"),
        }
    }
}

impl std::error::Error for CliError {}

/// Exit codes: 0 ok, 1 generic, 2 usage/invalid, 3 not found, 4 conflict, 5 server unreachable.
pub fn exit_code(e: &CliError) -> i32 {
    match e {
        CliError::Api { code, .. } => match code.as_str() {
            "not_found" => 3,
            "conflict" => 4,
            "invalid" => 2,
            _ => 1,
        },
        CliError::Unreachable(_) => 5,
        CliError::Usage(_) => 2,
        CliError::Other(_) => 1,
        CliError::Reported(code) => *code,
    }
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(server: &str) -> Result<Self, CliError> {
        match url::Url::parse(server) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
            _ => {
                return Err(CliError::Usage(format!(
                    "invalid server URL {server:?} (expected http://host:port)"
                )));
            }
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| CliError::Other(e.to_string()))?;
        Ok(Client {
            base: server.trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// GET returning raw bytes (e.g. the database backup).
    pub async fn get_bytes(&self, path: &str) -> Result<Vec<u8>, CliError> {
        let res = self
            .http
            .get(self.url(path))
            .send()
            .await
            .map_err(|e| CliError::Unreachable(format!("cannot reach lineup server at {}: {e}", self.base)))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| CliError::Other(format!("reading response: {e}")))?;
        if !status.is_success() {
            return Err(match serde_json::from_slice::<ErrorBody>(&bytes) {
                Ok(b) => CliError::Api {
                    code: b.error.code,
                    message: b.error.message,
                },
                Err(_) => CliError::Api {
                    code: "internal".into(),
                    message: format!("{path}: HTTP {status}"),
                },
            });
        }
        Ok(bytes.to_vec())
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        self.send(Method::GET, path, None::<&()>).await
    }

    pub async fn get_query<T: DeserializeOwned, Q: Serialize>(&self, path: &str, q: &Q) -> Result<T, CliError> {
        let req = self.http.get(self.url(path)).query(q);
        self.finish(path, req).await
    }

    pub async fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T, CliError> {
        self.send(Method::POST, path, Some(body)).await
    }

    /// POST without the default timeout, for requests that wait on slow work (EPG downloads).
    pub async fn post_slow<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T, CliError> {
        let req = self
            .http
            .post(self.url(path))
            .json(body)
            .timeout(std::time::Duration::from_secs(60 * 60));
        self.finish(path, req).await
    }

    pub async fn patch<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T, CliError> {
        self.send(Method::PATCH, path, Some(body)).await
    }

    pub async fn put<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T, CliError> {
        self.send(Method::PUT, path, Some(body)).await
    }

    /// DELETE; tolerates 204 by deserialising `()`-like targets from `null`.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        self.send(Method::DELETE, path, None::<&()>).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    async fn send<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, CliError> {
        let mut req = self.http.request(method, self.url(path));
        if let Some(b) = body {
            req = req.json(b);
        }
        self.finish(path, req).await
    }

    async fn finish<T: DeserializeOwned>(&self, path: &str, req: reqwest::RequestBuilder) -> Result<T, CliError> {
        let res = req
            .send()
            .await
            .map_err(|e| CliError::Unreachable(format!("cannot reach lineup server at {}: {e}", self.base)))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| CliError::Other(format!("reading response: {e}")))?;
        if !status.is_success() {
            return Err(match serde_json::from_slice::<ErrorBody>(&bytes) {
                Ok(b) => CliError::Api {
                    code: b.error.code,
                    message: b.error.message,
                },
                Err(_) => CliError::Api {
                    code: if status == StatusCode::NOT_FOUND {
                        "not_found".into()
                    } else {
                        "internal".into()
                    },
                    message: format!("{path}: HTTP {status}"),
                },
            });
        }
        let bytes = if bytes.is_empty() { &b"null"[..] } else { &bytes[..] };
        serde_json::from_slice(bytes).map_err(|e| CliError::Other(format!("unexpected response from {path}: {e}")))
    }
}

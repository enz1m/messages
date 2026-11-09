use std::time::Duration;

use reqwest::{Client, Method, Response};
use serde_json::Value;
use url::Url;

use crate::AdapterError;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Bounded, timeout-aware HTTP shared by all adapters. Debug never includes an
/// authorization header or URL query containing a credential.
#[derive(Clone)]
pub struct Http {
    pub(crate) client: Client,
    base: Url,
    authorization: Option<String>,
}

impl Http {
    pub fn new(endpoint: &str, authorization: Option<String>) -> Result<Self, AdapterError> {
        let base = Url::parse(endpoint)
            .map_err(|_| AdapterError::Configuration("Enter a valid server URL.".into()))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(AdapterError::Configuration(
                "Use an HTTP(S) server URL without credentials, query or fragment.".into(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(35))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("enz-Messages/0.1.0")
            .build()
            .map_err(|_| AdapterError::Network)?;
        Ok(Self {
            client,
            base,
            authorization,
        })
    }

    pub fn url(&self, segments: &[&str]) -> Result<Url, AdapterError> {
        let mut url = self.base.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| AdapterError::Configuration("Invalid server URL.".into()))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    pub(crate) fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.request(method, url);
        if let Some(authorization) = &self.authorization {
            request.header(reqwest::header::AUTHORIZATION, authorization)
        } else {
            request
        }
    }

    pub async fn json(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, AdapterError> {
        let mut request = self.request(method, self.url(segments)?).query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| AdapterError::Network)?;
        Self::decode(response).await
    }

    pub(crate) fn check_status(response: &Response) -> Result<(), AdapterError> {
        let status = response.status().as_u16();
        if matches!(status, 401 | 403) {
            return Err(AdapterError::Authentication);
        }
        if status == 429 {
            let seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(5.0)
                .min(3600.0);
            return Err(AdapterError::RateLimited(Duration::from_secs_f64(seconds)));
        }
        if !(200..300).contains(&status) {
            return Err(AdapterError::Http(status));
        }
        Ok(())
    }

    pub(crate) async fn decode(response: Response) -> Result<Value, AdapterError> {
        Self::decode_limited(response, MAX_RESPONSE_BYTES).await
    }

    pub(crate) async fn decode_limited(
        response: Response,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        Self::check_status(&response)?;
        Self::payload_limited(response, limit).await
    }

    pub(crate) async fn payload_limited(
        mut response: Response,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(AdapterError::InvalidData(
                "response exceeds the size limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| AdapterError::Network)? {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(AdapterError::InvalidData(
                    "response exceeds the size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        if bytes.len() > 1024 * 1024 {
            tokio::task::spawn_blocking(move || serde_json::from_slice(&bytes))
                .await
                .map_err(|_| AdapterError::Network)?
                .map_err(|_| AdapterError::InvalidData("invalid JSON".into()))
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| AdapterError::InvalidData("invalid JSON".into()))
        }
    }
}

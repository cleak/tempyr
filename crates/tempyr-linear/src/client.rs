use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{LinearError, Result};

const DEFAULT_ENDPOINT: &str = "https://api.linear.app/graphql";

/// Low-level GraphQL client for the Linear API.
pub struct LinearClient {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl LinearClient {
    pub fn new(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            endpoint: DEFAULT_ENDPOINT.to_string(),
        }
    }

    /// Create a client from the `LINEAR_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("LINEAR_API_KEY").map_err(|_| {
            LinearError::Config(
                "LINEAR_API_KEY environment variable not set. \
                 Get one at https://linear.app/settings/api"
                    .to_string(),
            )
        })?;
        Ok(Self::new(api_key))
    }

    /// Execute a GraphQL query/mutation and deserialize the `data` field.
    pub async fn execute<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let body = json!({
            "query": query,
            "variables": variables,
        });

        let response = self.raw_post(&body).await?;

        // Check for GraphQL-level errors
        if let Some(errors) = response.get("errors") {
            let messages: Vec<String> = errors
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .map(String::from)
                .collect();
            return Err(LinearError::GraphQL(messages.join("; ")));
        }

        let data = response
            .get("data")
            .ok_or_else(|| LinearError::GraphQL("No 'data' field in response".to_string()))?;

        serde_json::from_value(data.clone())
            .map_err(|e| LinearError::GraphQL(format!("Failed to deserialize response: {e}")))
    }

    /// Send a raw POST request to the GraphQL endpoint.
    /// Handles HTTP errors and rate limiting with one automatic retry.
    async fn raw_post(&self, body: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        let status = resp.status();

        // Rate limited — extract retry delay and retry once
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_ms = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|secs| secs * 1000)
                .unwrap_or(5000);

            tokio::time::sleep(std::time::Duration::from_millis(retry_ms)).await;

            let retry_resp = self
                .http
                .post(&self.endpoint)
                .header("Authorization", &self.api_key)
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await?;

            let retry_status = retry_resp.status();
            if retry_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LinearError::RateLimited {
                    retry_after_ms: retry_ms,
                });
            }

            if !retry_status.is_success() {
                let text = retry_resp.text().await.unwrap_or_default();
                return Err(LinearError::GraphQL(format!(
                    "Linear API error {retry_status}: {text}"
                )));
            }

            return retry_resp.json().await.map_err(LinearError::Http);
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LinearError::GraphQL(format!(
                "Linear API error {status}: {text}"
            )));
        }

        resp.json().await.map_err(LinearError::Http)
    }
}

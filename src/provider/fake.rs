//! A deterministic, offline `Provider` for tests. Scripted with a queue of
//! responses; each `complete` call pops the next one and records the request it
//! was given so tests can assert on what the loop sent.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{ModelRequest, ModelResponse, Provider};
use crate::error::ProviderError;

/// A provider that replays a fixed script of responses.
pub struct FakeProvider {
    responses: Mutex<VecDeque<Result<ModelResponse, ProviderError>>>,
    seen: Arc<Mutex<Vec<ModelRequest>>>,
}

impl FakeProvider {
    /// Build from an ordered script of responses (one consumed per call).
    pub fn new(responses: Vec<Result<ModelResponse, ProviderError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The requests this provider has received so far, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().expect("not poisoned").clone()
    }

    /// A shared handle to the recorded requests, so a test can inspect them
    /// after the provider has been moved into a `Mind`/`Brainstem`.
    pub fn requests_handle(&self) -> Arc<Mutex<Vec<ModelRequest>>> {
        Arc::clone(&self.seen)
    }
}

#[async_trait]
impl Provider for FakeProvider {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.seen
            .lock()
            .expect("not poisoned")
            .push(request.clone());
        self.responses
            .lock()
            .expect("not poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                Err(ProviderError::Api {
                    status: 500,
                    body: "FakeProvider script exhausted".into(),
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replays_in_order_and_records_requests() {
        let provider = FakeProvider::new(vec![
            Ok(ModelResponse::text("first")),
            Ok(ModelResponse::text("second")),
        ]);
        let req = ModelRequest {
            system: "sys".into(),
            messages: Vec::new(),
            tools: Vec::new(),
        };

        assert_eq!(
            provider.complete(&req).await.unwrap().text.as_deref(),
            Some("first")
        );
        assert_eq!(
            provider.complete(&req).await.unwrap().text.as_deref(),
            Some("second")
        );
        assert_eq!(provider.requests().len(), 2);
    }

    #[tokio::test]
    async fn exhausted_script_errors() {
        let provider = FakeProvider::new(vec![]);
        let req = ModelRequest {
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
        };
        assert!(provider.complete(&req).await.is_err());
    }
}

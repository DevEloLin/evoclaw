//! Azure provider — wraps `OpenAiCompatProvider` with Azure-specific URL
//! layout, header-based auth (`api-key`) and the required `api-version`
//! query parameter.
//!
//! Supports two endpoint shapes:
//!
//! 1. **Azure OpenAI** — `https://{resource}.openai.azure.com`
//!    Wire URL: `…/openai/deployments/{deployment}/chat/completions?api-version=…`
//!    Auth: `api-key: <key>` header
//!    Model field in body = deployment name
//!
//! 2. **Azure AI Inference** — `https://{resource}.services.ai.azure.com/models`
//!    Wire URL: `…/chat/completions?api-version=…`
//!    Auth: `api-key: <key>` (also accepts Bearer; we use api-key for consistency)
//!    Model field in body = actual model id
//!
//! Detection: if the base URL already contains `/openai/deployments/`, we
//! treat it as a fully-resolved Azure OpenAI deployment URL. If the host
//! ends with `.openai.azure.com`, the deployment name is appended from the
//! `model` argument. Otherwise the URL is used verbatim (Inference shape).

use crate::{ChatRequest, OpenAiCompatProvider, Provider, ProviderError, StreamEvent};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// Default Azure REST API version (GA-aligned, supports tool calling).
pub const DEFAULT_API_VERSION: &str = "2024-08-01-preview";

#[derive(Debug, Clone)]
pub struct AzureProvider {
    inner: OpenAiCompatProvider,
    pub model: String,
}

impl AzureProvider {
    /// Construct a new Azure provider.
    ///
    /// `base_url` is the resource URL or fully-qualified deployment URL.
    /// `model` is the deployment name (Azure OpenAI) or the model ID
    /// (Azure AI Inference). `api_version` defaults to
    /// [`DEFAULT_API_VERSION`] when `None`.
    pub fn new(
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        api_version: Option<&str>,
    ) -> Self {
        let url = base_url.as_ref().trim_end_matches('/');
        let model_str: String = model.into();
        let base = resolve_base_url(url, &model_str);
        let version = api_version.unwrap_or(DEFAULT_API_VERSION);
        let inner = OpenAiCompatProvider::new(base, api_key, &model_str)
            .with_header_auth("api-key")
            .with_query("api-version", version);
        Self {
            inner,
            model: model_str,
        }
    }
}

/// Compute the base URL whose `/chat/completions` suffix yields the right
/// Azure wire URL. See the module docs for the three shapes recognised.
fn resolve_base_url(url: &str, deployment_or_model: &str) -> String {
    if url.contains("/openai/deployments/") {
        // Fully resolved (user pasted the deployment path).
        url.to_string()
    } else if url.contains(".openai.azure.com") {
        // Resource root for Azure OpenAI — append the deployment path.
        format!("{url}/openai/deployments/{deployment_or_model}")
    } else {
        // Assume Azure AI Inference (`.services.ai.azure.com/models`) or a
        // custom path the user has wired up themselves.
        url.to_string()
    }
}

#[async_trait]
impl Provider for AzureProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        self.inner.stream(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_root_gets_deployment_appended() {
        let base = resolve_base_url("https://myrsc.openai.azure.com", "gpt-4o");
        assert_eq!(base, "https://myrsc.openai.azure.com/openai/deployments/gpt-4o");
    }

    #[test]
    fn fully_resolved_deployment_url_unchanged() {
        let base = resolve_base_url(
            "https://myrsc.openai.azure.com/openai/deployments/gpt-4o",
            "ignored",
        );
        assert_eq!(base, "https://myrsc.openai.azure.com/openai/deployments/gpt-4o");
    }

    #[test]
    fn inference_endpoint_unchanged() {
        let base = resolve_base_url(
            "https://myrsc.services.ai.azure.com/models",
            "Mistral-large",
        );
        assert_eq!(base, "https://myrsc.services.ai.azure.com/models");
    }

    #[test]
    fn default_api_version_constant() {
        assert!(DEFAULT_API_VERSION.starts_with("2024-"));
    }

    #[test]
    fn provider_constructs_with_deployment_name_as_model() {
        let p = AzureProvider::new(
            "https://myrsc.openai.azure.com",
            "secret-key",
            "gpt-4o",
            None,
        );
        assert_eq!(p.model, "gpt-4o");
    }
}

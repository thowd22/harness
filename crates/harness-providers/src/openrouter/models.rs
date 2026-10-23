//! The OpenRouter model catalog (`GET /models`), used for capabilities and
//! for rejecting models that cannot call tools.

use serde_json::Value;

use crate::model::Capabilities;
use crate::provider::ProviderError;

/// What the catalog says about one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// OpenRouter model id, e.g. `anthropic/claude-sonnet-4`.
    pub id: String,
    /// Context window in tokens (0 if unknown).
    pub context_length: u32,
    /// Maximum completion tokens of the top provider (0 if unknown).
    pub max_completion_tokens: u32,
    /// `tools` is among the supported parameters.
    pub supports_tools: bool,
    /// `reasoning` is among the supported parameters.
    pub supports_reasoning: bool,
    /// Image input is supported.
    pub supports_images: bool,
}

impl ModelInfo {
    /// Parses one entry of the catalog's `data` array.
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id")?.as_str()?.to_owned();
        let params: Vec<&str> = v["supported_parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let has = |p: &str| params.contains(&p);
        let as_u32 = |p: &str| {
            v.pointer(p)
                .and_then(Value::as_u64)
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
        };
        Some(Self {
            id,
            context_length: as_u32("/context_length"),
            max_completion_tokens: as_u32("/top_provider/max_completion_tokens"),
            supports_tools: has("tools"),
            supports_reasoning: has("reasoning") || has("include_reasoning"),
            supports_images: v["architecture"]["input_modalities"]
                .as_array()
                .is_some_and(|m| m.iter().any(|x| x == "image")),
        })
    }

    /// The model's capabilities. `caching` is set for Anthropic models,
    /// whose requests carry explicit cache breakpoints
    /// ([`super::request::uses_cache_breakpoints`]).
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: self.supports_tools,
            parallel_tool_calls: self.supports_tools,
            thinking: self.supports_reasoning,
            caching: super::request::uses_cache_breakpoints(&self.id),
            images: self.supports_images,
            context_window: self.context_length,
            max_output_tokens: self.max_completion_tokens,
        }
    }
}

/// Parses a `GET /models` response body.
pub fn parse_catalog(body: &Value) -> Result<Vec<ModelInfo>, ProviderError> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Decode("OpenRouter /models response has no `data`".into()))?;
    Ok(data.iter().filter_map(ModelInfo::from_json).collect())
}

/// Checks that `info` can serve an agent. Agents that use tools need a
/// model with tool support.
pub fn check_tools(info: &ModelInfo, needs_tools: bool) -> Result<(), ProviderError> {
    if needs_tools && !info.supports_tools {
        return Err(ProviderError::InvalidRequest(format!(
            "OpenRouter model `{}` does not support tool calling, so it cannot run an agent \
             that uses tools. Choose a model that lists tool support on openrouter.ai/models, \
             or remove the tools from this agent.",
            info.id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_catalog_entries() {
        let body = json!({"data": [
            {"id": "a/tools", "context_length": 200000, "supported_parameters": ["tools", "reasoning"],
             "architecture": {"input_modalities": ["text", "image"]}, "top_provider": {"max_completion_tokens": 64000}},
            {"id": "b/plain", "context_length": 8192, "supported_parameters": ["temperature"]},
            {"no_id": true}
        ]});
        let models = parse_catalog(&body).unwrap();
        assert_eq!(models.len(), 2);
        let c = models[0].capabilities();
        assert!(c.tools && c.thinking && c.images);
        assert_eq!((c.context_window, c.max_output_tokens), (200_000, 64_000));
        assert!(!models[1].supports_tools);
        assert!(check_tools(&models[0], true).is_ok());
        assert!(check_tools(&models[1], false).is_ok());
        let err = check_tools(&models[1], true).unwrap_err().to_string();
        assert!(
            err.contains("`b/plain` does not support tool calling"),
            "{err}"
        );
        assert!(parse_catalog(&json!({})).is_err());
        assert!(!c.caching);
        let claude = ModelInfo::from_json(&json!({"id": "anthropic/claude-haiku-5.5"})).unwrap();
        assert!(claude.capabilities().caching);
    }
}

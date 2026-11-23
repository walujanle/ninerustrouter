//! Embeddings core plus the embedding-provider adapters.
//!
//! Providers kept (per `docs/MODALITIES.md`): **mistral**, **nvidia**,
//! **openrouter**. All three use the OpenAI-compatible adapter; there is no
//! dynamic-node branch. The whole adapter map collapses to one
//! `embeddingConfig` read out of the registry, which is why there is no trait
//! here: one implementation is not an abstraction.

use serde_json::{Map, Value, json};

use crate::credentials::Credentials;
use crate::executors::http::ProxyOptions;
use crate::modalities::{
    ModalityBody, ModalityError, ModalityHttp, ModalityResponse, headers_from_value,
    provider_config, upstream_error_message,
};
use crate::runtime_config::{FETCH_CONNECT_TIMEOUT_MS, http_status};

/// The providers whose `embeddingConfig` this core serves.
const SUPPORTED: [&str; 3] = ["mistral", "nvidia", "openrouter"];

/// `INPUT_TYPE_PARAM` — the registry `models[].params` entry NVIDIA's asymmetric
/// embedding models declare. Without it the upstream answers 400.
const INPUT_TYPE_PARAM: &str = "input_type";
/// `INPUT_TYPE_VALUES`: anything else is rejected upstream.
const INPUT_TYPE_VALUES: [&str; 2] = ["query", "passage"];
/// `DEFAULT_INPUT_TYPE`: read-time usage is the common case.
const DEFAULT_INPUT_TYPE: &str = "query";

/// `resolveInputType(declaredParams, clientValue)`: `None` when the model
/// declares no `input_type`, so every other provider's payload stays
/// byte-identical.
fn resolve_input_type(declared: &[String], client_value: Option<&Value>) -> Option<&'static str> {
    if !declared.iter().any(|p| p == INPUT_TYPE_PARAM) {
        return None;
    }
    let raw = client_value
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    INPUT_TYPE_VALUES
        .iter()
        .find(|v| **v == raw)
        .copied()
        .or(Some(DEFAULT_INPUT_TYPE))
}

/// `handleEmbeddingsCore({body, modelInfo, credentials, ...})`.
///
/// The 401/403 token-refresh-and-retry half lives in the route layer, because
/// the refresh hook (`getExecutor(provider).refreshCredentials`) is an executor
/// concern and the cores stay free of the executor map. The route layer retries
/// by calling this again with the refreshed credentials.
pub async fn embeddings_core(
    body: &Value,
    provider: &str,
    model: &str,
    credentials: Option<&Credentials>,
    proxy_options: &ProxyOptions,
) -> Result<ModalityResponse, ModalityError> {
    // Validate input.
    let input = body.get("input");
    match input {
        None | Some(Value::Null) => {
            return Err(ModalityError::openai(
                http_status::BAD_REQUEST,
                "Missing required field: input",
            ));
        }
        Some(Value::String(_)) | Some(Value::Array(_)) => {}
        Some(_) => {
            return Err(ModalityError::openai(
                http_status::BAD_REQUEST,
                "input must be a string or array of strings",
            ));
        }
    }
    let input = input.expect("checked above");

    let Some(cfg) = provider_config(provider, "embeddingConfig") else {
        return Err(ModalityError::openai(
            http_status::BAD_REQUEST,
            format!("Provider '{provider}' does not support embeddings."),
        ));
    };
    if !SUPPORTED.contains(&provider) {
        return Err(ModalityError::openai(
            http_status::BAD_REQUEST,
            format!("Provider '{provider}' does not support embeddings."),
        ));
    }

    // `buildUrl` / `buildHeaders` / `buildBody`, then a misconfiguration is a
    // 400 with the reason in it, not an uncaught throw.
    let Some(url) = cfg.get("baseUrl").and_then(Value::as_str) else {
        return Err(ModalityError::openai(
            http_status::BAD_REQUEST,
            format!("[{provider}/{model}] missing embeddingConfig.baseUrl"),
        ));
    };

    let mut headers: Vec<(String, String)> =
        vec![("Content-Type".into(), "application/json".into())];
    if let Some(token) = super::credential_token(credentials) {
        headers.push(("Authorization".into(), format!("Bearer {token}")));
    }
    headers.extend(headers_from_value(cfg.get("headers")));

    let encoding_format = body
        .get("encoding_format")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("float");
    let input_type = {
        let registry = crate::providers::registry::registry();
        let declared = crate::providers::lookup::model_params(registry.alias_for(provider), model);
        resolve_input_type(&declared, body.get(INPUT_TYPE_PARAM))
    };
    let request_body = build_body(
        model,
        input,
        encoding_format,
        body.get("dimensions"),
        input_type,
    );

    let response = ModalityHttp::send(
        "POST",
        url,
        &headers,
        ModalityBody::Json(&request_body),
        proxy_options,
        FETCH_CONNECT_TIMEOUT_MS,
    )
    .await
    .map_err(|e| {
        ModalityError::openai(
            http_status::BAD_GATEWAY,
            crate::utils::error::format_provider_error(None, e.message(), None, None),
        )
    })?;

    if !response.ok() {
        let message = upstream_error_message(&response);
        return Err(ModalityError::openai(
            response.status,
            crate::utils::error::format_provider_error(None, &message, None, None),
        ));
    }

    // The OpenAI-compatible adapter's `normalize` is the identity function, but
    // the response is echoed back and `usage` rides along for the billing hook.
    let normalized: Value = response.json().map_err(|_| {
        ModalityError::openai(
            http_status::BAD_GATEWAY,
            format!("Invalid JSON response from {provider}"),
        )
    })?;
    let usage = normalized
        .get("usage")
        .filter(|u| crate::translator::concerns::primitives::js_truthy(u))
        .cloned();
    Ok(ModalityResponse::json_with_usage(normalized, usage))
}

/// The OpenAI-compatible `buildBody`.
///
/// `encoding_format` is always present (`body.encoding_format || "float"`), and
/// `dimensions` is only added when it parses as a finite number above zero —
/// `dimensions: ""` and `dimensions: 0` are both dropped.
fn build_body(
    model: &str,
    input: &Value,
    encoding_format: &str,
    dimensions: Option<&Value>,
    input_type: Option<&str>,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("input".into(), input.clone());
    body.insert("encoding_format".into(), json!(encoding_format));
    if let Some(dim) = dimensions
        .and_then(Value::as_f64)
        .filter(|d| d.is_finite() && *d > 0.0)
    {
        // `Number(dimensions)` then `JSON.stringify`: a whole value has no
        // decimal point, and payloads here are byte-compared.
        body.insert(
            "dimensions".into(),
            crate::translator::concerns::primitives::js_json_number(dim),
        );
    }
    // Only sent when the model declares it; every other payload stays
    // byte-identical.
    if let Some(input_type) = input_type {
        body.insert(INPUT_TYPE_PARAM.into(), json!(input_type));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_carries_encoding_format_and_conditional_dimensions() {
        let body = build_body("m", &json!("hello"), "float", None, None);
        assert_eq!(
            body,
            json!({"model": "m", "input": "hello", "encoding_format": "float"})
        );

        let body = build_body("m", &json!(["a", "b"]), "base64", Some(&json!(256)), None);
        assert_eq!(
            body,
            json!({"model": "m", "input": ["a", "b"], "encoding_format": "base64", "dimensions": 256})
        );

        // Empty string, zero and negative are all dropped.
        for dim in [json!(""), json!(0), json!(-4), json!(null)] {
            let body = build_body("m", &json!("x"), "float", Some(&dim), None);
            assert!(body.get("dimensions").is_none(), "{dim} must be dropped");
        }
    }

    #[test]
    fn input_type_is_resolved_only_when_the_model_declares_it() {
        let declared = vec!["input_type".to_string()];
        assert_eq!(resolve_input_type(&declared, None), Some("query"));
        assert_eq!(
            resolve_input_type(&declared, Some(&json!(" passage "))),
            Some("passage")
        );
        assert_eq!(
            resolve_input_type(&declared, Some(&json!("QUERY"))),
            Some("query")
        );
        // Unknown, non-string and empty all fall back to the default.
        for bad in [json!("document"), json!(42), json!("")] {
            assert_eq!(resolve_input_type(&declared, Some(&bad)), Some("query"));
        }
        // A model that declares nothing never receives the param.
        assert_eq!(resolve_input_type(&[], Some(&json!("query"))), None);
        assert_eq!(resolve_input_type(&["dimensions".to_string()], None), None);

        // End to end: the payload gains the key only for a declaring model.
        let body = build_body("m", &json!("x"), "float", None, Some("passage"));
        assert_eq!(body["input_type"], json!("passage"));
    }

    #[test]
    fn nvidia_asymmetric_models_declare_input_type() {
        assert!(
            crate::providers::lookup::model_params("nvidia", "nvidia/nemotron-3.5-embed-8b")
                .iter()
                .any(|p| p == INPUT_TYPE_PARAM)
        );
    }

    #[tokio::test]
    async fn missing_input_is_a_400() {
        let err = embeddings_core(&json!({}), "mistral", "m", None, &ProxyOptions::default())
            .await
            .unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.message, "Missing required field: input");
    }

    #[tokio::test]
    async fn an_unsupported_provider_is_a_400() {
        let err = embeddings_core(
            &json!({"input": "hi"}),
            "deepseek",
            "m",
            None,
            &ProxyOptions::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("does not support embeddings"));
    }
}

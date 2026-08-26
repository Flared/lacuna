use anyhow::Context;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::api_type::{ApiType, ApiTypeHandler, api_type_for_path};

use crate::authorization::Authorization;
use crate::capabilities::Capabilities;
use crate::http_middleware::{auth, capabilities, user_agent};
use crate::inspector::CallbackInspector;
use crate::inspector::DecodingInspector;
use crate::inspector::stream::InspectorStream;
use crate::metrics;
use crate::model_rules;
use crate::provider::{self, ProviderManager};
use crate::request_metadata::{RequestMetadata, ResponseMetadata};

async fn forward_to_provider(
    provider: &provider::Provider,
    api_type: Option<&ApiType>,
    request: axum::extract::Request,
) -> Response {
    try_forward_to_provider(provider, api_type, request)
        .await
        .unwrap_or_else(|e| {
            error!(provider = %provider.key, error = format!("{e:#}"), "forward_to_provider failed");
            (StatusCode::BAD_GATEWAY, "internal server error").into_response()
        })
}

async fn try_forward_to_provider(
    provider: &provider::Provider,
    api_type: Option<&ApiType>,
    mut request: axum::extract::Request,
) -> anyhow::Result<Response> {
    let method = request.method().to_owned();
    let path = request.uri().path().to_owned();
    let user = auth::get_caller_identity(&request);
    let user_agent = user_agent::get_user_agent(&request);
    let normalized_user_agent = user_agent.as_ref().map(|u| u.normalized.clone());
    let api_type_handler = api_type.map(|t| t.handler());
    let api_type_handler_id = api_type_handler
        .as_ref()
        .map(|h| h.id())
        .unwrap_or_default() // empty string: ""
        .to_owned();

    // Inspect the request body to extract metadata
    let request_inspection_metadata = match api_type_handler.as_ref() {
        Some(handler) => {
            let (result, rebuilt_request) = handler.inspect_request(request).await;
            request = rebuilt_request;
            result
                .inspect_err(|e| warn!("Failed to inspect request: {e}"))
                .unwrap_or_default()
        }
        None => Default::default(),
    };

    let mut labels = provider.labels.clone();
    let caps = capabilities::get_capabilities(&request);
    if let Some(ref c) = caps {
        for (k, v) in &c.labels {
            labels.insert(k.clone(), v.clone());
        }
    }

    if !labels.is_empty() {
        tracing::Span::current().record("request_labels", tracing::field::debug(&labels));
    }

    let request_metadata = RequestMetadata {
        provider_key: provider.key.clone(),
        api_handler_id: api_type_handler_id,
        user_identity: user,
        user_agent,
        inspected: request_inspection_metadata,
        labels,
    };

    if !provider.authorizer.is_allowed(&request_metadata) {
        return forbidden_response("request not allowed by provider");
    }

    if let Some(caps) = caps.as_ref()
        && !Authorization::from(caps.clone()).is_allowed(&request_metadata)
    {
        return capabilities_forbidden_response("request not allowed by capabilities", caps);
    }

    let model_rules =
        model_rules::merge_model_rules(provider, caps.as_ref(), normalized_user_agent.as_deref());
    let mut rewritten_model: Option<String> = None;
    if let Some(model) = request_metadata.inspected.model.as_deref()
        && let Some(new_name) = model_rules::get_rewritten_name(model, &model_rules)
        && let Some(handler) = api_type_handler.as_ref()
    {
        request = handler.rewrite_model_in_request(request, &new_name)?;
        rewritten_model = Some(new_name);
    }

    debug!(%method, %path, "downstream_req");

    let upstream_req = provider
        .build_request(request)
        .await
        .context("failed to build request")?;

    let upstream_res = provider
        .send(upstream_req)
        .await
        .context("upstream request failed")?;

    let status = upstream_res.status();
    let model = request_metadata.inspected.model.as_deref();
    info!(%method, %path, %status, ?model, ?rewritten_model, "upstream_resp");
    metrics::record_request(&request_metadata);

    let request_inspection_metadata = request_metadata.inspected.clone();
    let upstream_res = wrap_upstream_response(
        api_type_handler,
        upstream_res,
        &request_inspection_metadata,
        move |result| match result {
            Ok(metadata) => metrics::record_response(&request_metadata, metadata),
            Err(e) => warn!("Failed to inspect response: {e}"),
        },
    );
    Ok(upstream_res)
}

pub async fn provider_proxy_handler(
    State(provider): State<Arc<provider::Provider>>,
    request: axum::extract::Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let api_type = api_type_for_path(&path);
    forward_to_provider(&provider, api_type.as_ref(), request).await
}

pub async fn proxy_handler(
    State(manager): State<Arc<ProviderManager>>,
    request: axum::extract::Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let api_type = match api_type_for_path(&path) {
        Some(api_type) => api_type,
        None => return (StatusCode::NOT_FOUND, "unknown api type for path").into_response(),
    };
    let provider = match manager.get_for_api_type(&api_type) {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, "no provider found for api type").into_response(),
    };
    forward_to_provider(provider, Some(&api_type), request).await
}

fn wrap_upstream_response(
    api_type_handler: Option<Box<dyn ApiTypeHandler + Send + Sync>>,
    upstream_res: reqwest::Response,
    request_inspection_metadata: &crate::request_metadata::RequestInspectionMetadata,
    on_response: impl FnOnce(&Result<ResponseMetadata, anyhow::Error>) + Send + 'static,
) -> Response {
    let status_code = upstream_res.status().as_u16();
    let headers = upstream_res.headers().clone();

    let body = if let Some(api_type_handler) = api_type_handler {
        let inspector =
            api_type_handler.response_inspector(status_code, &headers, request_inspection_metadata);
        let inspector = if let Some(encoding) = headers
            .get("content-encoding")
            .and_then(|s| s.to_str().ok())
        {
            DecodingInspector::wrap(inspector, encoding)
        } else {
            inspector
        };
        let inspector = CallbackInspector::new(inspector, on_response);
        let stream = InspectorStream::new(upstream_res.bytes_stream(), Box::new(inspector));
        Body::from_stream(stream)
    } else {
        Body::from_stream(upstream_res.bytes_stream())
    };

    let mut builder = Response::builder().status(status_code);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    builder.body(body).unwrap().into_response()
}

fn forbidden_response(error: &str) -> anyhow::Result<Response> {
    let body = serde_json::json!({
        "error": error,
    });
    let resp = Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))?;
    Ok(resp)
}

fn capabilities_forbidden_response(
    error: &str,
    capabilities: &Capabilities,
) -> anyhow::Result<Response> {
    let body = serde_json::json!({
        "error": error,
        "capabilities": capabilities.grants,
    });
    let resp = Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::model_rules::ModelRule;
    use crate::provider::ProviderManager;
    use crate::provider::compatibility::Compatibility;
    use crate::test_utils::{make_provider, make_provider_with_model_rules, spawn_echo_server};

    #[tokio::test]
    async fn unmatched_path_returns_404() {
        let response = crate::app::AppBuilder::new()
            .build()
            .oneshot(
                Request::builder()
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn proxy_forwards_to_upstream() {
        let addr = spawn_echo_server().await;

        let mut compat = Compatibility::default();
        compat.openai_chat = true;

        let mut manager = ProviderManager::new();
        manager.add(make_provider("provider-key", &format!("http://{addr}"), compat).await);

        let response = crate::app::AppBuilder::new()
            .manager(manager)
            .build()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .body(Body::from("test-body"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            "echoed /v1/chat/completions test-body"
        );
    }

    #[tokio::test]
    async fn proxy_routes_by_provider_name_prefix() {
        let addr = spawn_echo_server().await;

        let mut compat = Compatibility::default();
        compat.openai_chat = true;

        let mut manager = ProviderManager::new();
        manager.add(make_provider("myopenai", &format!("http://{addr}"), compat).await);

        let response = crate::app::AppBuilder::new()
            .manager(manager)
            .build()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/myopenai/v1/chat/completions")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            "echoed /v1/chat/completions hello"
        );
    }

    #[tokio::test]
    async fn proxy_forwards_to_provider_without_api_type() {
        // We should always be able to call /myprovider/<path> without specifying an API type.
        // This is useful for very simple lacuna use cases where we just want to proxy a generic
        // HTTP API that may not even be an AI provider.
        let addr = spawn_echo_server().await;

        // This is a default compatibility where everything is disabled.
        let compat = Compatibility::default();

        let mut manager = ProviderManager::new();
        manager.add(make_provider("myprovider", &format!("http://{addr}"), compat).await);

        let response = crate::app::AppBuilder::new()
            .manager(manager)
            .build()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/myprovider/some/unknown/path")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            "echoed /some/unknown/path hello"
        );
    }

    #[tokio::test]
    async fn proxy_validates_capabilities() {
        let addr = spawn_echo_server().await;

        let mut manager = ProviderManager::new();
        manager.add(
            make_provider(
                "myprovider",
                &format!("http://{addr}"),
                Compatibility::default(),
            )
            .await,
        );

        let app = crate::app::AppBuilder::new()
            .manager(manager)
            .capabilities_header(Some("Tailscale-App-Capabilities".to_owned()))
            .build();

        // Request without the capabilities header — should be forbidden.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/myprovider/model/us.anthropic.claude-sonnet-4-5/invoke")
                    .body(Body::from("test"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "error": "request not allowed by capabilities",
                "capabilities": [],
            })
        );

        // Request with the capabilities header granting access — should succeed.
        let caps_header = serde_json::json!({
            "flare.io/cap/lacuna/grants": [
                {
                    "providers": ["myprovider"],
                    "models": {"us.anthropic.claude-*": {}}
                }
            ]
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/myprovider/model/us.anthropic.claude-sonnet-4-5/invoke")
                    .header("Tailscale-App-Capabilities", caps_header.to_string())
                    .body(Body::from("test"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn bedrock_compat() -> Compatibility {
        let mut compat = Compatibility::default();
        compat.bedrock_model_invoke = true;
        compat
    }

    #[tokio::test]
    async fn model_rewrite_applied_to_bedrock_path() {
        let addr = spawn_echo_server().await;

        let mut manager = ProviderManager::new();
        manager.add(make_provider_with_model_rules(
            "p",
            &format!("http://{addr}"),
            bedrock_compat(),
            vec![ModelRule {
                pattern: glob::Pattern::new("us.anthropic.claude-opus-4-5*").unwrap(),
                rewrite: Some(
                    "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abcd1234567"
                        .to_owned(),
                ),
            }],
        ).await);

        let response = crate::app::AppBuilder::new()
            .manager(manager)
            .build()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/p/model/us.anthropic.claude-opus-4-5x/invoke")
                    .body(Body::from("body-unchanged"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);

        // Path is rewritten
        assert_eq!(
            body,
            "echoed /model/arn%3Aaws%3Abedrock%3Aus%2Deast%2D1%3A123456789012%3Aapplication%2Dinference%2Dprofile%2Fabcd1234567/invoke body-unchanged",
            "unexpected echoed path: {body}"
        );
    }

    #[tokio::test]
    async fn authorization_is_verified_against_original_model_not_rewrite_target() {
        let addr = spawn_echo_server().await;

        let mut manager = ProviderManager::new();
        manager.add(
            make_provider_with_model_rules(
                "p",
                &format!("http://{addr}"),
                bedrock_compat(),
                vec![ModelRule {
                    pattern: glob::Pattern::new("public-model-*").unwrap(),
                    rewrite: Some("internalmodel".to_owned()),
                }],
            )
            .await,
        );
        let app = crate::app::AppBuilder::new().manager(manager).build();

        // The original model is authorized
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/p/model/public-model-4-6/invoke")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("/model/internalmodel/invoke"),
            "expected rewrite to the target model, got: {body}"
        );

        // The rewrite target is not authorized
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/p/model/internalmodel/invoke")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn proxy_provider_restriction() {
        // Use Bedrock-style paths where the model is extracted from the URL.
        let addr = spawn_echo_server().await;

        let mut compat = Compatibility::default();
        compat.bedrock_model_invoke = true;

        let mut manager = ProviderManager::new();
        manager.add(
            make_provider_with_model_rules(
                "provider-a",
                &format!("http://{addr}"),
                compat.clone(),
                vec![ModelRule {
                    pattern: glob::Pattern::new("*").unwrap(),
                    rewrite: None,
                }],
            )
            .await,
        );

        manager.add(
            make_provider_with_model_rules(
                "provider-b",
                &format!("http://{addr}"),
                compat,
                vec![ModelRule {
                    pattern: glob::Pattern::new("gpt-4o").unwrap(),
                    rewrite: None,
                }],
            )
            .await,
        );

        let app = crate::app::AppBuilder::new().manager(manager).build();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/provider-a/model/allowed-model/invoke")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/provider-b/model/disallowed-model/invoke")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "error": "request not allowed by provider",
            })
        );
    }
}

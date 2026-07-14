mod eventstream_inspector;
mod headers_inspector;

use crate::inspector::protocol::ProtocolInspector;
use crate::inspector::protocol::amazon_eventstream::AmazonEventstreamProtocol;

use super::anthropic::AnthropicSseInspector;
use super::{ApiTypeHandler, ResponseMetadataInspector};
use crate::request_metadata::RequestInspectionMetadata;

pub struct BedrockModelInvokeHandler;

use async_trait::async_trait;

fn is_amazon_event_stream(headers: &http::HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/vnd.amazon.eventstream"))
}

/// Extract the model ID from a Bedrock path like `/model/<model_id>/invoke` or
/// `/model/<model_id>/invokeWithResponseStream`.
fn extract_model_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/model/")?;
    let model = rest.split('/').next()?;
    if model.is_empty() {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(model)
        .decode_utf8_lossy()
        .into_owned();
    Some(decoded)
}

// Bedrock api url format : `/model/<model_id>/invoke[WithResponseStream]`
/// Rewrite the model_id segment of a Bedrock path.
fn rewrite_model_in_path(path: &str, new_name: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let [prefix, model, suffix] = parts.as_slice() else {
        return None;
    };
    if prefix.is_empty() || model.is_empty() || suffix.is_empty() {
        return None;
    }
    let encoded =
        percent_encoding::utf8_percent_encode(new_name, percent_encoding::NON_ALPHANUMERIC);
    Some(format!("/{prefix}/{encoded}/{suffix}"))
}

#[async_trait]
impl ApiTypeHandler for BedrockModelInvokeHandler {
    fn id(&self) -> &'static str {
        "bedrock_model_invoke"
    }

    async fn inspect_request(
        &self,
        request: axum::extract::Request,
    ) -> (
        anyhow::Result<RequestInspectionMetadata>,
        axum::extract::Request,
    ) {
        let model = extract_model_from_path(request.uri().path());
        (
            Ok(RequestInspectionMetadata {
                model,
                ..Default::default()
            }),
            request,
        )
    }

    fn rewrite_model_in_request(
        &self,
        mut request: axum::extract::Request,
        new_name: &str,
    ) -> anyhow::Result<axum::extract::Request> {
        let uri = request.uri();
        let Some(new_path) = rewrite_model_in_path(uri.path(), new_name) else {
            return Ok(request);
        };
        let query_suffix = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

        let mut parts = uri.clone().into_parts();
        parts.path_and_query = Some(format!("{new_path}{query_suffix}").parse()?);
        *request.uri_mut() = http::Uri::from_parts(parts)?;
        Ok(request)
    }

    fn response_inspector(
        &self,
        status: u16,
        headers: &http::HeaderMap,
        request_metadata: &RequestInspectionMetadata,
    ) -> ResponseMetadataInspector {
        if is_amazon_event_stream(headers) {
            Box::new(ProtocolInspector::new(
                AmazonEventstreamProtocol::default(),
                AnthropicSseInspector {
                    input_tokens: None,
                    output_tokens: None,
                    cache_creation_tokens: None,
                    cache_read_input_tokens: None,
                    cache_ttl_hint: request_metadata.cache_ttl_secs,
                },
            ))
        } else {
            headers_inspector::BedrockModelInvokeJsonHandler.response_inspector(
                status,
                headers,
                request_metadata,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    #[test]
    fn extract_model_from_invoke_path() {
        assert_eq!(
            extract_model_from_path("/model/us.anthropic.claude-sonnet-4-5/invoke"),
            Some("us.anthropic.claude-sonnet-4-5".to_owned()),
        );
    }

    #[test]
    fn extract_model_from_streaming_path() {
        assert_eq!(
            extract_model_from_path(
                "/model/us.anthropic.claude-opus-4-5-20251101-v1:0/invokeWithResponseStream"
            ),
            Some("us.anthropic.claude-opus-4-5-20251101-v1:0".to_owned()),
        );
    }

    #[test]
    fn extract_model_percent_decodes_segment() {
        let arn =
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abcd1234567";
        let encoded = "/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Aapplication-inference-profile%2Fabcd1234567/invoke";
        assert_eq!(extract_model_from_path(encoded), Some(arn.to_owned()));
    }

    #[test]
    fn extract_model_from_invalid_paths() {
        assert_eq!(extract_model_from_path("/model//invoke"), None);
        assert_eq!(extract_model_from_path("/other/path"), None);
        assert_eq!(extract_model_from_path(""), None);
    }

    #[test]
    fn rewrite_model_in_path_only_replaces_the_segment() {
        // A model id that also appears in the suffix must not be touched there:
        // only the `<model_id>` segment is rewritten.
        assert_eq!(
            rewrite_model_in_path("/model/invoke/invoke", "target"),
            Some("/model/target/invoke".to_owned()),
        );

        assert_eq!(
            rewrite_model_in_path("/model/invoke/invokeWithResponseStream", "target2"),
            Some("/model/target2/invokeWithResponseStream".to_owned()),
        );
    }

    #[test]
    fn rewrite_model_in_path_percent_encodes_target() {
        let arn =
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abcd1234567";
        let out = rewrite_model_in_path("/model/us.anthropic.claude-opus-4-5/invoke", arn).unwrap();

        assert_eq!(
            out,
            "/model/arn%3Aaws%3Abedrock%3Aus%2Deast%2D1%3A123456789012%3Aapplication%2Dinference%2Dprofile%2Fabcd1234567/invoke"
        );
    }

    #[test]
    fn dont_rewrite_model_in_path_if_invalid_path() {
        // Not exactly three segments.
        assert_eq!(rewrite_model_in_path("/other/invoke", "target"), None);
        assert_eq!(rewrite_model_in_path("", "target"), None);
        assert_eq!(rewrite_model_in_path("/model/no-suffix", "target"), None);
        assert_eq!(
            rewrite_model_in_path("/model/foo/invoke/extra", "target"),
            None
        );
        // Empty model segment.
        assert_eq!(rewrite_model_in_path("/model//invoke", "target"), None);
        // Empty prefix segment.
        assert_eq!(rewrite_model_in_path("//foo/invoke", "target"), None);
        // Empty suffix segment.
        assert_eq!(rewrite_model_in_path("/model/foo/", "target"), None);
    }

    fn invoke_request(uri: &str) -> axum::extract::Request {
        axum::http::Request::builder()
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn rewrite_model_in_request_preserves_query() {
        let request =
            invoke_request("/model/us.anthropic.claude-sonnet-4-5/invoke?foo=bar&baz=qux%26");
        let rewritten = BedrockModelInvokeHandler
            .rewrite_model_in_request(request, "target")
            .unwrap();
        assert_eq!(
            rewritten.uri().path_and_query().unwrap().as_str(),
            "/model/target/invoke?foo=bar&baz=qux%26",
        );
    }
}

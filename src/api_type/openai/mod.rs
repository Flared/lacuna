mod chat_completion;
mod responses;

pub use chat_completion::OpenAiChatCompletionHandler;
pub use responses::OpenAiResponsesHandler;

use axum::body::Body;
use serde::Deserialize;

use crate::request_metadata::RequestInspectionMetadata;

/// The subset of an OpenAI request body (chat completions and responses) that we inspect.
/// Both APIs carry the model name in a top-level `model` field.
#[derive(Debug, Deserialize)]
struct OpenAiRequestBody {
    model: Option<String>,
}

/// Read the request body to extract the model name, then rebuild the request unchanged.
///
/// An unparsable body is not an error: the metadata is left empty and the upstream
/// provider is left to reject the request.
async fn inspect_openai_request(
    request: axum::extract::Request,
) -> (
    anyhow::Result<RequestInspectionMetadata>,
    axum::extract::Request,
) {
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let request = axum::http::Request::from_parts(parts, Body::empty());
            return (
                Err(anyhow::anyhow!("failed to read request body: {e}")),
                request,
            );
        }
    };
    let metadata = match serde_json::from_slice::<OpenAiRequestBody>(&bytes) {
        Ok(body) => RequestInspectionMetadata {
            model: body.model,
            ..Default::default()
        },
        Err(e) => {
            tracing::error!("Failed to parse OpenAI request body: {e}");
            RequestInspectionMetadata::default()
        }
    };
    let request = axum::http::Request::from_parts(parts, Body::from(bytes));
    (Ok(metadata), request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_type::ApiTypeHandler;

    fn make_request(uri: &'static str, body: &'static [u8]) -> axum::extract::Request {
        axum::http::Request::builder()
            .uri(uri)
            .body(Body::from(body))
            .unwrap()
    }

    async fn model_for(
        handler: &(dyn ApiTypeHandler + Sync),
        uri: &'static str,
        body: &'static [u8],
    ) -> Option<String> {
        let (result, _request) = handler.inspect_request(make_request(uri, body)).await;
        result.unwrap().model
    }

    #[tokio::test]
    async fn chat_completion_model() {
        let model = model_for(
            &OpenAiChatCompletionHandler,
            "http://localhost/v1/chat/completions",
            br#"{"model": "gpt-4o", "messages": [{"role": "user", "content": "Hi"}]}"#,
        )
        .await;
        assert_eq!(model, Some("gpt-4o".to_owned()));
    }

    #[tokio::test]
    async fn responses_model() {
        let model = model_for(
            &OpenAiResponsesHandler,
            "http://localhost/v1/responses",
            br#"{"model": "gpt-5", "input": "Hi"}"#,
        )
        .await;
        assert_eq!(model, Some("gpt-5".to_owned()));
    }

    #[tokio::test]
    async fn no_model() {
        let model = model_for(
            &OpenAiChatCompletionHandler,
            "http://localhost/v1/chat/completions",
            br#"{"messages": []}"#,
        )
        .await;
        assert_eq!(model, None);
    }

    #[tokio::test]
    async fn null_model() {
        let model = model_for(
            &OpenAiResponsesHandler,
            "http://localhost/v1/responses",
            br#"{"model": null}"#,
        )
        .await;
        assert_eq!(model, None);
    }

    #[tokio::test]
    async fn invalid_json() {
        let model = model_for(
            &OpenAiChatCompletionHandler,
            "http://localhost/v1/chat/completions",
            b"not json",
        )
        .await;
        assert_eq!(model, None);
    }

    #[tokio::test]
    async fn empty_body() {
        let model = model_for(
            &OpenAiResponsesHandler,
            "http://localhost/v1/responses",
            b"",
        )
        .await;
        assert_eq!(model, None);
    }

    #[tokio::test]
    async fn body_is_preserved() {
        let body: &'static [u8] = br#"{"model": "gpt-4o", "stream": true}"#;
        let (_result, request) = OpenAiChatCompletionHandler
            .inspect_request(make_request("http://localhost/v1/chat/completions", body))
            .await;
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], body);
    }
}

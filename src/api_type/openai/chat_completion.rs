use super::super::{ApiTypeHandler, Inspector, ResponseMetadata, ResponseMetadataInspector};
use crate::inspector::protocol::ProtocolInspector;
use crate::inspector::protocol::text::{TextBody, TextProtocol};
use crate::request_metadata::RequestInspectionMetadata;
use async_trait::async_trait;
use serde::Deserialize;

// https://platform.openai.com/docs/api-reference/chat/object
#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatCompletionUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatCompletionResponse {
    usage: OpenAiChatCompletionUsage,
}

pub struct OpenAiChatCompletionHandler;

#[async_trait]
impl ApiTypeHandler for OpenAiChatCompletionHandler {
    fn id(&self) -> &'static str {
        "openai_chat_completion"
    }

    async fn inspect_request(
        &self,
        request: axum::extract::Request,
    ) -> (
        anyhow::Result<RequestInspectionMetadata>,
        axum::extract::Request,
    ) {
        super::inspect_openai_request(request).await
    }

    fn response_inspector(
        &self,
        _status: u16,
        _headers: &http::HeaderMap,
        _request_metadata: &crate::request_metadata::RequestInspectionMetadata,
    ) -> ResponseMetadataInspector {
        Box::new(ProtocolInspector::new(
            TextProtocol::new(),
            OpenAiChatInspector { metadata: None },
        ))
    }
}

struct OpenAiChatInspector {
    metadata: Option<Result<ResponseMetadata, anyhow::Error>>,
}

impl Inspector<TextBody> for OpenAiChatInspector {
    type Output = ResponseMetadata;

    fn feed(&mut self, body: TextBody) {
        self.metadata = Some(parse_chat_completion(&body.data));
    }

    fn finish(self: Box<Self>) -> Result<ResponseMetadata, anyhow::Error> {
        self.metadata
            .unwrap_or_else(|| Err(anyhow::anyhow!("no response body")))
    }
}

fn parse_chat_completion(data: &[u8]) -> Result<ResponseMetadata, anyhow::Error> {
    let parsed = serde_json::from_slice::<OpenAiChatCompletionResponse>(data)?;
    let cache_read_input_tokens = parsed
        .usage
        .prompt_tokens_details
        .and_then(|d| d.cached_tokens);
    Ok(ResponseMetadata {
        input_tokens: Some(parsed.usage.prompt_tokens),
        output_tokens: Some(parsed.usage.completion_tokens),
        cache_read_input_tokens,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_inspector() -> ResponseMetadataInspector {
        OpenAiChatCompletionHandler.response_inspector(
            200,
            &http::HeaderMap::new(),
            &crate::request_metadata::RequestInspectionMetadata::default(),
        )
    }

    #[test]
    fn inspect_response_full() {
        let body = br#"{
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi!"}}],
            "usage": {"prompt_tokens": 15, "completion_tokens": 42, "total_tokens": 57}
        }"#;
        let mut inspector = make_inspector();
        inspector.feed(body);
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.input_tokens, Some(15));
        assert_eq!(metadata.output_tokens, Some(42));
    }

    #[test]
    fn inspect_response_missing_usage() {
        let mut inspector = make_inspector();
        inspector.feed(br#"{"id": "chatcmpl-abc123", "object": "chat.completion"}"#);
        assert!(inspector.finish().is_err());
    }

    #[test]
    fn inspect_response_invalid_json() {
        let mut inspector = make_inspector();
        inspector.feed(b"not json");
        assert!(inspector.finish().is_err());
    }

    #[test]
    fn inspect_response_cached_tokens() {
        let body = br#"{
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 42,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        }"#;
        let mut inspector = make_inspector();
        inspector.feed(body);
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.input_tokens, Some(100));
        assert_eq!(metadata.cache_read_input_tokens, Some(80));
    }

    #[test]
    fn inspect_response_no_cached_tokens() {
        let body = br#"{
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "usage": {"prompt_tokens": 15, "completion_tokens": 42}
        }"#;
        let mut inspector = make_inspector();
        inspector.feed(body);
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.cache_read_input_tokens, None);
    }
}

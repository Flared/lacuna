use std::collections::HashMap;

use crate::inspector::protocol::ProtocolInspector;
use crate::inspector::protocol::text::{TextBody, TextProtocol};

use super::super::anthropic::parse_anthropic_json;
use super::super::{ApiTypeHandler, Inspector, ResponseMetadata, ResponseMetadataInspector};

pub struct BedrockModelInvokeJsonHandler;

impl ApiTypeHandler for BedrockModelInvokeJsonHandler {
    fn id(&self) -> &'static str {
        "bedrock_model_invoke"
    }

    fn response_inspector(
        &self,
        _status: u16,
        headers: &http::HeaderMap,
        _request_metadata: &crate::request_metadata::RequestInspectionMetadata,
    ) -> ResponseMetadataInspector {
        let input_tokens =
            parse_token_header(headers, "x-amzn-bedrock-input-token-count").unwrap_or(None);
        let output_tokens =
            parse_token_header(headers, "x-amzn-bedrock-output-token-count").unwrap_or(None);
        let cache_read_input_tokens =
            parse_token_header(headers, "x-amzn-bedrock-cache-read-input-token-count")
                .unwrap_or(None);
        let cache_creation_tokens =
            parse_token_header(headers, "x-amzn-bedrock-cache-write-input-token-count")
                .unwrap_or(None)
                .filter(|tokens| *tokens > 0)
                .map(|tokens| HashMap::from([("unknown".to_owned(), tokens)]));
        Box::new(ProtocolInspector::new(
            TextProtocol::new(),
            BedrockInvokeJsonInspector {
                header_metadata: ResponseMetadata {
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_input_tokens,
                },
                body_metadata: None,
            },
        ))
    }
}

struct BedrockInvokeJsonInspector {
    header_metadata: ResponseMetadata,
    body_metadata: Option<ResponseMetadata>,
}

impl Inspector<TextBody> for BedrockInvokeJsonInspector {
    type Output = ResponseMetadata;

    fn feed(&mut self, body: TextBody) {
        self.body_metadata = parse_anthropic_json(&body.data).ok();
    }

    fn finish(self: Box<Self>) -> Result<ResponseMetadata, anyhow::Error> {
        let headers = self.header_metadata;
        let Some(body) = self.body_metadata else {
            return Ok(headers);
        };
        Ok(ResponseMetadata {
            input_tokens: body.input_tokens.or(headers.input_tokens),
            output_tokens: body.output_tokens.or(headers.output_tokens),
            cache_creation_tokens: body.cache_creation_tokens.or(headers.cache_creation_tokens),
            cache_read_input_tokens: body
                .cache_read_input_tokens
                .or(headers.cache_read_input_tokens),
        })
    }
}

fn parse_token_header(
    headers: &http::HeaderMap,
    header: &str,
) -> Result<Option<u64>, anyhow::Error> {
    match headers.get(header) {
        Some(value) => Ok(Some(value.to_str()?.parse::<u64>()?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_to_map(headers: &[(&str, &str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (key, value) in headers {
            map.insert(
                http::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn inspect_response_with_headers() {
        let headers = headers_to_map(&[
            ("x-amzn-bedrock-input-token-count", "25"),
            ("x-amzn-bedrock-output-token-count", "150"),
        ]);
        let inspector = BedrockModelInvokeJsonHandler.response_inspector(
            200,
            &headers,
            &crate::request_metadata::RequestInspectionMetadata::default(),
        );
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.input_tokens, Some(25));
        assert_eq!(metadata.output_tokens, Some(150));
    }

    #[test]
    fn inspect_response_missing_headers() {
        let headers = http::HeaderMap::new();
        let inspector = BedrockModelInvokeJsonHandler.response_inspector(
            200,
            &headers,
            &crate::request_metadata::RequestInspectionMetadata::default(),
        );
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.input_tokens, None);
        assert_eq!(metadata.output_tokens, None);
    }

    #[test]
    fn inspect_response_invalid_header() {
        let headers = headers_to_map(&[("x-amzn-bedrock-input-token-count", "not_a_number")]);
        let inspector = BedrockModelInvokeJsonHandler.response_inspector(
            200,
            &headers,
            &crate::request_metadata::RequestInspectionMetadata::default(),
        );
        let metadata = inspector.finish().unwrap();
        assert_eq!(metadata.input_tokens, None);
        assert_eq!(metadata.output_tokens, None);
    }
}

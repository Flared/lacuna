use std::collections::HashMap;

use async_trait::async_trait;
use axum::body::Body;
use serde::Deserialize;

use crate::inspector::protocol::ProtocolInspector;
use crate::inspector::protocol::sse::{SseEvent, SseProtocol};
use crate::inspector::protocol::text::{TextBody, TextProtocol};
use crate::request_metadata::RequestInspectionMetadata;

use super::{ApiTypeHandler, Inspector, ResponseMetadata, ResponseMetadataInspector};

#[derive(Debug, Deserialize)]
struct CacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
}

fn build_cache_creation_map(
    usage: &Usage,
    cache_ttl_hint: Option<u64>,
) -> Option<HashMap<String, u64>> {
    if let Some(cc) = &usage.cache_creation {
        let mut map = HashMap::new();
        if let Some(tokens) = cc.ephemeral_5m_input_tokens
            && tokens > 0
        {
            map.insert("5m".to_owned(), tokens);
        }
        if let Some(tokens) = cc.ephemeral_1h_input_tokens
            && tokens > 0
        {
            map.insert("1h".to_owned(), tokens);
        }
        if map.is_empty() {
            return None;
        } else {
            return Some(map);
        }
    } else if let Some(tokens) = usage.cache_creation_input_tokens
        && tokens > 0
    {
        let duration = match cache_ttl_hint {
            Some(300) => "5m",
            Some(3600) => "1h",
            _ => "unknown",
        };
        return Some(HashMap::from([(duration.to_owned(), tokens)]));
    }
    None
}

// Payload for both non-streaming and SSE.
#[derive(Debug, Deserialize)]
struct AnthropicDataWithUsage {
    usage: Usage,
}

// SSE streaming event payloads.
#[derive(Debug, Deserialize)]
struct MessageStartData {
    message: AnthropicDataWithUsage,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicRequestBody {
    model: Option<String>,
    system: Option<serde_json::Value>,
    messages: Option<Vec<Message>>,
}

/// Collect cache_control TTL values from a JSON value that may be a single
/// content block or an array of content blocks.
fn collect_cache_ttls(value: &serde_json::Value, ttls: &mut Vec<u64>) {
    let blocks: Vec<&serde_json::Value> = if let Some(arr) = value.as_array() {
        arr.iter().collect()
    } else if value.is_object() {
        vec![value]
    } else {
        return;
    };
    for block in blocks {
        if let Some(cc) = block.get("cache_control") {
            // cache_control block present → default TTL is 300s when ttl field absent
            let ttl = cc.get("ttl").and_then(|v| v.as_u64()).unwrap_or(300);
            ttls.push(ttl);
        }
    }
}

/// Extract a uniform cache TTL from the request body.
/// Returns Some(ttl) if all cache_control blocks share the same TTL, None if mixed or absent.
fn extract_cache_ttl(body: &AnthropicRequestBody) -> Option<u64> {
    let mut ttls = Vec::new();
    if let Some(system) = &body.system {
        collect_cache_ttls(system, &mut ttls);
    }
    if let Some(messages) = &body.messages {
        for msg in messages {
            if let Some(content) = &msg.content {
                collect_cache_ttls(content, &mut ttls);
            }
        }
    }
    if ttls.is_empty() {
        return None;
    }
    let first = ttls[0];
    if ttls.iter().all(|&t| t == first) {
        Some(first)
    } else {
        None
    }
}

pub struct AnthropicMessagesHandler;

#[async_trait]
impl ApiTypeHandler for AnthropicMessagesHandler {
    fn id(&self) -> &'static str {
        "anthropic_messages"
    }

    async fn inspect_request(
        &self,
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
        let metadata = match serde_json::from_slice::<AnthropicRequestBody>(&bytes) {
            Ok(body) => {
                let cache_ttl_secs = extract_cache_ttl(&body);
                RequestInspectionMetadata {
                    model: body.model,
                    cache_ttl_secs,
                }
            }
            Err(e) => {
                tracing::error!("Failed to parse Anthropic request body: {e}");
                RequestInspectionMetadata::default()
            }
        };
        let request = axum::http::Request::from_parts(parts, Body::from(bytes));
        (Ok(metadata), request)
    }

    fn response_inspector(
        &self,
        _status: u16,
        headers: &http::HeaderMap,
        request_metadata: &RequestInspectionMetadata,
    ) -> ResponseMetadataInspector {
        if is_event_stream(headers) {
            Box::new(ProtocolInspector::new(
                SseProtocol::new(),
                AnthropicSseInspector {
                    input_tokens: None,
                    output_tokens: None,
                    cache_creation_tokens: None,
                    cache_read_input_tokens: None,
                    cache_ttl_hint: request_metadata.cache_ttl_secs,
                },
            ))
        } else {
            Box::new(ProtocolInspector::new(
                TextProtocol::new(),
                AnthropicJsonInspector { metadata: None },
            ))
        }
    }
}

fn is_event_stream(headers: &http::HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"))
}

pub(crate) struct AnthropicSseInspector {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) cache_creation_tokens: Option<HashMap<String, u64>>,
    pub(crate) cache_read_input_tokens: Option<u64>,
    pub(crate) cache_ttl_hint: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SseEventType {
    r#type: String,
}

impl AnthropicSseInspector {
    /// Process a single Anthropic JSON event string.
    /// Used by Bedrock eventstream inspector where event type is embedded in JSON.
    pub(crate) fn process_event_json(&mut self, json: &str) {
        if let Ok(evt) = serde_json::from_str::<SseEventType>(json) {
            self.process_event(evt.r#type.as_str(), json);
        }
    }

    fn process_event(&mut self, event_type: &str, data: &str) {
        match event_type {
            "message_start" => {
                if let Ok(msg) = serde_json::from_str::<MessageStartData>(data) {
                    self.input_tokens = msg.message.usage.input_tokens;
                    self.cache_creation_tokens =
                        build_cache_creation_map(&msg.message.usage, self.cache_ttl_hint);
                    self.cache_read_input_tokens = msg.message.usage.cache_read_input_tokens;
                }
            }
            "message_delta" => {
                if let Ok(delta) = serde_json::from_str::<AnthropicDataWithUsage>(data) {
                    self.output_tokens = delta.usage.output_tokens;
                }
            }
            _ => {}
        }
    }
}

impl Inspector<SseEvent> for AnthropicSseInspector {
    type Output = ResponseMetadata;

    fn feed(&mut self, event: SseEvent) {
        self.process_event(event.event_type.as_str(), &event.data);
    }

    fn finish(self: Box<Self>) -> Result<ResponseMetadata, anyhow::Error> {
        if self.input_tokens.is_none() && self.output_tokens.is_none() {
            return Err(anyhow::anyhow!("no token usage found in SSE stream"));
        }
        Ok(ResponseMetadata {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        })
    }
}

struct AnthropicJsonInspector {
    metadata: Option<Result<ResponseMetadata, anyhow::Error>>,
}

impl Inspector<TextBody> for AnthropicJsonInspector {
    type Output = ResponseMetadata;

    fn feed(&mut self, body: TextBody) {
        self.metadata = Some(parse_anthropic_json(&body.data));
    }

    fn finish(self: Box<Self>) -> Result<ResponseMetadata, anyhow::Error> {
        self.metadata
            .unwrap_or_else(|| Err(anyhow::anyhow!("no response body")))
    }
}

pub(crate) fn parse_anthropic_json(data: &[u8]) -> Result<ResponseMetadata, anyhow::Error> {
    let parsed = serde_json::from_slice::<AnthropicDataWithUsage>(data)?;
    let cache_creation_tokens = build_cache_creation_map(&parsed.usage, None);
    Ok(ResponseMetadata {
        input_tokens: parsed.usage.input_tokens,
        output_tokens: parsed.usage.output_tokens,
        cache_creation_tokens,
        cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
    })
}

#[cfg(test)]
mod tests;

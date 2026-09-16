use crate::tools::ToolDefinition;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use ureq::http::{HeaderValue, Request};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 512;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: &'a [Message],
    tools: &'a [ToolDefinition],
    tool_choice: ToolChoice,
}

#[derive(Debug, Serialize)]
struct ToolChoice {
    #[serde(rename = "type")]
    kind: &'static str,
    disable_parallel_tool_use: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Message {
    role: &'static str,
    content: Vec<ContentBlock>,
}

impl Message {
    pub(crate) fn user(text: String) -> Self {
        Self {
            role: "user",
            content: vec![ContentBlock::Text { text }],
        }
    }

    pub(crate) fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: "assistant",
            content,
        }
    }

    pub(crate) fn tool_result(tool_use_id: String, content: String, is_error: bool) -> Self {
        Self {
            role: "user",
            content: vec![ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            }],
        }
    }
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    #[serde(rename = "type")]
    kind: String,
    role: String,
    content: Vec<ContentBlock>,
    stop_reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result", skip_deserializing)]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
    },
    #[serde(other)]
    Unsupported,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
}

#[derive(Debug)]
pub(crate) struct AssistantResponse {
    pub(crate) content: Vec<ContentBlock>,
    pub(crate) tool_call: Option<ToolCall>,
}

pub(crate) struct Client {
    agent: ureq::Agent,
    key: String,
}

impl Client {
    pub(crate) fn new(key: String) -> Result<Self, String> {
        validate_key(&key)?;
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Ok(Self { agent, key })
    }

    pub(crate) fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<AssistantResponse, String> {
        let request = build_request(messages, tools, &self.key)?;
        let mut response = self.agent.run(request).map_err(transport_error)?;
        check_status(response.status().as_u16())?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(transport_error)?;
        decode_response(&body)
    }
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("ANTHROPIC_API_KEY must be nonempty ASCII without whitespace".into());
    }
    Ok(())
}

fn build_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    key: &str,
) -> Result<Request<Vec<u8>>, String> {
    validate_key(key)?;
    let mut header = HeaderValue::from_str(key).map_err(|_| "invalid API key header")?;
    header.set_sensitive(true);
    let body = encode_request(messages, tools)?;
    Request::post(ENDPOINT)
        .header("x-api-key", header)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body)
        .map_err(|_| "could not construct the HTTP request".into())
}

fn encode_request(messages: &[Message], tools: &[ToolDefinition]) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(&MessageRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        stream: false,
        messages,
        tools,
        tool_choice: ToolChoice {
            kind: "auto",
            disable_parallel_tool_use: true,
        },
    })
    .map_err(|_| "could not encode the request as JSON")?;
    check_request_size(body.len())?;
    Ok(body)
}

fn check_request_size(bytes: usize) -> Result<(), String> {
    if bytes > MAX_REQUEST_BYTES {
        return Err("serialized request exceeds the 1 MiB limit; request was not sent".into());
    }
    Ok(())
}

fn check_status(status: u16) -> Result<(), String> {
    if status == 200 {
        return Ok(());
    }
    let explanation = match status {
        401 => "authentication failed; check ANTHROPIC_API_KEY",
        _ => "unexpected response status",
    };
    Err(format!("Anthropic HTTP {status}: {explanation}"))
}

fn decode_response(body: &[u8]) -> Result<AssistantResponse, String> {
    let response: MessageResponse = serde_json::from_slice(body)
        .map_err(|_| "Anthropic returned invalid JSON or an unexpected message schema")?;
    if response.kind != "message" || response.role != "assistant" {
        return Err("expected an Anthropic assistant message".into());
    }
    match response.stop_reason.as_str() {
        "end_turn" | "tool_use" => {}
        "max_tokens" => {
            return Err(
                "response reached the 512-token output limit; partial text withheld".into(),
            );
        }
        "refusal" => return Err("Anthropic refused the request".into()),
        _ => return Err("response did not finish with end_turn or tool_use".into()),
    }

    let mut has_text = false;
    let mut tool_call = None;
    for block in &response.content {
        match block {
            ContentBlock::Text { text } => has_text |= !text.trim().is_empty(),
            ContentBlock::ToolUse { id, name, input } => {
                if id.is_empty()
                    || !id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    return Err("tool request has an invalid call ID".into());
                }
                if name.is_empty() {
                    return Err("tool request has an empty name".into());
                }
                if !input.is_object() {
                    return Err("tool request input must be a JSON object".into());
                }
                if tool_call.is_some() {
                    return Err("response contains multiple tool requests".into());
                }
                tool_call = Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            ContentBlock::Unsupported | ContentBlock::ToolResult { .. } => {
                return Err("response contains unsupported non-text content".into());
            }
        }
    }

    if (response.stop_reason == "tool_use") != tool_call.is_some() {
        return Err("response stop_reason is inconsistent with tool request content".into());
    }
    if tool_call.is_none() && !has_text {
        return Err("response contains no usable text".into());
    }
    Ok(AssistantResponse {
        content: response.content,
        tool_call,
    })
}

fn transport_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => "Anthropic request timed out (60-second limit)",
        ureq::Error::BodyExceedsLimit(_) => "Anthropic response exceeds the 1 MiB limit",
        _ => "Anthropic transport failed; check connectivity, TLS, and proxy settings",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolCatalog;
    use serde_json::json;

    fn response() -> serde_json::Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"end_turn",
            "content":[{"type":"text", "text":"Hello "}, {"type":"text", "text":"世界!"}],
            "usage":{"input_tokens":8,"output_tokens":4}})
    }

    fn tool_response(name: &str, input: serde_json::Value) -> serde_json::Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"tool_use",
            "content":[{"type":"tool_use", "id":"toolu_synthetic", "name":name,
                "input":input}]})
    }

    #[test]
    fn request_exposes_the_wire_contract() {
        let text = "  Say \"hello\"\n世界\\  ";
        let tools = ToolCatalog::without_workspace().definitions();
        let request =
            build_request(&[Message::user(text.into())], &tools, "synthetic-key").unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.uri(), ENDPOINT);
        assert!(request.headers()["x-api-key"].is_sensitive());
        let wire: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
        assert_eq!(wire["messages"][0]["content"][0]["text"], text);
        assert_eq!(wire["tools"][0]["name"], "get_runtime_info");
        assert_eq!(wire["tool_choice"]["disable_parallel_tool_use"], true);
    }

    #[test]
    fn decoder_preserves_blocks_and_accepts_unknown_structural_tool_calls() {
        let mut body = tool_response("future_tool", json!({"value":1}));
        body["content"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"type":"text","text":"Before"}));
        let decoded = decode_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(decoded.tool_call.unwrap().name, "future_tool");
        assert_eq!(
            serde_json::to_value(decoded.content).unwrap(),
            body["content"]
        );
    }

    #[test]
    fn correlated_tool_results_serialize_success_and_error() {
        let success = Message::tool_result("call-1".into(), "ok".into(), false);
        let error = Message::tool_result("call-2".into(), "denied".into(), true);
        assert_eq!(
            serde_json::to_value(success).unwrap(),
            json!({"role":"user","content":[{"type":"tool_result",
                "tool_use_id":"call-1","content":"ok"}]})
        );
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({"role":"user","content":[{"type":"tool_result",
                "tool_use_id":"call-2","content":"denied","is_error":true}]})
        );
    }

    #[test]
    fn rejects_malformed_and_inconsistent_responses() {
        for body in [b"not json".as_slice(), b"{}"] {
            assert!(decode_response(body).is_err());
        }
        for content in [
            json!([]),
            json!([{"type":"text","text":"  "}]),
            json!([{"type":"unknown"}]),
        ] {
            let mut body = response();
            body["content"] = content;
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        for reason in ["max_tokens", "refusal", "unknown"] {
            let mut body = response();
            body["stop_reason"] = json!(reason);
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        let valid = tool_response("get_runtime_info", json!({}))["content"][0].clone();
        for content in [
            json!([valid, valid]),
            json!([{"type":"tool_use","id":"bad id","name":"x","input":{}}]),
            json!([{"type":"tool_use","id":"ok","name":"","input":{}}]),
            json!([{"type":"tool_use","id":"ok","name":"x","input":null}]),
        ] {
            let mut body = tool_response("get_runtime_info", json!({}));
            body["content"] = content;
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
    }

    #[test]
    fn request_size_and_status_boundaries_are_explicit() {
        assert!(check_request_size(MAX_REQUEST_BYTES).is_ok());
        assert!(check_request_size(MAX_REQUEST_BYTES + 1).is_err());
        let tools = ToolCatalog::without_workspace().definitions();
        let oversized = Message::user("x".repeat(MAX_REQUEST_BYTES));
        assert!(encode_request(&[oversized], &tools).is_err());
        assert!(check_status(200).is_ok());
        assert!(check_status(401).unwrap_err().contains("authentication"));
        assert!(check_status(500).unwrap_err().contains("unexpected"));
        for key in ["", " ", "synthetic\nsecret", "é"] {
            assert!(validate_key(key).is_err());
        }
    }
}

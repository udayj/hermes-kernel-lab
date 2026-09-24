use crate::tools::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_slice, to_vec};
use std::time::Duration;
use ureq::{
    Agent as HttpAgent, Error as HttpError,
    http::{HeaderValue, Request},
};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
pub const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 512;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    system: &'a str,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: String) -> Self {
        Self {
            role: "user".into(),
            content: vec![ContentBlock::Text { text }],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: "assistant".into(),
            content,
        }
    }

    pub fn tool_result(tool_use_id: String, content: String, is_error: bool) -> Self {
        Self {
            role: "user".into(),
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
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
    #[serde(other)]
    Unsupported,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug)]
pub struct AssistantResponse {
    pub content: Vec<ContentBlock>,
    pub tool_call: Option<ToolCall>,
}

pub struct Client {
    agent: HttpAgent,
    key: String,
}

impl Client {
    pub fn new(key: String) -> Result<Self, String> {
        validate_key(&key)?;
        let agent = HttpAgent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Ok(Self { agent, key })
    }

    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<AssistantResponse, String> {
        let request = build_request(system, messages, tools, &self.key)?;
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
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    key: &str,
) -> Result<Request<Vec<u8>>, String> {
    validate_key(key)?;
    let mut header = HeaderValue::from_str(key).map_err(|_| "invalid API key header")?;
    header.set_sensitive(true);
    let body = encode_request(system, messages, tools)?;
    Request::post(ENDPOINT)
        .header("x-api-key", header)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body)
        .map_err(|_| "could not construct the HTTP request".into())
}

fn encode_request(
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<Vec<u8>, String> {
    let body = to_vec(&MessageRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        stream: false,
        system,
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
    let response: MessageResponse = from_slice(body)
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

    let tool_call = validate_assistant_content(&response.content)?;
    if (response.stop_reason == "tool_use") != tool_call.is_some() {
        return Err("response stop_reason is inconsistent with tool request content".into());
    }
    Ok(AssistantResponse {
        content: response.content,
        tool_call,
    })
}

// Checkpoints share block validation, but HTTP still requires a valid stop reason.
pub fn validate_assistant_content(content: &[ContentBlock]) -> Result<Option<ToolCall>, String> {
    let mut has_text = false;
    let mut tool_call = None;
    for block in content {
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
                // Dispatch validates arguments so even non-object input can
                // receive a correlated tool error and remain resumable.
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

    if tool_call.is_none() && !has_text {
        return Err("response contains no usable text".into());
    }
    Ok(tool_call)
}

fn transport_error(error: HttpError) -> String {
    match error {
        HttpError::Timeout(_) => "Anthropic request timed out (60-second limit)",
        HttpError::BodyExceedsLimit(_) => "Anthropic response exceeds the 1 MiB limit",
        _ => "Anthropic transport failed; check connectivity, TLS, and proxy settings",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolCatalog;
    use serde_json::{json, to_value};

    fn response() -> Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"end_turn",
            "content":[{"type":"text", "text":"Hello "}, {"type":"text", "text":"世界!"}],
            "usage":{"input_tokens":8,"output_tokens":4}})
    }

    fn tool_response(name: &str, input: Value) -> Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"tool_use",
            "content":[{"type":"tool_use", "id":"toolu_synthetic", "name":name,
                "input":input}]})
    }

    #[test]
    fn request_exposes_the_wire_contract() {
        let text = "  Say \"hello\"\n世界\\  ";
        let tools = ToolCatalog::without_workspace().definitions();
        let request = build_request(
            "synthetic system",
            &[Message::user(text.into())],
            &tools,
            "synthetic-key",
        )
        .unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.uri(), ENDPOINT);
        assert!(request.headers()["x-api-key"].is_sensitive());
        let wire: Value = from_slice(request.body()).unwrap();
        assert_eq!(wire["system"], "synthetic system");
        assert_eq!(
            wire["messages"],
            json!([{"role":"user", "content":[{"type":"text", "text":text}]}])
        );
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
        let decoded = decode_response(&to_vec(&body).unwrap()).unwrap();
        assert_eq!(decoded.tool_call.unwrap().name, "future_tool");
        assert_eq!(to_value(decoded.content).unwrap(), body["content"]);
        let body = tool_response("skill_view", json!(null));
        let decoded = decode_response(&to_vec(&body).unwrap()).unwrap();
        assert_eq!(decoded.tool_call.unwrap().input, json!(null));
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
            json!([{"type":"tool_result","tool_use_id":"call-1","content":"ok"}]),
        ] {
            let mut body = response();
            body["content"] = content;
            assert!(decode_response(&to_vec(&body).unwrap()).is_err());
        }
        for reason in ["max_tokens", "refusal", "unknown"] {
            let mut body = response();
            body["stop_reason"] = json!(reason);
            assert!(decode_response(&to_vec(&body).unwrap()).is_err());
        }
        let valid = tool_response("get_runtime_info", json!({}))["content"][0].clone();
        for content in [
            json!([valid, valid]),
            json!([{"type":"tool_use","id":"bad id","name":"x","input":{}}]),
            json!([{"type":"tool_use","id":"ok","name":"","input":{}}]),
            json!([{"type":"tool_use","id":"ok","name":"skill_view"}]),
        ] {
            let mut body = tool_response("get_runtime_info", json!({}));
            body["content"] = content;
            assert!(decode_response(&to_vec(&body).unwrap()).is_err());
        }
    }

    #[test]
    fn serialized_request_limit_includes_system_and_json_escaping() {
        let tools = ToolCatalog::without_workspace().definitions();
        let system = "synthetic operator text\n";
        let overhead = encode_request(system, &[Message::user(String::new())], &tools)
            .unwrap()
            .len();
        let messages = [Message::user("x".repeat(MAX_REQUEST_BYTES - overhead))];
        assert_eq!(
            encode_request(system, &messages, &tools).unwrap().len(),
            MAX_REQUEST_BYTES
        );
        assert!(encode_request(&format!("{system}x"), &messages, &tools).is_err());
        // Same UTF-8 byte count, but a newline needs an extra JSON escape byte.
        let escaped = system.replacen('s', "\n", 1);
        assert_eq!(escaped.len(), system.len());
        assert!(encode_request(&escaped, &messages, &tools).is_err());
        assert!(encode_request("", &messages, &tools).is_ok());
    }

    #[test]
    fn request_size_and_status_boundaries_are_explicit() {
        assert!(check_request_size(MAX_REQUEST_BYTES).is_ok());
        assert!(check_request_size(MAX_REQUEST_BYTES + 1).is_err());
        let tools = ToolCatalog::without_workspace().definitions();
        let oversized = Message::user("x".repeat(MAX_REQUEST_BYTES));
        assert!(encode_request("synthetic system", &[oversized], &tools).is_err());
        assert!(check_status(200).is_ok());
        assert!(check_status(401).unwrap_err().contains("authentication"));
        assert!(check_status(500).unwrap_err().contains("unexpected"));
        for key in ["", " ", "synthetic\nsecret", "é"] {
            assert!(validate_key(key).is_err());
        }
    }
}

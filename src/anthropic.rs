use crate::tools::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_slice, to_vec};
use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    time::Duration,
};
use ureq::{
    Agent as HttpAgent, Error as HttpError,
    http::{HeaderValue, Request},
};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
pub const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 512;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const OUTPUT_LIMIT_ERROR: &str =
    "response reached the 512-token output limit; partial response discarded";

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    system: &'a str,
    messages: &'a [Message],
    // A tool-free request (the compaction summary) omits both fields.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [ToolDefinition],
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
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

#[derive(Debug, Default, Deserialize)]
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
        output: &mut impl Write,
    ) -> Result<AssistantResponse, String> {
        let request = build_request(system, messages, tools, &self.key)?;
        let mut response = self.agent.run(request).map_err(transport_error)?;
        check_status(response.status().as_u16())?;
        read_body(response.body_mut(), output)
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

pub(crate) fn encode_request(
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<Vec<u8>, String> {
    let body = to_vec(&MessageRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        stream: true,
        system,
        messages,
        tools,
        tool_choice: (!tools.is_empty()).then_some(ToolChoice {
            kind: "auto",
            disable_parallel_tool_use: true,
        }),
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

pub(crate) fn decode_response(body: &[u8]) -> Result<AssistantResponse, String> {
    let response: MessageResponse = from_slice(body)
        .map_err(|_| "Anthropic returned invalid JSON or an unexpected message schema")?;
    finish(response)
}

fn read_body(body: &mut ureq::Body, output: &mut impl Write) -> Result<AssistantResponse, String> {
    let reader = body.with_config().limit(MAX_RESPONSE_BYTES).reader();
    read_stream(BufReader::new(reader), output)
}

pub(crate) fn read_stream(
    reader: impl BufRead,
    output: &mut impl Write,
) -> Result<AssistantResponse, String> {
    let mut wrote_text = false;
    let read = || {
        let mut response = MessageResponse::default();
        let mut block_open = false;
        let mut tool_input = String::new();
        let mut saw_message_stop = false;

        for line in reader.lines() {
            let line = line.map_err(|error| match error.kind() {
                ErrorKind::InvalidData => "Anthropic stream contains invalid UTF-8".into(),
                _ => transport_error(error.into()),
            })?;
            if line.is_empty() || line.starts_with("event:") || line.starts_with(':') {
                continue;
            }
            // Anthropic puts each JSON object on one data line. Multi-line SSE data is unsupported.
            let data = line
                .strip_prefix("data:")
                .ok_or("unexpected Anthropic stream line")?;
            let event: Value =
                serde_json::from_str(data).map_err(|_| "invalid JSON in Anthropic stream")?;
            let field = |path| {
                event
                    .pointer(path)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("missing or invalid Anthropic stream field {path}"))
            };
            let kind = field("/type")?;
            let index = event["index"].as_u64();
            let mut text_to_write = "";
            match kind {
                "message_start" => {
                    let message = &event["message"];
                    if message["type"] != "message" || message["role"] != "assistant" {
                        return Err("expected an Anthropic assistant message".into());
                    }
                    if !response.kind.is_empty() {
                        return Err("duplicate Anthropic message_start".into());
                    }
                    response.kind = "message".into();
                    response.role = "assistant".into();
                }
                "content_block_start" => {
                    if response.kind.is_empty()
                        || block_open
                        || index != Some(response.content.len() as u64)
                    {
                        return Err("unexpected Anthropic content block start".into());
                    }
                    let content = match field("/content_block/type")? {
                        "text" => {
                            text_to_write = field("/content_block/text")?;
                            ContentBlock::Text {
                                text: text_to_write.into(),
                            }
                        }
                        "tool_use" => ContentBlock::ToolUse {
                            id: field("/content_block/id")?.into(),
                            name: field("/content_block/name")?.into(),
                            input: Value::Null,
                        },
                        _ => ContentBlock::Unsupported,
                    };
                    response.content.push(content);
                    block_open = true;
                }
                "content_block_delta" | "content_block_stop" => {
                    if !block_open || index != Some(response.content.len() as u64 - 1) {
                        return Err("unexpected Anthropic content block index".into());
                    }
                    let block = response.content.last_mut().expect("open block");
                    match (kind, block, event["delta"]["type"].as_str()) {
                        ("content_block_stop", ContentBlock::ToolUse { input, .. }, _) => {
                            *input = serde_json::from_str(if tool_input.is_empty() {
                                "{}"
                            } else {
                                &tool_input
                            })
                            .map_err(|_| "invalid tool input JSON in Anthropic stream")?;
                            tool_input.clear();
                            block_open = false;
                        }
                        ("content_block_stop", _, _) => block_open = false,
                        (_, ContentBlock::Text { text }, Some("text_delta")) => {
                            text_to_write = field("/delta/text")?;
                            text.push_str(text_to_write);
                        }
                        (_, ContentBlock::ToolUse { .. }, Some("input_json_delta")) => {
                            tool_input.push_str(field("/delta/partial_json")?);
                        }
                        (_, ContentBlock::Unsupported, _) => {}
                        _ => return Err("unexpected Anthropic content block delta".into()),
                    }
                }
                "message_delta" => match event["delta"].get("stop_reason") {
                    Some(Value::String(reason)) => response.stop_reason.clone_from(reason),
                    Some(Value::Null) | None => {}
                    _ => return Err("invalid Anthropic stop_reason".into()),
                },
                "message_stop" => {
                    saw_message_stop = true;
                    break;
                }
                "error" => {
                    let kind = event["error"]["type"].as_str().unwrap_or("unknown");
                    return Err(format!("Anthropic stream error: {}", kind.escape_debug()));
                }
                _ => {} // Includes pings and future event types.
            }
            if !text_to_write.is_empty() {
                wrote_text = true;
                output
                    .write_all(text_to_write.as_bytes())
                    .and_then(|()| output.flush())
                    .map_err(|_| "could not write response to stdout")?;
            }
        }
        if !saw_message_stop {
            return Err("stream ended before message_stop".into());
        }
        if block_open {
            return Err("message_stop arrived before content_block_stop".into());
        }
        finish(response)
    };

    read().map_err(|error| {
        if wrote_text {
            let _ = writeln!(output).and_then(|()| output.flush());
        }
        if error == OUTPUT_LIMIT_ERROR {
            error
        } else {
            format!("{error}; partial response discarded")
        }
    })
}

fn finish(response: MessageResponse) -> Result<AssistantResponse, String> {
    if response.kind != "message" || response.role != "assistant" {
        return Err("expected an Anthropic assistant message".into());
    }
    match response.stop_reason.as_str() {
        "end_turn" | "tool_use" => {}
        "max_tokens" => return Err(OUTPUT_LIMIT_ERROR.into()),
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
        assert_eq!(wire["stream"], true);
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
    }

    #[test]
    fn rejects_truncated_responses() {
        let mut body = response();
        body["stop_reason"] = json!("max_tokens");
        assert!(
            decode_response(&to_vec(&body).unwrap())
                .unwrap_err()
                .contains("partial response discarded")
        );
    }

    #[test]
    fn serialized_request_limit_includes_system() {
        let tools = ToolCatalog::without_workspace().definitions();
        assert!(
            encode_request(
                &"x".repeat(MAX_REQUEST_BYTES * 2),
                &[Message::user("Hello".into())],
                &tools
            )
            .is_err()
        );
    }

    #[test]
    fn status_and_key_failures_are_explicit() {
        assert!(check_status(200).is_ok());
        assert!(check_status(401).unwrap_err().contains("authentication"));
        assert!(validate_key("").is_err());
    }

    // Encode a synthetic event stream.
    fn sse(events: &[Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for event in events {
            let kind = event["type"].as_str().unwrap();
            write!(bytes, "event: {kind}\ndata: {event}\n\n").unwrap();
        }
        bytes
    }

    #[test]
    fn stream_matches_decoder() {
        let bytes = sse(&[
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo "}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"ping"}),
            json!({"type":"synthetic_future_event"}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"世界!"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            json!({"type":"message_stop"}),
        ]);
        let mut output = Vec::new();
        let streamed = read_stream(&bytes[..], &mut output).unwrap();
        let decoded = decode_response(&to_vec(&response()).unwrap()).unwrap();
        assert_eq!(
            to_value(&streamed.content).unwrap(),
            to_value(&decoded.content).unwrap()
        );
        assert_eq!(streamed.tool_call, decoded.tool_call);
        assert_eq!(output, "Hello 世界!".as_bytes());
    }

    #[test]
    fn tool_input_is_assembled() {
        let start = [
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{
                "type":"tool_use","id":"toolu_synthetic","name":"read_file","input":{}
            }}),
        ];
        let deltas = [
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"th\":\"a"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":".txt\"}"}}),
        ];
        let end = [
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
            json!({"type":"message_stop"}),
        ];
        for (deltas, expected) in [(&deltas[..], json!({"path":"a.txt"})), (&[][..], json!({}))] {
            let bytes = sse(&[&start[..], deltas, &end[..]].concat());
            let mut output = Vec::new();
            let streamed = read_stream(&bytes[..], &mut output).unwrap();
            assert_eq!(streamed.tool_call.unwrap().input, expected);
            assert!(output.is_empty());
        }
    }

    #[test]
    fn error_event_after_text() {
        let bytes = sse(&[
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}),
            json!({"type":"error","error":{"type":"overloaded_error"}}),
        ]);
        let mut output = Vec::new();
        let error = read_stream(&bytes[..], &mut output).unwrap_err();
        assert!(error.contains("overloaded_error"));
        assert!(error.contains("partial response discarded"));
        assert_eq!(output, b"partial\n");
    }

    #[test]
    fn eof_before_message_stop() {
        let bytes = sse(&[
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ]);
        let mut output = Vec::new();
        let error = read_stream(&bytes[..], &mut output).unwrap_err();
        assert!(error.contains("message_stop"));
        assert!(output.is_empty());
    }

    #[test]
    fn max_tokens_discards() {
        let bytes = sse(&[
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
            json!({"type":"message_stop"}),
        ]);
        let mut output = Vec::new();
        let error = read_stream(&bytes[..], &mut output).unwrap_err();
        assert_eq!(error, OUTPUT_LIMIT_ERROR);
        assert_eq!(output, b"partial\n");
    }

    #[test]
    fn invalid_tool_json_and_wrong_index() {
        let start = [
            json!({"type":"message_start","message":{"type":"message","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{
                "type":"tool_use","id":"toolu_synthetic","name":"read_file","input":{}
            }}),
        ];
        let invalid_json = [
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_stop","index":0}),
        ];
        let wrong_index = [
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        ];
        for (tail, expected) in [
            (&invalid_json[..], "invalid tool input JSON"),
            (&wrong_index[..], "unexpected Anthropic content block index"),
        ] {
            let bytes = sse(&[&start[..], tail].concat());
            let mut output = Vec::new();
            let error = read_stream(&bytes[..], &mut output).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(output.is_empty());
        }
    }

    #[test]
    fn body_limit_is_enforced() {
        let mut bytes =
            sse(&[json!({"type":"message_start","message":{"type":"message","role":"assistant"}})]);
        let comment = b": ping\n";
        bytes.extend(comment.repeat(MAX_RESPONSE_BYTES as usize / comment.len() + 1));
        let mut body = ureq::Body::builder().data(bytes);
        let mut output = Vec::new();
        let error = read_body(&mut body, &mut output).unwrap_err();
        assert!(error.contains("1 MiB"), "{error}");
        assert!(output.is_empty());
    }
}

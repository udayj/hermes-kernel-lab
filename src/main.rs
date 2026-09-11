use serde::{Deserialize, Serialize};
use std::{env, ffi::OsString, fs::File, io::Write, process::ExitCode, time::Duration};
use ureq::http::{HeaderValue, Request};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 512;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: &'a [TextMessage<'a>],
    tools: serde_json::Value,
    tool_choice: serde_json::Value,
}

#[derive(Serialize)]
struct TextMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessageResponse {
    #[serde(rename = "type")]
    kind: String,
    role: String,
    content: Vec<ContentBlock>,
    stop_reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: EchoInput,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EchoInput {
    text: String,
}

#[derive(Debug)]
enum Response {
    Text(String),
    ToolRequested(Vec<ContentBlock>),
}

fn user_messages(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(String, Option<String>), String> {
    let message = args
        .next()
        .ok_or("usage: hermes-kernel-lab \"first message\" [\"follow-up message\"]")?;
    let follow_up = args.next();
    if args.next().is_some() {
        return Err("expected one or two quoted user messages".into());
    }
    Ok((
        validate_message(message)?,
        follow_up.map(validate_message).transpose()?,
    ))
}

fn validate_message(message: OsString) -> Result<String, String> {
    let message = message
        .into_string()
        .map_err(|_| "user message must be valid Unicode")?;
    if message.trim().is_empty() {
        return Err("user message must not be blank".into());
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("user message exceeds the 16 KiB limit".into());
    }
    Ok(message)
}

fn key_from_dotenv(reader: impl std::io::Read) -> Result<String, String> {
    let mut key = None;
    for entry in dotenvy::from_read_iter(reader) {
        // Parser errors may contain file contents, so never display them.
        let (name, value) = entry.map_err(|_| "could not parse .env; check its syntax")?;
        if name == "ANTHROPIC_API_KEY" {
            if key.is_some() {
                return Err(".env contains duplicate ANTHROPIC_API_KEY entries".into());
            }
            key = Some(value);
        }
    }
    key.ok_or_else(|| "ANTHROPIC_API_KEY is missing from .env".into())
}

fn api_key() -> Result<String, String> {
    match env::var("ANTHROPIC_API_KEY") {
        Ok(key) => Ok(key),
        Err(env::VarError::NotUnicode(_)) => Err("ANTHROPIC_API_KEY must be valid Unicode".into()),
        Err(env::VarError::NotPresent) => {
            let file = File::open(".env").map_err(|_| {
                "set ANTHROPIC_API_KEY in the environment or a readable .env in the current directory"
            })?;
            key_from_dotenv(file)
        }
    }
}

fn build_request(messages: &[TextMessage<'_>], key: &str) -> Result<Request<Vec<u8>>, String> {
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("ANTHROPIC_API_KEY must be nonempty ASCII without whitespace".into());
    }
    let mut header = HeaderValue::from_str(key).map_err(|_| "invalid API key header")?;
    header.set_sensitive(true);
    let body = serde_json::to_vec(&MessageRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        stream: false,
        messages,
        tools: serde_json::json!([{
            "name": "echo",
            "description": "Repeat the supplied text.",
            "input_schema": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }
        }]),
        tool_choice: serde_json::json!({"type": "auto", "disable_parallel_tool_use": true}),
    })
    .map_err(|_| "could not encode the request as JSON")?;
    Request::post(ENDPOINT)
        .header("x-api-key", header)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body)
        .map_err(|_| "could not construct the HTTP request".into())
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

fn decode_response(body: &[u8]) -> Result<Response, String> {
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
    let mut text = String::new();
    let mut tool_count = 0;
    for block in &response.content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::ToolUse { id, name, .. } => {
                if id.is_empty()
                    || !id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    return Err("tool request has an invalid call ID".into());
                }
                if name != "echo" {
                    return Err("tool request names an unadvertised tool".into());
                }
                tool_count += 1;
                if tool_count > 1 {
                    return Err("response contains multiple tool requests".into());
                }
            }
            ContentBlock::Unsupported => {
                return Err("response contains unsupported non-text content".into());
            }
        }
    }
    if (response.stop_reason == "tool_use") != (tool_count == 1) {
        return Err("response stop_reason is inconsistent with tool request content".into());
    }
    if tool_count == 1 {
        return Ok(Response::ToolRequested(response.content));
    }
    if text.trim().is_empty() {
        return Err("response contains no usable text".into());
    }
    Ok(Response::Text(text))
}

fn transport_error(error: ureq::Error) -> String {
    // Do not include raw errors that might contain headers or response data.
    match error {
        ureq::Error::Timeout(_) => "Anthropic request timed out (60-second limit)",
        ureq::Error::BodyExceedsLimit(_) => "Anthropic response exceeds the 1 MiB limit",
        _ => "Anthropic transport failed; check connectivity, TLS, and proxy settings",
    }
    .into()
}

fn send(client: &ureq::Agent, messages: &[TextMessage<'_>], key: &str) -> Result<Response, String> {
    let request = build_request(messages, key)?;
    let mut response = client.run(request).map_err(transport_error)?;
    check_status(response.status().as_u16())?;
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(transport_error)?;
    decode_response(&body)
}

fn write_response(
    output: &mut impl Write,
    response: &Response,
    follow_up_pending: bool,
) -> Result<(), String> {
    let mut write = || -> std::io::Result<()> {
        match response {
            Response::Text(text) => writeln!(output, "{text}"),
            Response::ToolRequested(blocks) => {
                writeln!(output, "Tool requested, not executed (task incomplete):")?;
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => write!(output, "{text}")?,
                        ContentBlock::ToolUse { id, name, input } => {
                            writeln!(output, "\nCall ID: {id}\nName: {name}")?;
                            write!(output, "Arguments: ")?;
                            serde_json::to_writer(&mut *output, input)?;
                            writeln!(output)?;
                        }
                        ContentBlock::Unsupported => unreachable!("decoder validates content"),
                    }
                }
                writeln!(output)?;
                if follow_up_pending {
                    writeln!(output, "Supplied follow-up was not sent.")?;
                }
                Ok(())
            }
        }
    };
    write().map_err(|_| "could not write response to stdout".into())
}

fn run() -> Result<(), String> {
    let (first, follow_up) = user_messages(env::args_os().skip(1))?;
    let key = api_key()?;
    let client: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut history = vec![TextMessage {
        role: "user",
        content: &first,
    }];
    let first_response = send(&client, &history, &key)?;
    let mut output = std::io::stdout().lock();
    write_response(&mut output, &first_response, follow_up.is_some())?;
    let Response::Text(first_response) = first_response else {
        return Ok(());
    };

    if let Some(follow_up) = follow_up {
        history.push(TextMessage {
            role: "assistant",
            content: &first_response,
        });
        history.push(TextMessage {
            role: "user",
            content: &follow_up,
        });
        let second_response = send(&client, &history, &key)?;
        write_response(&mut output, &second_response, false)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // All data below is synthetic, not captured from Anthropic.
    fn response() -> serde_json::Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"end_turn",
            "content":[{"type":"text", "text":"Hello "}, {"type":"text", "text":"世界!"}],
            "usage":{"input_tokens":8,"output_tokens":4}})
    }

    #[test]
    fn request_exposes_the_wire_contract() {
        let text = "  Say \"hello\"\n世界\\  ";
        let request = build_request(
            &[TextMessage {
                role: "user",
                content: text,
            }],
            "synthetic-key",
        )
        .unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.uri(), ENDPOINT);
        assert_eq!(request.headers()["x-api-key"], "synthetic-key");
        assert!(request.headers()["x-api-key"].is_sensitive());
        assert_eq!(request.headers()["anthropic-version"], "2023-06-01");
        assert_eq!(request.headers()["content-type"], "application/json");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(request.body()).unwrap(),
            json!({"model":"claude-haiku-4-5-20251001", "max_tokens":512,
                "stream":false,"messages":[{"role":"user","content":text}],
                "tools":[{"name":"echo","description":"Repeat the supplied text.",
                    "input_schema":{"type":"object","properties":{"text":{"type":"string"}},
                        "required":["text"],"additionalProperties":false}}],
                "tool_choice":{"type":"auto","disable_parallel_tool_use":true}})
        );
    }

    #[test]
    fn validates_input_and_key_without_echoing_them() {
        for args in [
            vec![],
            vec![" \n".into()],
            vec!["a".into(), "b".into(), "c".into()],
            vec!["x".repeat(MAX_MESSAGE_BYTES + 1).into()],
            vec!["a".into(), " \n".into()],
            vec!["a".into(), "é".repeat(MAX_MESSAGE_BYTES / 2 + 1).into()],
        ] {
            let result = user_messages(args.into_iter());
            assert!(result.is_err());
        }
        let text = " x ";
        assert_eq!(
            user_messages(vec![text.into()].into_iter()).unwrap(),
            (text.into(), None)
        );
        assert!(
            user_messages(
                vec![
                    "x".repeat(MAX_MESSAGE_BYTES).into(),
                    "é".repeat(MAX_MESSAGE_BYTES / 2).into()
                ]
                .into_iter()
            )
            .is_ok()
        );
        let messages = [TextMessage {
            role: "user",
            content: "hello",
        }];
        for key in ["", " ", "synthetic\nsecret", "synthetic\tsecret", "é"] {
            assert!(build_request(&messages, key).is_err());
        }
        assert!(
            !build_request(&messages, "synthetic\nsecret")
                .unwrap_err()
                .contains("synthetic")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_message_without_panicking() {
        use std::os::unix::ffi::OsStringExt;

        for args in [
            vec![OsString::from_vec(vec![0xff])],
            vec!["first".into(), OsString::from_vec(vec![0xff])],
        ] {
            let result = user_messages(args.into_iter());
            assert_eq!(result.unwrap_err(), "user message must be valid Unicode");
        }
    }

    #[test]
    fn dotenv_parsing_is_offline_and_errors_do_not_expose_contents() {
        assert_eq!(
            key_from_dotenv(b"# synthetic fixture\nANTHROPIC_API_KEY='synthetic-key'\n".as_slice())
                .unwrap(),
            "synthetic-key"
        );
        for file in [
            "OTHER=value",
            "ANTHROPIC_API_KEY=a\nANTHROPIC_API_KEY=b",
            "ANTHROPIC_API_KEY='synthetic-unclosed",
        ] {
            let error = key_from_dotenv(file.as_bytes()).unwrap_err();
            assert!(!error.contains("synthetic-unclosed"));
        }
    }

    #[test]
    fn decodes_text_blocks_in_order_and_ignores_metadata() {
        let decoded = decode_response(&serde_json::to_vec(&response()).unwrap()).unwrap();
        let Response::Text(text) = decoded else {
            panic!("expected text")
        };
        assert_eq!(text, "Hello 世界!");
    }

    #[test]
    fn stop_reason_is_required_by_the_non_streaming_schema() {
        let mut body = response();
        body.as_object_mut().unwrap().remove("stop_reason");
        assert!(
            decode_response(&serde_json::to_vec(&body).unwrap())
                .unwrap_err()
                .contains("schema")
        );
        for value in [json!(null), json!(42)] {
            body["stop_reason"] = value;
            assert!(
                decode_response(&serde_json::to_vec(&body).unwrap())
                    .unwrap_err()
                    .contains("schema")
            );
        }
    }

    #[test]
    fn decoder_rejects_unaccepted_stop_reasons() {
        for (reason, expected) in [
            (
                "max_tokens",
                "response reached the 512-token output limit; partial text withheld",
            ),
            ("refusal", "Anthropic refused the request"),
            (
                "unknown",
                "response did not finish with end_turn or tool_use",
            ),
        ] {
            let mut body = response();
            body["stop_reason"] = json!(reason);
            assert_eq!(
                decode_response(&serde_json::to_vec(&body).unwrap()).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn rejects_malformed_missing_empty_and_non_text_responses() {
        for body in [b"not json".as_slice(), b"{}"] {
            assert!(decode_response(body).is_err());
        }
        for content in [
            json!([]),
            json!([{"type":"text","text":"  "}]),
            json!([{"type":"text"}]),
            json!([{"type":"tool_use"}]),
            json!([{"type":"text","text":"partial"},{"type":"unknown"}]),
        ] {
            let mut body = response();
            body["content"] = content;
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        for (field, value) in [("role", json!("user")), ("type", json!("error"))] {
            let mut body = response();
            body[field] = value;
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
    }

    fn tool_response() -> serde_json::Value {
        json!({"type":"message", "role":"assistant", "stop_reason":"tool_use",
            "content":[{"type":"tool_use", "id":"toolu_synthetic", "name":"echo",
                "input":{"text":"hello"}}]})
    }

    #[test]
    fn inspects_tool_only_and_mixed_content_in_order() {
        for mixed in [false, true] {
            let mut body = tool_response();
            if mixed {
                let blocks = body["content"].as_array_mut().unwrap();
                blocks.insert(0, json!({"type":"text","text":"Before"}));
                blocks.push(json!({"type":"text","text":"After"}));
            }
            let decoded = decode_response(&serde_json::to_vec(&body).unwrap()).unwrap();
            assert!(matches!(decoded, Response::ToolRequested(_)));
            for pending in [false, true] {
                let mut output = Vec::new();
                write_response(&mut output, &decoded, pending).unwrap();
                let output = String::from_utf8(output).unwrap();
                assert!(output.contains("requested, not executed (task incomplete)"));
                assert!(output.contains("Call ID: toolu_synthetic\nName: echo"));
                assert!(output.contains(r#"Arguments: {"text":"hello"}"#));
                assert_eq!(output.contains("Supplied follow-up was not sent."), pending);
                if mixed {
                    assert!(output.find("Before").unwrap() < output.find("Call ID:").unwrap());
                    assert!(output.find("Arguments:").unwrap() < output.find("After").unwrap());
                }
            }
        }
    }

    #[test]
    fn rejects_malformed_tool_calls() {
        let valid = tool_response()["content"][0].clone();
        for (field, value) in [
            ("id", json!(null)),
            ("id", json!(42)),
            ("id", json!("")),
            ("id", json!(" ")),
            ("id", json!("bad\nID")),
            ("name", json!(null)),
            ("name", json!("other")),
            ("input", json!(null)),
            ("input", json!([])),
            ("input", json!({})),
            ("input", json!({"text":null})),
            ("input", json!({"text":42})),
            ("input", json!({"text":"hello","extra":true})),
        ] {
            let mut body = tool_response();
            body["content"][0][field] = value;
            assert!(
                decode_response(&serde_json::to_vec(&body).unwrap()).is_err(),
                "{field}"
            );
        }
        for field in ["id", "name", "input"] {
            let mut body = tool_response();
            body["content"][0].as_object_mut().unwrap().remove(field);
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        for content in [
            json!([valid, valid]),
            json!([valid, {"type":"unknown"}]),
            json!([valid, {"type":"text"}]),
        ] {
            let mut body = tool_response();
            body["content"] = content;
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        // Empty text is still a valid string argument for echo.
        let mut body = tool_response();
        body["content"][0]["input"]["text"] = json!("");
        assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_ok());
    }

    #[test]
    fn rejects_inconsistent_or_incomplete_tool_responses() {
        let mut text = response();
        text["stop_reason"] = json!("tool_use");
        assert!(
            decode_response(&serde_json::to_vec(&text).unwrap())
                .unwrap_err()
                .contains("inconsistent")
        );
        text["content"] = json!([]);
        assert!(decode_response(&serde_json::to_vec(&text).unwrap()).is_err());
        for reason in ["end_turn", "max_tokens", "refusal", "unknown"] {
            let mut body = tool_response();
            body["stop_reason"] = json!(reason);
            assert!(decode_response(&serde_json::to_vec(&body).unwrap()).is_err());
        }
    }

    #[test]
    fn text_output_is_unchanged_and_output_failures_are_reported() {
        let decoded = Response::Text("Hello 世界!".into());
        let mut output = Vec::new();
        write_response(&mut output, &decoded, true).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "Hello 世界!\n");
        let tool = decode_response(&serde_json::to_vec(&tool_response()).unwrap()).unwrap();
        for response in [decoded, tool] {
            assert_eq!(
                write_response(&mut &mut [0u8; 0][..], &response, true).unwrap_err(),
                "could not write response to stdout"
            );
        }
    }

    #[test]
    fn unsuccessful_statuses_and_transport_failures_are_clear() {
        assert!(check_status(200).is_ok());
        for status in [201, 500] {
            assert_eq!(
                check_status(status).unwrap_err(),
                format!("Anthropic HTTP {status}: unexpected response status")
            );
        }
        assert_eq!(
            check_status(401).unwrap_err(),
            "Anthropic HTTP 401: authentication failed; check ANTHROPIC_API_KEY"
        );
        assert!(
            transport_error(ureq::Error::BodyExceedsLimit(MAX_RESPONSE_BYTES)).contains("1 MiB")
        );
    }
}

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

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Unsupported,
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

fn decode_response(body: &[u8]) -> Result<String, String> {
    let response: MessageResponse = serde_json::from_slice(body)
        .map_err(|_| "Anthropic returned invalid JSON or an unexpected message schema")?;
    if response.kind != "message" || response.role != "assistant" {
        return Err("expected an Anthropic assistant message".into());
    }
    match response.stop_reason.as_str() {
        "end_turn" => {}
        "max_tokens" => {
            return Err(
                "response reached the 512-token output limit; partial text withheld".into(),
            );
        }
        "refusal" => return Err("Anthropic refused the request".into()),
        _ => return Err("response did not finish with end_turn".into()),
    }
    let mut text = String::new();
    for block in response.content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(&part),
            ContentBlock::Unsupported => {
                return Err("response contains unsupported non-text content".into());
            }
        }
    }
    if text.trim().is_empty() {
        return Err("response contains no usable text".into());
    }
    Ok(text)
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

fn send(client: &ureq::Agent, messages: &[TextMessage<'_>], key: &str) -> Result<String, String> {
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
    writeln!(output, "{first_response}").map_err(|_| "could not write response to stdout")?;

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
        writeln!(output, "{second_response}").map_err(|_| "could not write response to stdout")?;
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
                "stream":false,"messages":[{"role":"user","content":text}]})
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
        assert_eq!(
            decode_response(&serde_json::to_vec(&response()).unwrap()).unwrap(),
            "Hello 世界!"
        );
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
            ("unknown", "response did not finish with end_turn"),
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

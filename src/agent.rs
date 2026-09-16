use crate::{
    anthropic::{AssistantResponse, Client, ContentBlock, Message},
    tools::{ToolCatalog, ToolDefinition, ToolOutcome},
};
use std::io::Write;

const MAX_MODEL_CALLS_PER_TURN: usize = 8;

pub(crate) struct Agent {
    client: Client,
    history: Vec<Message>,
    tools: ToolCatalog,
}

impl Agent {
    pub(crate) fn new(key: String, tools: ToolCatalog) -> Result<Self, String> {
        Ok(Self {
            client: Client::new(key)?,
            history: Vec::new(),
            tools,
        })
    }

    pub(crate) fn run_turn(
        &mut self,
        message: String,
        output: &mut impl Write,
    ) -> Result<(), String> {
        run_turn(
            &mut self.history,
            &self.tools,
            message,
            output,
            |history, definitions| self.client.send(history, definitions),
        )
    }
}

fn run_turn(
    history: &mut Vec<Message>,
    tools: &ToolCatalog,
    message: String,
    output: &mut impl Write,
    mut model_call: impl FnMut(&[Message], &[ToolDefinition]) -> Result<AssistantResponse, String>,
) -> Result<(), String> {
    history.push(Message::user(message));
    for calls in 1..=MAX_MODEL_CALLS_PER_TURN {
        let definitions = tools.definitions();
        let response = model_call(history, &definitions)?;
        check_call_budget(&response, calls)?;
        write_assistant(output, &response)?;
        let tool_call = response.tool_call.clone();
        history.push(Message::assistant(response.content));

        let Some(call) = tool_call else {
            return Ok(());
        };
        let outcome = tools.execute(&call.name, &call.input);
        history.push(Message::tool_result(
            call.id.clone(),
            outcome.content.clone(),
            outcome.is_error,
        ));
        write_tool_outcome(output, &call.name, &call.id, &outcome)?;
    }
    unreachable!("call eight either ends the turn or fails before tool execution")
}

fn check_call_budget(response: &AssistantResponse, calls: usize) -> Result<(), String> {
    if response.tool_call.is_some() && calls >= MAX_MODEL_CALLS_PER_TURN {
        return Err(
            "model-call budget exhausted (8 calls per user turn); tool not executed".into(),
        );
    }
    Ok(())
}

fn write_assistant(output: &mut impl Write, response: &AssistantResponse) -> Result<(), String> {
    let mut write = || -> std::io::Result<()> {
        let mut has_text = false;
        for block in &response.content {
            if let ContentBlock::Text { text } = block {
                write!(output, "{text}")?;
                has_text = true;
            }
        }
        if has_text {
            writeln!(output)?;
        }
        output.flush()
    };
    write().map_err(|_| "could not write response to stdout".into())
}

fn write_tool_outcome(
    output: &mut impl Write,
    name: &str,
    id: &str,
    outcome: &ToolOutcome,
) -> Result<(), String> {
    let label = if outcome.is_error { "error" } else { "result" };
    let mut write = || -> std::io::Result<()> {
        writeln!(output, "[Local {name} {label}; call {id}]")?;
        write!(output, "{}", outcome.content)?;
        if !outcome.content.ends_with('\n') {
            writeln!(output)?;
        }
        output.flush()
    };
    write().map_err(|_| "could not write local tool result to stdout".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::ToolCall;
    use serde_json::{Value, json};
    use std::collections::VecDeque;

    fn response(tool_call: Option<ToolCall>) -> AssistantResponse {
        let mut content = vec![ContentBlock::Text {
            text: "Before".into(),
        }];
        if let Some(call) = &tool_call {
            content.push(ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }
        content.push(ContentBlock::Text {
            text: "After".into(),
        });
        AssistantResponse { content, tool_call }
    }

    // Synthetic decoded responses: this exercises orchestration, not HTTP decoding.
    struct Script {
        responses: VecDeque<Result<AssistantResponse, String>>,
        requests: Vec<(Value, Value)>,
    }

    impl Script {
        fn new(responses: impl IntoIterator<Item = Result<AssistantResponse, String>>) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                requests: Vec::new(),
            }
        }

        fn send(
            &mut self,
            history: &[Message],
            definitions: &[ToolDefinition],
        ) -> Result<AssistantResponse, String> {
            self.requests.push((json!(history), json!(definitions)));
            self.responses
                .pop_front()
                .expect("unexpected extra model request")
        }

        fn assert_requests(&self, histories: &[Value], tools: &ToolCatalog) {
            let definitions = json!(tools.definitions());
            let expected: Vec<_> = histories
                .iter()
                .map(|history| (history.clone(), definitions.clone()))
                .collect();
            assert_eq!(self.requests, expected);
        }
    }

    fn tool_response(id: &str, name: &str, input: Value) -> AssistantResponse {
        response(Some(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }))
    }

    fn user(text: &str) -> Value {
        json!({"role":"user", "content":[{"type":"text", "text":text}]})
    }

    fn answer() -> Value {
        json!({"role":"assistant", "content":[
            {"type":"text", "text":"Before"}, {"type":"text", "text":"After"}
        ]})
    }

    fn assistant_tool(id: &str, name: &str, input: Value) -> Value {
        json!({"role":"assistant", "content":[
            {"type":"text", "text":"Before"},
            {"type":"tool_use", "id":id, "name":name, "input":input},
            {"type":"text", "text":"After"}
        ]})
    }

    fn result(id: &str, content: &str, is_error: bool) -> Value {
        let mut block = json!({"type":"tool_result", "tool_use_id":id, "content":content});
        if is_error {
            block["is_error"] = json!(true);
        }
        json!({"role":"user", "content":[block]})
    }

    fn workspace() -> (tempfile::TempDir, ToolCatalog) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("fixture.txt"), "synthetic file\n").unwrap();
        let tools = ToolCatalog::open(Some(directory.path())).unwrap();
        (directory, tools)
    }

    #[test]
    fn scripted_direct_answer() {
        let tools = ToolCatalog::without_workspace();
        let mut script = Script::new([Ok(response(None))]);
        let mut history = Vec::new();
        let mut output = Vec::new();
        run_turn(&mut history, &tools, "Hello".into(), &mut output, |h, t| {
            script.send(h, t)
        })
        .unwrap();
        script.assert_requests(&[json!([user("Hello")])], &tools);
        assert!(script.responses.is_empty());
        assert_eq!(json!(history), json!([user("Hello"), answer()]));
        assert_eq!(output, b"BeforeAfter\n");
    }

    #[test]
    fn scripted_tool_result_and_next_turn_inherit_complete_history() {
        let (_directory, tools) = workspace();
        let input = json!({"path":"fixture.txt"});
        let mut script = Script::new([
            Ok(tool_response("read-1", "read_file", input.clone())),
            Ok(response(None)),
            Ok(response(None)),
        ]);
        let mut history = Vec::new();
        let mut output = Vec::new();
        run_turn(&mut history, &tools, "Read".into(), &mut output, |h, t| {
            script.send(h, t)
        })
        .unwrap();
        let mut expected = vec![
            user("Read"),
            assistant_tool("read-1", "read_file", input),
            result("read-1", "synthetic file\n", false),
        ];
        let after_tool = json!(expected);
        expected.push(answer());
        assert_eq!(json!(history), json!(expected));
        run_turn(&mut history, &tools, "Again".into(), &mut output, |h, t| {
            script.send(h, t)
        })
        .unwrap();
        expected.push(user("Again"));
        script.assert_requests(
            &[json!([user("Read")]), after_tool, json!(expected)],
            &tools,
        );
        expected.push(answer());
        assert_eq!(json!(history), json!(expected));
        assert!(script.responses.is_empty());
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "BeforeAfter\n[Local read_file result; call read-1]\nsynthetic file\nBeforeAfter\nBeforeAfter\n"
        );
    }

    #[test]
    fn scripted_tool_error_is_returned_and_model_recovers() {
        let tools = ToolCatalog::without_workspace();
        let input = json!({"extra":true});
        let mut script = Script::new([
            Ok(tool_response("bad-1", "get_runtime_info", input.clone())),
            Ok(response(None)),
        ]);
        let mut history = Vec::new();
        let mut output = Vec::new();
        run_turn(&mut history, &tools, "Try".into(), &mut output, |h, t| {
            script.send(h, t)
        })
        .unwrap();
        let mut expected = vec![
            user("Try"),
            assistant_tool("bad-1", "get_runtime_info", input),
            result(
                "bad-1",
                "get_runtime_info input must be exactly an empty object",
                true,
            ),
        ];
        script.assert_requests(&[json!([user("Try")]), json!(expected)], &tools);
        expected.push(answer());
        assert_eq!(json!(history), json!(expected));
        assert!(script.responses.is_empty());
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "BeforeAfter\n[Local get_runtime_info error; call bad-1]\nget_runtime_info input must be exactly an empty object\nBeforeAfter\n"
        );
    }

    #[test]
    fn scripted_budget_boundary_and_reset() {
        // Both successful results and tool errors consume the same turn budget.
        for tool_error in [false, true] {
            for final_tool in [false, true] {
                let (_directory, tools) = workspace();
                let input = if tool_error {
                    json!({})
                } else {
                    json!({"path":"fixture.txt"})
                };
                let content = if tool_error {
                    "tool input must contain exactly one field named path"
                } else {
                    "synthetic file\n"
                };
                let mut responses: Vec<_> = (1..=7)
                    .map(|i| {
                        Ok(tool_response(
                            &format!("read-{i}"),
                            "read_file",
                            input.clone(),
                        ))
                    })
                    .collect();
                responses.push(Ok(if final_tool {
                    tool_response("read-8", "read_file", input.clone())
                } else {
                    response(None)
                }));
                let mut script = Script::new(responses);
                let mut history = Vec::new();
                let mut output = Vec::new();
                let outcome =
                    run_turn(&mut history, &tools, "Start".into(), &mut output, |h, t| {
                        script.send(h, t)
                    });
                let mut expected = vec![user("Start")];
                let mut requests = vec![json!(expected)];
                for i in 1..=7 {
                    let id = format!("read-{i}");
                    expected.push(assistant_tool(&id, "read_file", input.clone()));
                    expected.push(result(&id, content, tool_error));
                    requests.push(json!(expected));
                }
                script.assert_requests(&requests, &tools);
                assert!(script.responses.is_empty());
                if final_tool {
                    assert_eq!(
                        outcome.unwrap_err(),
                        "model-call budget exhausted (8 calls per user turn); tool not executed"
                    );
                    assert_eq!(
                        String::from_utf8(output)
                            .unwrap()
                            .matches("BeforeAfter\n")
                            .count(),
                        7
                    );
                } else {
                    outcome.unwrap();
                    expected.push(answer());
                    assert_eq!(json!(history), json!(expected));
                    // A second full eight-call turn proves the counter resets.
                    script.responses.extend((1..=7).map(|i| {
                        Ok(tool_response(
                            &format!("next-{i}"),
                            "read_file",
                            input.clone(),
                        ))
                    }));
                    script.responses.push_back(Ok(response(None)));
                    run_turn(&mut history, &tools, "Next".into(), &mut output, |h, t| {
                        script.send(h, t)
                    })
                    .unwrap();
                    expected.push(user("Next"));
                    requests.push(json!(expected));
                    for i in 1..=7 {
                        let id = format!("next-{i}");
                        expected.push(assistant_tool(&id, "read_file", input.clone()));
                        expected.push(result(&id, content, tool_error));
                        requests.push(json!(expected));
                    }
                    expected.push(answer());
                    script.assert_requests(&requests, &tools);
                    assert!(script.responses.is_empty());
                }
                assert_eq!(json!(history), json!(expected));
            }
        }
    }

    #[test]
    fn scripted_model_failure_stops_requests() {
        let (_directory, tools) = workspace();
        for after_tool in [false, true] {
            let input = json!({"path":"fixture.txt"});
            let mut responses = Vec::new();
            if after_tool {
                responses.push(Ok(tool_response("read-1", "read_file", input.clone())));
            }
            responses.push(Err("synthetic model failure".into()));
            responses.push(Ok(response(None)));
            let mut script = Script::new(responses);
            let mut history = Vec::new();
            let error = run_turn(
                &mut history,
                &tools,
                "Start".into(),
                &mut Vec::new(),
                |h, t| script.send(h, t),
            )
            .unwrap_err();
            assert_eq!(error, "synthetic model failure");
            let mut expected = vec![user("Start")];
            let mut requests = vec![json!(expected)];
            if after_tool {
                expected.push(assistant_tool("read-1", "read_file", input));
                expected.push(result("read-1", "synthetic file\n", false));
                requests.push(json!(expected));
            }
            script.assert_requests(&requests, &tools);
            assert_eq!(script.responses.len(), 1);
            assert_eq!(json!(history), json!(expected));
        }
    }

    struct FailingOutput {
        completed_flushes: usize,
        fail_after_flushes: usize,
        fail_on_flush: bool,
    }

    impl Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.fail_on_flush && self.completed_flushes == self.fail_after_flushes {
                return Err(std::io::Error::other("synthetic write failure"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_on_flush && self.completed_flushes == self.fail_after_flushes {
                return Err(std::io::Error::other("synthetic flush failure"));
            }
            self.completed_flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn scripted_output_failures_stop_requests_at_history_boundary() {
        let (_directory, tools) = workspace();
        for fail_after_flushes in [0, 1] {
            for fail_on_flush in [false, true] {
                let input = json!({"path":"fixture.txt"});
                let mut script = Script::new([
                    Ok(tool_response("read-1", "read_file", input.clone())),
                    Ok(response(None)),
                ]);
                let mut output = FailingOutput {
                    completed_flushes: 0,
                    fail_after_flushes,
                    fail_on_flush,
                };
                let mut history = Vec::new();
                let error = run_turn(&mut history, &tools, "Start".into(), &mut output, |h, t| {
                    script.send(h, t)
                })
                .unwrap_err();
                let mut expected = vec![user("Start")];
                if fail_after_flushes == 0 {
                    assert_eq!(error, "could not write response to stdout");
                } else {
                    assert_eq!(error, "could not write local tool result to stdout");
                    expected.push(assistant_tool("read-1", "read_file", input));
                    expected.push(result("read-1", "synthetic file\n", false));
                }
                script.assert_requests(&[json!([user("Start")])], &tools);
                assert_eq!(script.responses.len(), 1);
                assert_eq!(json!(history), json!(expected));
            }
        }
    }

    #[test]
    fn budget_allows_call_eight_to_end_but_not_request_a_tool() {
        assert!(check_call_budget(&response(None), 8).is_ok());
        let tool_call = ToolCall {
            id: "call".into(),
            name: "get_runtime_info".into(),
            input: json!({}),
        };
        assert!(check_call_budget(&response(Some(tool_call.clone())), 7).is_ok());
        assert!(check_call_budget(&response(Some(tool_call)), 8).is_err());
    }

    #[test]
    fn output_distinguishes_assistant_text_and_local_results() {
        let mut output = Vec::new();
        write_assistant(&mut output, &response(None)).unwrap();
        write_tool_outcome(
            &mut output,
            "read_file",
            "call-1",
            &ToolOutcome {
                content: "file contents".into(),
                is_error: false,
            },
        )
        .unwrap();
        write_tool_outcome(
            &mut output,
            "read_file",
            "call-2",
            &ToolOutcome {
                content: "denied".into(),
                is_error: true,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "BeforeAfter\n[Local read_file result; call call-1]\nfile contents\n[Local read_file error; call call-2]\ndenied\n"
        );
    }

    #[test]
    fn output_failures_are_fatal() {
        assert!(write_assistant(&mut &mut [0_u8; 0][..], &response(None)).is_err());
        assert!(
            write_tool_outcome(
                &mut &mut [0_u8; 0][..],
                "tool",
                "call",
                &ToolOutcome {
                    content: "result".into(),
                    is_error: false,
                }
            )
            .is_err()
        );
    }
}

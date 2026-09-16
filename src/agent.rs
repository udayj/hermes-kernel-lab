use crate::{
    anthropic::{AssistantResponse, Client, ContentBlock, Message},
    tools::{ToolCatalog, ToolOutcome},
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
        self.history.push(Message::user(message));
        for calls in 1..=MAX_MODEL_CALLS_PER_TURN {
            let definitions = self.tools.definitions();
            let response = self.client.send(&self.history, &definitions)?;
            check_call_budget(&response, calls)?;
            write_assistant(output, &response)?;
            let tool_call = response.tool_call.clone();
            self.history.push(Message::assistant(response.content));

            let Some(call) = tool_call else {
                return Ok(());
            };
            let outcome = self.tools.execute(&call.name, &call.input);
            self.history.push(Message::tool_result(
                call.id.clone(),
                outcome.content.clone(),
                outcome.is_error,
            ));
            write_tool_outcome(output, &call.name, &call.id, &outcome)?;
        }
        unreachable!("call eight either ends the turn or fails before tool execution")
    }
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
    use serde_json::json;

    fn response(tool_call: Option<ToolCall>) -> AssistantResponse {
        AssistantResponse {
            content: vec![
                ContentBlock::Text {
                    text: "Before".into(),
                },
                ContentBlock::Text {
                    text: "After".into(),
                },
            ],
            tool_call,
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

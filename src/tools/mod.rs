mod workspace;

use self::workspace::Workspace;
use serde::Serialize;
use serde_json::{Value, json};
use std::{env, path::Path};

const RUNTIME_TOOL: &str = "get_runtime_info";
const LIST_TOOL: &str = "list_directory";
const READ_TOOL: &str = "read_file";

#[derive(Debug, Serialize)]
pub(crate) struct ToolDefinition {
    pub(crate) name: &'static str,
    description: &'static str,
    input_schema: Value,
}

#[derive(Debug)]
pub(crate) struct ToolOutcome {
    pub(crate) content: String,
    pub(crate) is_error: bool,
}

impl ToolOutcome {
    fn success(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    fn error(error: impl Into<String>) -> Self {
        Self {
            content: error.into(),
            is_error: true,
        }
    }
}

pub(crate) struct ToolCatalog {
    workspace: Option<Workspace>,
}

impl ToolCatalog {
    pub(crate) fn open(workspace: Option<&Path>) -> Result<Self, String> {
        let workspace = workspace.map(Workspace::open).transpose()?;
        Ok(Self { workspace })
    }

    #[cfg(test)]
    pub(crate) fn without_workspace() -> Self {
        Self { workspace: None }
    }

    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = vec![runtime_definition()];
        if self.workspace.is_some() {
            definitions.push(list_definition());
            definitions.push(read_definition());
        }
        definitions
    }

    pub(crate) fn execute(&self, name: &str, input: &Value) -> ToolOutcome {
        let result = match name {
            RUNTIME_TOOL => validate_empty_object(input).and_then(|()| runtime_info()),
            LIST_TOOL => self
                .workspace
                .as_ref()
                .ok_or_else(|| "list_directory is disabled; no workspace was authorized".into())
                .and_then(|workspace| exact_path(input).and_then(|path| workspace.list(&path))),
            READ_TOOL => self
                .workspace
                .as_ref()
                .ok_or_else(|| "read_file is disabled; no workspace was authorized".into())
                .and_then(|workspace| exact_path(input).and_then(|path| workspace.read(&path))),
            _ => Err("unknown or disabled tool".into()),
        };
        match result {
            Ok(content) => ToolOutcome::success(content),
            Err(error) => ToolOutcome::error(error),
        }
    }
}

fn runtime_definition() -> ToolDefinition {
    ToolDefinition {
        name: RUNTIME_TOOL,
        description: "Return the binary target OS and architecture, and an estimate of parallelism available to this process, not a physical-core count or current CPU load.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    }
}

fn list_definition() -> ToolDefinition {
    ToolDefinition {
        name: LIST_TOOL,
        description: "List one authorized workspace directory. Paths are workspace-relative; use '.' for the workspace root. The result is a sorted JSON array of names and entry types.",
        input_schema: path_schema(),
    }
}

fn read_definition() -> ToolDefinition {
    ToolDefinition {
        name: READ_TOOL,
        description: "Read one authorized workspace-relative regular UTF-8 text file, up to 32 KiB, preserving its contents.",
        input_schema: path_schema(),
    }
}

fn path_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
        "additionalProperties": false
    })
}

fn validate_empty_object(input: &Value) -> Result<(), String> {
    if input.as_object().is_some_and(|object| object.is_empty()) {
        Ok(())
    } else {
        Err("get_runtime_info input must be exactly an empty object".into())
    }
}

fn exact_path(input: &Value) -> Result<String, String> {
    let object = input
        .as_object()
        .ok_or_else(|| "tool input must be a JSON object".to_string())?;
    if object.len() != 1 || !object.contains_key("path") {
        return Err("tool input must contain exactly one field named path".into());
    }
    object["path"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "path must be a string".into())
}

#[derive(Serialize)]
struct RuntimeInfo {
    target_os: &'static str,
    target_arch: &'static str,
    available_parallelism: Option<usize>,
}

fn runtime_info() -> Result<String, String> {
    serde_json::to_string(&RuntimeInfo {
        target_os: env::consts::OS,
        target_arch: env::consts::ARCH,
        available_parallelism: std::thread::available_parallelism().map(|n| n.get()).ok(),
    })
    .map_err(|_| "could not encode runtime information".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn workspace() -> (TempDir, ToolCatalog) {
        let temporary = TempDir::new().unwrap();
        let catalog = ToolCatalog::open(Some(temporary.path())).unwrap();
        (temporary, catalog)
    }

    fn execute(catalog: &ToolCatalog, name: &str, input: Value) -> ToolOutcome {
        catalog.execute(name, &input)
    }

    #[test]
    fn definitions_match_enabled_dispatch() {
        let disabled = ToolCatalog::without_workspace();
        assert_eq!(
            disabled
                .definitions()
                .iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            vec![RUNTIME_TOOL]
        );
        assert!(!execute(&disabled, RUNTIME_TOOL, json!({})).is_error);
        assert!(execute(&disabled, LIST_TOOL, json!({"path":"."})).is_error);

        let (_temporary, enabled) = workspace();
        let names = enabled
            .definitions()
            .iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec![RUNTIME_TOOL, LIST_TOOL, READ_TOOL]);
        for name in names {
            let input = if name == RUNTIME_TOOL {
                json!({})
            } else if name == LIST_TOOL {
                json!({"path":"."})
            } else {
                json!({"path":"missing"})
            };
            assert_ne!(
                execute(&enabled, name, input).content,
                "unknown or disabled tool"
            );
        }
    }

    #[test]
    fn arguments_are_exact_and_invalid_arguments_do_not_touch_the_workspace() {
        let temporary = TempDir::new().unwrap();
        let missing = temporary.path().join("missing");
        let catalog = ToolCatalog::open(Some(temporary.path())).unwrap();
        for input in [
            json!(null),
            json!({}),
            json!({"path":null}),
            json!({"path":"missing","extra":true}),
        ] {
            assert!(execute(&catalog, READ_TOOL, input).is_error);
        }
        assert!(!missing.exists());
        assert!(execute(&catalog, RUNTIME_TOOL, json!({"extra":true})).is_error);
        assert!(execute(&catalog, "other", json!({})).is_error);
    }
}

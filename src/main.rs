mod agent;
mod anthropic;
mod bounded;
mod cli;
mod session;
mod tools;

use agent::{Agent, compose_instructions};
use bounded::{ReadError, read_bounded};
use clap::Parser;
use cli::{Cli, StdinEvent, read_stdin_event};
use session::{Checkpoint, Session};
use std::{
    collections::VecDeque,
    env::{var, var_os},
    fs::File,
    io::{IsTerminal, Write, stderr, stdin, stdout},
    path::Path,
    process::ExitCode,
};
use tools::ToolCatalog;

fn api_key() -> Result<String, String> {
    dotenvy::dotenv().ok();
    var("ANTHROPIC_API_KEY").map_err(|_| "set ANTHROPIC_API_KEY in the environment or .env".into())
}

fn run_stdin(
    tools: ToolCatalog,
    session: Session,
    checkpoint: Option<Checkpoint>,
    script: Option<VecDeque<serde_json::Value>>,
) -> Result<(), String> {
    let checkpoint = match checkpoint {
        Some(checkpoint) => checkpoint,
        None => {
            let home = var_os("HOME")
                .filter(|home| !home.is_empty())
                .ok_or("HOME must be set for automatic session saving")?;
            Checkpoint::automatic(Path::new(&home))?
        }
    };
    let mut error_output = stderr().lock();
    let agent_checkpoint_path = checkpoint.path().display().to_string();
    let mut agent = match script {
        Some(script) => Agent::from_script(script, tools, session, checkpoint),
        None => Agent::new(api_key()?, tools, session, checkpoint)?,
    };
    writeln!(
        error_output,
        "Session will be saved to {}",
        agent_checkpoint_path
    )
    .and_then(|()| error_output.flush())
    .map_err(|_| "could not print the resume path to stderr")?;
    let stdin = stdin();
    let show_prompt = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = stdout().lock();

    loop {
        if show_prompt {
            write!(error_output, "> ")
                .and_then(|()| error_output.flush())
                .map_err(|_| "could not write the input prompt to stderr")?;
        }
        match read_stdin_event(&mut input)? {
            StdinEvent::Blank => continue,
            StdinEvent::Exit | StdinEvent::Eof => return Ok(()),
            StdinEvent::Message(message) => agent.run_turn(message, &mut output)?,
        }
    }
}

fn load_instructions(tools: &ToolCatalog, path: Option<&Path>) -> Result<String, String> {
    let operator = if let Some(path) = path {
        Some(tools.read_instructions(path)?)
    } else {
        tools.default_instructions()?
    };
    Ok(compose_instructions(operator.as_deref()))
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let mut tools = ToolCatalog::open(
        cli.workspace.as_deref(),
        cli.skills_dir.as_deref(),
        cli.memory_dir.as_deref(),
    )?;
    let (session, checkpoint) = if let Some(path) = &cli.resume_session {
        let checkpoint = Checkpoint::open(path)?;
        let session = checkpoint
            .load()
            .map_err(|error| format!("{}: {error}", checkpoint.path().display()))?;
        (session, Some(checkpoint))
    } else {
        let system = load_instructions(&tools, cli.instructions.as_deref())?;
        (Session::new(system), None)
    };
    let script = if let Some(path) = &cli.offline_script {
        let file = File::open(path).map_err(|_| "could not open offline script")?;
        let bytes = read_bounded(file, 1024 * 1024).map_err(|error| match error {
            ReadError::Io => "could not read offline script",
            ReadError::TooLarge => "offline script exceeds 1 MiB",
        })?;
        Some(
            serde_json::from_slice::<VecDeque<serde_json::Value>>(&bytes).map_err(
                |_| "offline script must be a JSON array of synthetic provider messages",
            )?,
        )
    } else {
        None
    };
    tools.configure(cli.allow_workspace_writes, cli.allow_shell)?;
    run_stdin(tools, session, checkpoint, script)
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
    use std::fs::write;
    use tempfile::tempdir;

    #[test]
    fn startup_instruction_precedence_and_exact_text() {
        let directory = tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path()), None, None).unwrap();
        assert_eq!(
            load_instructions(&tools, None).unwrap(),
            compose_instructions(None)
        );
        write(directory.path().join("AGENTS.md"), "root default").unwrap();
        assert_eq!(
            load_instructions(&tools, None).unwrap(),
            compose_instructions(Some("root default"))
        );
        let text = "  synthetic operator text\n世界\n  ";
        write(directory.path().join("instructions.txt"), text).unwrap();
        assert_eq!(
            load_instructions(&tools, Some(Path::new("instructions.txt"))).unwrap(),
            compose_instructions(Some(text))
        );
    }

    #[test]
    fn explicit_instruction_failures_propagate_from_workspace_reads() {
        let directory = tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path()), None, None).unwrap();
        assert!(load_instructions(&tools, Some(Path::new("missing"))).is_err());
    }
}

mod agent;
mod anthropic;
mod cli;
mod session;
mod tools;

use agent::{Agent, compose_instructions};
use clap::Parser;
use cli::{Cli, StdinEvent, read_stdin_event};
use dotenvy::from_read_iter;
use session::{Checkpoint, Session};
use std::{
    env::{VarError, var, var_os},
    fs::File,
    io::{IsTerminal, Read, Write, stderr, stdin, stdout},
    path::Path,
    process::ExitCode,
};
use tools::ToolCatalog;

fn key_from_dotenv(reader: impl Read) -> Result<String, String> {
    let mut key = None;
    for entry in from_read_iter(reader) {
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
    match var("ANTHROPIC_API_KEY") {
        Ok(key) => Ok(key),
        Err(VarError::NotUnicode(_)) => Err("ANTHROPIC_API_KEY must be valid Unicode".into()),
        Err(VarError::NotPresent) => {
            let file = File::open(".env").map_err(|_| {
                "set ANTHROPIC_API_KEY in the environment or a readable .env in the current directory"
            })?;
            key_from_dotenv(file)
        }
    }
}

fn run_stdin(
    tools: ToolCatalog,
    session: Session,
    checkpoint: Option<Checkpoint>,
) -> Result<(), String> {
    let stdin = stdin();
    let show_prompt = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = stdout().lock();
    let mut error_output = stderr().lock();
    let mut startup = Some((tools, session, checkpoint));
    let mut agent = None;

    loop {
        if show_prompt {
            write!(error_output, "> ")
                .and_then(|()| error_output.flush())
                .map_err(|_| "could not write the input prompt to stderr")?;
        }
        match read_stdin_event(&mut input)? {
            StdinEvent::Blank => continue,
            StdinEvent::Exit | StdinEvent::Eof => return Ok(()),
            StdinEvent::Message(message) => {
                if agent.is_none() {
                    let key = api_key()?;
                    let (tools, session, checkpoint) =
                        startup.take().expect("agent is initialized only once");
                    let checkpoint = match checkpoint {
                        Some(checkpoint) => checkpoint,
                        None => {
                            let home = var_os("HOME")
                                .filter(|home| !home.is_empty())
                                .ok_or("HOME must be set for automatic session saving")?;
                            Checkpoint::automatic(Path::new(&home))?
                        }
                    };
                    agent = Some(Agent::new(key, tools, session, checkpoint)?);
                }
                if let Some(path) = agent
                    .as_mut()
                    .expect("agent was initialized")
                    .run_turn(message, &mut output)?
                {
                    writeln!(
                        error_output,
                        "Resume this session with --resume-session {}",
                        path.display()
                    )
                    .and_then(|()| error_output.flush())
                    .map_err(
                        |_| "checkpoint saved, but could not print its resume path to stderr",
                    )?;
                }
            }
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
    let tools = ToolCatalog::open(cli.workspace.as_deref(), cli.skills_dir.as_deref())?;
    let (session, checkpoint) = if let Some(path) = cli.resume_session {
        let checkpoint = Checkpoint::open(&path, true)?;
        (checkpoint.load()?, Some(checkpoint))
    } else {
        let system = load_instructions(&tools, cli.instructions.as_deref())?;
        (Session::new(system), None)
    };
    run_stdin(tools, session, checkpoint)
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
    use std::fs::{create_dir, remove_dir, remove_file, write};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn startup_instruction_precedence_and_exact_text() {
        let directory = tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path()), None).unwrap();
        assert_eq!(
            load_instructions(&tools, None).unwrap(),
            compose_instructions(None)
        );
        write(directory.path().join("AGENTS.md"), "root default").unwrap();
        assert_eq!(
            load_instructions(&tools, None).unwrap(),
            compose_instructions(Some("root default"))
        );
        for text in ["", "  synthetic operator text\n世界\n  "] {
            write(directory.path().join("instructions.txt"), text).unwrap();
            let system = load_instructions(&tools, Some(Path::new("instructions.txt"))).unwrap();
            assert_eq!(
                system,
                format!(
                    "{}\n\nOperator instructions:\n\n{text}",
                    compose_instructions(None)
                )
            );
        }
        let disabled = ToolCatalog::without_workspace();
        assert_eq!(
            load_instructions(&disabled, None).unwrap(),
            compose_instructions(None)
        );
        assert!(load_instructions(&disabled, Some(Path::new("instructions.txt"))).is_err());
        let default = directory.path().join("AGENTS.md");
        for bytes in [vec![0xff], vec![b'x'; 32 * 1024 + 1]] {
            write(&default, bytes).unwrap();
            assert!(load_instructions(&tools, None).is_err());
        }
        remove_file(&default).unwrap();
        create_dir(&default).unwrap();
        assert!(load_instructions(&tools, None).is_err());
        remove_dir(&default).unwrap();
        #[cfg(unix)]
        {
            symlink("missing", &default).unwrap();
            assert!(load_instructions(&tools, None).is_err());
        }
    }

    #[test]
    fn explicit_instruction_failures_propagate_from_workspace_reads() {
        let directory = tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path()), None).unwrap();
        write(directory.path().join("invalid.bin"), [0xff]).unwrap();
        // Exhaustive filesystem restrictions belong to workspace tests.
        for path in ["missing", "invalid.bin", "../outside"] {
            assert!(load_instructions(&tools, Some(Path::new(path))).is_err());
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
}

mod agent;
mod anthropic;
mod cli;
mod tools;

use agent::Agent;
use clap::Parser;
use cli::{Cli, StdinEvent};
use std::{
    env,
    fs::File,
    io::{IsTerminal, Write},
    process::ExitCode,
};
use tools::ToolCatalog;

fn key_from_dotenv(reader: impl std::io::Read) -> Result<String, String> {
    let mut key = None;
    for entry in dotenvy::from_read_iter(reader) {
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

fn run_once(message: String, tools: ToolCatalog) -> Result<(), String> {
    let key = api_key()?;
    let mut agent = Agent::new(key, tools)?;
    let mut output = std::io::stdout().lock();
    agent.run_turn(message, &mut output)
}

fn run_stdin(tools: ToolCatalog) -> Result<(), String> {
    let stdin = std::io::stdin();
    let show_prompt = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = std::io::stdout().lock();
    let mut error_output = std::io::stderr().lock();
    let mut tools = Some(tools);
    let mut agent = None;

    loop {
        if show_prompt {
            write!(error_output, "> ")
                .and_then(|()| error_output.flush())
                .map_err(|_| "could not write the input prompt to stderr")?;
        }
        match cli::read_stdin_event(&mut input)? {
            StdinEvent::Blank => continue,
            StdinEvent::Exit | StdinEvent::Eof => return Ok(()),
            StdinEvent::Message(message) => {
                if agent.is_none() {
                    let key = api_key()?;
                    agent = Some(Agent::new(
                        key,
                        tools.take().expect("agent is initialized only once"),
                    )?);
                }
                agent
                    .as_mut()
                    .expect("agent was initialized")
                    .run_turn(message, &mut output)?;
            }
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let message = cli.message.map(cli::validate_message).transpose()?;
    let tools = ToolCatalog::open(cli.workspace.as_deref())?;
    match message {
        Some(message) => run_once(message, tools),
        None => run_stdin(tools),
    }
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

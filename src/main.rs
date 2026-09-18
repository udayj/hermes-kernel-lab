mod agent;
mod anthropic;
mod cli;
mod tools;

use agent::{Agent, compose_instructions};
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

fn run_once(message: String, tools: ToolCatalog, system: String) -> Result<(), String> {
    let key = api_key()?;
    let mut agent = Agent::new(key, tools, system)?;
    let mut output = std::io::stdout().lock();
    agent.run_turn(message, &mut output)
}

fn run_stdin(tools: ToolCatalog, system: String) -> Result<(), String> {
    let stdin = std::io::stdin();
    let show_prompt = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = std::io::stdout().lock();
    let mut error_output = std::io::stderr().lock();
    let mut startup = Some((tools, system));
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
                    let (tools, system) = startup.take().expect("agent is initialized only once");
                    agent = Some(Agent::new(key, tools, system)?);
                }
                agent
                    .as_mut()
                    .expect("agent was initialized")
                    .run_turn(message, &mut output)?;
            }
        }
    }
}

fn load_instructions(
    tools: &ToolCatalog,
    path: Option<&std::path::Path>,
) -> Result<String, String> {
    let operator = path.map(|path| tools.read_instructions(path)).transpose()?;
    Ok(compose_instructions(operator.as_deref()))
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let message = cli.message.map(cli::validate_message).transpose()?;
    let tools = ToolCatalog::open(cli.workspace.as_deref())?;
    let system = load_instructions(&tools, cli.instructions.as_deref())?;
    match message {
        Some(message) => run_once(message, tools, system),
        None => run_stdin(tools, system),
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
    fn startup_loads_only_the_selected_file_and_preserves_text() {
        use std::{fs, path::Path};
        let directory = tempfile::tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path())).unwrap();
        fs::write(directory.path().join("AGENTS.md"), "not selected").unwrap();
        assert_eq!(
            load_instructions(&tools, None).unwrap(),
            compose_instructions(None)
        );
        for text in ["", "  synthetic operator text\n世界\n  "] {
            fs::write(directory.path().join("instructions.txt"), text).unwrap();
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
    }

    #[test]
    fn invalid_instruction_selections_fail_during_local_startup() {
        use std::{fs, path::Path};
        let directory = tempfile::tempdir().unwrap();
        let tools = ToolCatalog::open(Some(directory.path())).unwrap();
        fs::write(
            directory.path().join("large.txt"),
            vec![b'x'; 32 * 1024 + 1],
        )
        .unwrap();
        fs::write(directory.path().join("invalid.bin"), [0xff]).unwrap();
        fs::write(directory.path().join(".hidden"), "synthetic").unwrap();
        fs::create_dir(directory.path().join("directory")).unwrap();
        for path in [
            "",
            "missing",
            "large.txt",
            "invalid.bin",
            ".hidden",
            "../outside",
            "/absolute",
            "./relative",
            ".",
            "directory",
        ] {
            assert!(
                load_instructions(&tools, Some(Path::new(path))).is_err(),
                "accepted {path}"
            );
        }
        fs::write(directory.path().join("boundary.txt"), vec![b'x'; 32 * 1024]).unwrap();
        assert!(load_instructions(&tools, Some(Path::new("boundary.txt"))).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStringExt, fs::symlink};
            symlink("boundary.txt", directory.path().join("file-link")).unwrap();
            symlink("directory", directory.path().join("dir-link")).unwrap();
            fs::write(directory.path().join("directory/file.txt"), "synthetic").unwrap();
            for path in ["file-link", "dir-link/file.txt"] {
                assert!(load_instructions(&tools, Some(Path::new(path))).is_err());
            }
            let invalid = std::ffi::OsString::from_vec(vec![0xff]);
            assert!(load_instructions(&tools, Some(Path::new(&invalid))).is_err());
            let devices = ToolCatalog::open(Some(Path::new("/dev"))).unwrap();
            assert!(load_instructions(&devices, Some(Path::new("null"))).is_err());
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

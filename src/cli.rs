use clap::Parser;
use std::{
    io::{BufRead, Read},
    path::PathBuf,
};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(name = "hermes-kernel-lab")]
pub struct Cli {
    #[arg(long, value_name = "PATH")]
    pub workspace: Option<PathBuf>,
    /// Permit native workspace mutations and workspace writes by authorized shells.
    #[arg(long, requires = "workspace")]
    pub allow_workspace_writes: bool,
    /// Permit sandboxed, noninteractive shells (macOS only).
    #[arg(long, requires = "workspace")]
    pub allow_shell: bool,
    /// Consume synthetic provider responses from a JSON array without HTTP or credentials.
    #[arg(long, value_name = "PATH")]
    pub offline_script: Option<PathBuf>,
    /// Enable read-only procedures from one trusted local directory.
    #[arg(long, value_name = "PATH")]
    pub skills_dir: Option<PathBuf>,
    /// Enable persistent facts in one existing trusted directory.
    #[arg(long, value_name = "PATH")]
    pub memory_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        requires = "workspace",
        conflicts_with = "resume_session"
    )]
    pub instructions: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub resume_session: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum StdinEvent {
    Message(String),
    Blank,
    Exit,
    Eof,
}

pub fn read_stdin_event(reader: &mut impl BufRead) -> Result<StdinEvent, String> {
    let mut bytes = Vec::new();
    let bytes_read = reader
        .take(MAX_MESSAGE_BYTES as u64 + 3)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "could not read a user message from stdin")?;
    if bytes_read == 0 {
        return Ok(StdinEvent::Eof);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("user message exceeds the 16 KiB limit".into());
    }
    let message = String::from_utf8(bytes).map_err(|_| "user message must be valid Unicode")?;
    if message == "/exit" {
        return Ok(StdinEvent::Exit);
    }
    if message.trim().is_empty() {
        return Ok(StdinEvent::Blank);
    }
    Ok(StdinEvent::Message(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_stdin_options_and_rejects_missing_workspace() {
        let parsed = Cli::try_parse_from([
            "program",
            "--workspace",
            "example",
            "--instructions",
            "instructions.txt",
        ])
        .unwrap();
        assert_eq!(parsed.instructions, Some(PathBuf::from("instructions.txt")));
        assert!(Cli::try_parse_from(["program", "--instructions", "instructions.txt"]).is_err());
    }

    #[test]
    fn stdin_is_line_oriented_and_preserves_non_terminator_whitespace() {
        let mut input = Cursor::new(b"  hello  \r\n\n/exit\n");
        assert_eq!(
            read_stdin_event(&mut input).unwrap(),
            StdinEvent::Message("  hello  ".into())
        );
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Blank);
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Exit);
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Eof);
        let mut oversized = Cursor::new("x".repeat(MAX_MESSAGE_BYTES * 2));
        assert!(read_stdin_event(&mut oversized).is_err());
    }
}

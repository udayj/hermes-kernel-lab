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
    /// Enable read-only procedures from one trusted local directory.
    #[arg(long, value_name = "PATH")]
    pub skills_dir: Option<PathBuf>,
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

pub fn validate_text(message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("user message must not be blank".into());
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("user message exceeds the 16 KiB limit".into());
    }
    Ok(())
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
    use std::{io::Cursor, iter::once};

    #[test]
    fn parses_stdin_options_and_rejects_removed_modes() {
        assert!(Cli::try_parse_from(["program"]).is_ok());
        let parsed = Cli::try_parse_from([
            "program",
            "--workspace",
            "example",
            "--instructions",
            "instructions.txt",
        ])
        .unwrap();
        assert_eq!(parsed.instructions, Some(PathBuf::from("instructions.txt")));
        assert!(Cli::try_parse_from(["program", "--resume-session", "session.json"]).is_ok());
        let parsed = Cli::try_parse_from([
            "program",
            "--skills-dir",
            "skills",
            "--resume-session",
            "session.json",
        ])
        .unwrap();
        assert_eq!(parsed.skills_dir, Some(PathBuf::from("skills")));
        assert!(parsed.workspace.is_none());
        for args in [
            vec!["Hello"],
            vec!["--save-session", "session.json"],
            vec!["--no-project-instructions"],
            vec!["--instructions", "instructions.txt"],
            vec![
                "--workspace",
                ".",
                "--instructions",
                "AGENTS.md",
                "--resume-session",
                "session.json",
            ],
        ] {
            assert!(Cli::try_parse_from(once("program").chain(args)).is_err());
        }
    }

    #[test]
    fn stdin_is_line_oriented_and_preserves_non_terminator_whitespace() {
        let mut input = Cursor::new(b"  hello  \r\n\n/exit\nignored\n");
        assert_eq!(
            read_stdin_event(&mut input).unwrap(),
            StdinEvent::Message("  hello  ".into())
        );
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Blank);
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Exit);
    }

    #[test]
    fn stdin_handles_eof_unicode_invalid_input_and_bounds() {
        let mut input = Cursor::new("最後".as_bytes());
        assert_eq!(
            read_stdin_event(&mut input).unwrap(),
            StdinEvent::Message("最後".into())
        );
        assert_eq!(read_stdin_event(&mut input).unwrap(), StdinEvent::Eof);

        let mut invalid = Cursor::new(vec![0xff, b'\n']);
        assert!(read_stdin_event(&mut invalid).is_err());

        let mut exact = Cursor::new(format!("{}\r\n", "x".repeat(MAX_MESSAGE_BYTES)));
        assert!(matches!(
            read_stdin_event(&mut exact).unwrap(),
            StdinEvent::Message(_)
        ));
        let mut oversized = Cursor::new(format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES + 1)));
        assert!(read_stdin_event(&mut oversized).is_err());
    }
}

use clap::Parser;
use std::{
    ffi::OsString,
    io::{BufRead, Read},
    path::PathBuf,
};

pub(crate) const MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(name = "hermes-kernel-lab")]
pub(crate) struct Cli {
    #[arg(long, value_name = "PATH")]
    pub(crate) workspace: Option<PathBuf>,
    #[arg(long, value_name = "PATH", requires = "workspace", conflicts_with_all = ["no_project_instructions", "resume_session"])]
    pub(crate) instructions: Option<PathBuf>,
    #[arg(long, conflicts_with = "resume_session")]
    pub(crate) no_project_instructions: bool,
    #[arg(long, value_name = "PATH", conflicts_with = "resume_session")]
    pub(crate) save_session: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(crate) resume_session: Option<PathBuf>,
    #[arg(value_name = "MESSAGE")]
    pub(crate) message: Option<OsString>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum StdinEvent {
    Message(String),
    Blank,
    Exit,
    Eof,
}

pub(crate) fn validate_message(message: OsString) -> Result<String, String> {
    let message = message
        .into_string()
        .map_err(|_| "user message must be valid Unicode")?;
    validate_text(&message)?;
    Ok(message)
}

pub(crate) fn validate_text(message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("user message must not be blank".into());
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("user message exceeds the 16 KiB limit".into());
    }
    Ok(())
}

pub(crate) fn read_stdin_event(reader: &mut impl BufRead) -> Result<StdinEvent, String> {
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
    fn parses_interactive_and_one_shot_shapes() {
        let parsed = Cli::try_parse_from(["program"]).unwrap();
        assert!(parsed.message.is_none());
        assert!(parsed.workspace.is_none());

        let parsed = Cli::try_parse_from([
            "program",
            "--workspace",
            "example",
            "one message with spaces",
        ])
        .unwrap();
        assert_eq!(parsed.workspace, Some(PathBuf::from("example")));
        assert_eq!(parsed.message, Some("one message with spaces".into()));

        assert!(Cli::try_parse_from(["program", "one", "two"]).is_err());
    }

    #[test]
    fn instruction_and_session_flags_enforce_requirements_and_conflicts() {
        assert!(Cli::try_parse_from(["program", "--instructions", "instructions.txt"]).is_err());
        let parsed = Cli::try_parse_from([
            "program",
            "--workspace",
            "example",
            "--instructions",
            "instructions.txt",
        ])
        .unwrap();
        assert_eq!(parsed.instructions, Some(PathBuf::from("instructions.txt")));
        for args in [
            vec![
                "--save-session",
                "new.json",
                "--resume-session",
                "saved.json",
            ],
            vec![
                "--resume-session",
                "saved.json",
                "--no-project-instructions",
            ],
            vec![
                "--resume-session",
                "saved.json",
                "--workspace",
                ".",
                "--instructions",
                "AGENTS.md",
            ],
            vec![
                "--workspace",
                ".",
                "--instructions",
                "AGENTS.md",
                "--no-project-instructions",
            ],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("program").chain(args)).is_err());
        }
    }

    #[test]
    fn validates_one_shot_message_without_changing_whitespace() {
        let text = "  meaningful text  ";
        assert_eq!(validate_message(text.into()).unwrap(), text);
        for message in [
            " ".into(),
            "\n".into(),
            "x".repeat(MAX_MESSAGE_BYTES + 1).into(),
        ] {
            assert!(validate_message(message).is_err());
        }
        assert!(validate_message("x".repeat(MAX_MESSAGE_BYTES).into()).is_ok());
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

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_one_shot_message() {
        use std::os::unix::ffi::OsStringExt;
        assert!(validate_message(OsString::from_vec(vec![0xff])).is_err());
    }
}

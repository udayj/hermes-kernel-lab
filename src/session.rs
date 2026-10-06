use crate::anthropic::{ContentBlock, MODEL, Message};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, to_writer};
use std::{
    fs::{DirBuilder, read},
    io::Write,
    path::{Path, PathBuf},
    process::id,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;

const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    version: u32,
    provider: String,
    model: String,
    pub system: String,
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new(system: String) -> Self {
        Self {
            version: 1,
            provider: "anthropic".into(),
            model: MODEL.into(),
            system,
            messages: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.provider != "anthropic" || self.model != MODEL {
            return Err("unsupported checkpoint version, provider, or model".into());
        }
        if !self.messages.last().is_some_and(|message| {
            message.role == "assistant"
                && !message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
        }) {
            return Err("checkpoint does not end at a completed turn".into());
        }
        Ok(())
    }
}

pub struct Checkpoint {
    path: PathBuf,
}

impl Checkpoint {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub fn automatic(home: &Path) -> Result<Self, String> {
        if !home.is_absolute() || !home.is_dir() {
            return Err("HOME must name an existing absolute directory".into());
        }
        let directory = home.join(".hermes-kernel-lab/sessions");
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .map_err(|_| "could not create session directory")?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is before the Unix epoch")?
            .as_nanos();
        let path = directory.join(format!("{timestamp}-{}.json", id()));
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let name = path.file_name().ok_or("checkpoint must name a file")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = parent
            .canonicalize()
            .map_err(|_| "checkpoint parent must exist")?;
        if !parent.is_dir() {
            return Err("checkpoint parent must be a directory".into());
        }
        Ok(Self {
            path: parent.join(name),
        })
    }

    pub fn load(&self) -> Result<Session, String> {
        let bytes = read(&self.path).map_err(|_| "could not read checkpoint")?;
        if bytes.len() > MAX_SESSION_BYTES {
            return Err("checkpoint exceeds the 2 MiB limit".into());
        }
        let session: Session =
            from_slice(&bytes).map_err(|_| "invalid checkpoint JSON or message schema")?;
        session.validate()?;
        Ok(session)
    }

    pub fn save(&self, session: &Session) -> Result<(), String> {
        session.validate()?;
        // tempfile creates private files (0600 on Unix). The parent is trusted;
        // concurrent writers or replacement of the directory are unsupported.
        let mut staged = NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|_| "could not stage checkpoint")?;
        let mut buffer = vec![0; MAX_SESSION_BYTES];
        let mut remaining = buffer.as_mut_slice();
        to_writer(&mut remaining, session)
            .map_err(|_| "could not encode checkpoint within the 2 MiB limit")?;
        let used = MAX_SESSION_BYTES - remaining.len();
        staged
            .write_all(&buffer[..used])
            .map_err(|_| "could not write checkpoint")?;
        staged
            .flush()
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|_| "could not flush/sync checkpoint")?;
        staged
            .persist(&self.path)
            .map_err(|_| "could not publish checkpoint")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::to_value;
    use tempfile::tempdir;

    fn completed() -> Session {
        let mut session = Session::new("synthetic system".into());
        session.messages = vec![
            Message::user("Hello".into()),
            Message::assistant(vec![ContentBlock::Text {
                text: "Synthetic answer".into(),
            }]),
        ];
        session
    }

    #[test]
    fn automatic_sessions_save_and_restore() {
        let home = tempdir().unwrap();
        let checkpoint = Checkpoint::automatic(home.path()).unwrap();
        assert!(!checkpoint.path().exists());
        let session = completed();
        checkpoint.save(&session).unwrap();
        let restored = Checkpoint::open(checkpoint.path()).unwrap().load().unwrap();
        assert_eq!(to_value(restored).unwrap(), to_value(session).unwrap());
    }

    #[test]
    fn rejects_incomplete_checkpoints_without_publication() {
        let directory = tempdir().unwrap();
        let checkpoint = Checkpoint::open(&directory.path().join("session.json")).unwrap();
        let mut session = completed();
        session.messages.pop();
        assert!(
            checkpoint
                .save(&session)
                .unwrap_err()
                .contains("completed turn")
        );
        assert!(!checkpoint.path().exists());
        std::fs::write(checkpoint.path(), serde_json::to_vec(&session).unwrap()).unwrap();
        assert!(checkpoint.load().unwrap_err().contains("completed turn"));
    }
}

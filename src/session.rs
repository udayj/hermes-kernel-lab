use crate::anthropic::{ContentBlock, MODEL, Message, validate_assistant_content};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Session {
    version: u32,
    provider: String,
    model: String,
    pub(crate) system: String,
    pub(crate) messages: Vec<Message>,
}

impl Session {
    pub(crate) fn new(system: String) -> Self {
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
        // Only the sequence produced by our single-tool turn loop is resumable.
        let mut expect_user = true;
        let mut pending = None;
        let mut calls = 0;
        for message in &self.messages {
            if let Some(id) = pending.take() {
                match (message.role.as_str(), message.content.as_slice()) {
                    ("user", [ContentBlock::ToolResult { tool_use_id, .. }])
                        if *tool_use_id == id => {}
                    _ => return Err("checkpoint has an unmatched tool result".into()),
                }
            } else if expect_user {
                match (message.role.as_str(), message.content.as_slice()) {
                    ("user", [ContentBlock::Text { text }]) => {
                        crate::cli::validate_message(text.clone().into())?;
                    }
                    _ => return Err("checkpoint expected a user text message".into()),
                }
                expect_user = false;
                calls = 0;
            } else {
                if message.role != "assistant" {
                    return Err("checkpoint expected an assistant message".into());
                }
                calls += 1;
                if let Some(call) = validate_assistant_content(&message.content)? {
                    if calls >= crate::agent::MAX_MODEL_CALLS_PER_TURN {
                        return Err("checkpoint exceeds the completed-turn call budget".into());
                    }
                    pending = Some(call.id);
                } else {
                    expect_user = true;
                }
            }
        }
        if self.messages.is_empty() || !expect_user || pending.is_some() {
            return Err("checkpoint does not end at a completed turn".into());
        }
        Ok(())
    }
}

pub(crate) struct Checkpoint {
    path: PathBuf,
    existing: bool,
}

impl Checkpoint {
    pub(crate) fn open(path: &Path, existing: bool) -> Result<Self, String> {
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
        let checkpoint = Self {
            path: parent.join(name),
            existing,
        };
        checkpoint.check_destination()?;
        Ok(checkpoint)
    }

    fn check_destination(&self) -> Result<(), String> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if self.existing && metadata.is_file() => Ok(()),
            Err(error) if !self.existing && error.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(if self.existing {
                "checkpoint must be an existing regular file, not a symlink"
            } else {
                "new checkpoint destination must not exist"
            }
            .into()),
        }
    }

    pub(crate) fn load(&self) -> Result<Session, String> {
        self.check_destination()?;
        let parent = Dir::open_ambient_dir(self.path.parent().unwrap(), ambient_authority())
            .map_err(|_| "could not open checkpoint parent")?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = parent
            .open_with(self.path.file_name().unwrap(), &options)
            .map_err(|_| "could not open checkpoint without following symlinks")?;
        if !file
            .metadata()
            .map_err(|_| "could not inspect checkpoint")?
            .is_file()
        {
            return Err("checkpoint must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_SESSION_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "could not read checkpoint")?;
        if bytes.len() > MAX_SESSION_BYTES {
            return Err("checkpoint exceeds the 2 MiB limit".into());
        }
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid checkpoint JSON or message schema")?;
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn save(&mut self, session: &Session) -> Result<(), String> {
        session.validate()?;
        self.check_destination()?;
        // tempfile creates private files (0600 on Unix). The parent is trusted;
        // concurrent writers or replacement of the directory are unsupported.
        let mut staged = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|_| "could not stage checkpoint")?;
        serde_json::to_writer(
            LimitedWriter {
                file: staged.as_file_mut(),
                remaining: MAX_SESSION_BYTES,
            },
            session,
        )
        .map_err(|_| "could not encode/write checkpoint within the 2 MiB limit")?;
        staged
            .flush()
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|_| "could not flush/sync checkpoint")?;
        self.check_destination()?;
        let publication = if self.existing {
            staged.persist(&self.path)
        } else {
            staged.persist_noclobber(&self.path)
        };
        publication.map_err(|_| "could not publish checkpoint")?;
        // No fallible operation after publication. Directory durability across
        // power loss is deliberately not promised.
        self.existing = true;
        Ok(())
    }
}

struct LimitedWriter<'a> {
    file: &'a mut File,
    remaining: usize,
}

impl Write for LimitedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("checkpoint size limit"));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn rejects_incompatible_malformed_and_incomplete_checkpoints() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        let valid = serde_json::to_value(completed()).unwrap();
        let call = json!({"role":"assistant","content":[
            {"type":"text","text":"Before"},
            {"type":"tool_use","id":"call-1","name":"unknown","input":{}}
        ]});
        let result = json!({"role":"user","content":[
            {"type":"tool_result","tool_use_id":"call-1","content":"denied","is_error":true}
        ]});
        let user = valid["messages"][0].clone();
        let answer = valid["messages"][1].clone();
        let mut cases = Vec::new();
        for (field, value) in [
            ("version", json!(2)),
            ("provider", json!("other")),
            ("model", json!("other")),
            ("system", json!(null)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            cases.push(invalid);
        }
        for messages in [
            json!([]),
            json!([user]),
            json!([answer]),
            json!([user, call]),
            json!([user, call, result]),
            json!([user, result, answer]),
            json!([user, call, answer]),
            json!([user, answer, answer]),
            json!([{"role":"system","content":[{"type":"text","text":"bad"}]}]),
            json!([user, {"role":"assistant","content":[{"type":"unknown"}]}]),
            json!([user, {"role":"assistant","content":[{"type":"text","text":3}]}]),
            json!([user, {"role":"assistant","content":[{"type":"text","text":" "}]}]),
        ] {
            let mut invalid = valid.clone();
            invalid["messages"] = messages;
            cases.push(invalid);
        }
        let mut correlated = valid.clone();
        correlated["messages"] = json!([user, call, result, answer]);
        for (pointer, value) in [
            ("/messages/2/content/0/tool_use_id", json!("wrong")),
            ("/messages/2/content/0/is_error", json!("true")),
            ("/messages/1/content/1/id", json!("bad id")),
            ("/messages/1/content/1/input", json!(null)),
        ] {
            let mut invalid = correlated.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            cases.push(invalid);
        }
        for invalid in cases {
            fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(
                Checkpoint::open(&path, true).unwrap().load().is_err(),
                "accepted {invalid}"
            );
        }
        for invalid in [b"{".to_vec(), vec![0xff], vec![b' '; MAX_SESSION_BYTES + 1]] {
            fs::write(&path, invalid).unwrap();
            assert!(Checkpoint::open(&path, true).unwrap().load().is_err());
        }
        fs::write(&path, serde_json::to_vec(&correlated).unwrap()).unwrap();
        assert!(Checkpoint::open(&path, true).unwrap().load().is_ok());
    }

    #[test]
    fn publication_refuses_destinations_and_enforces_private_bounded_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        assert!(Checkpoint::open(&directory.path().join("missing/session.json"), false).is_err());
        assert!(Checkpoint::open(directory.path(), true).is_err());
        assert!(Checkpoint::open(&path, true).is_err());
        let mut checkpoint = Checkpoint::open(&path, false).unwrap();
        assert!(!path.exists());
        let mut session = completed();
        let overhead = serde_json::to_vec(&session).unwrap().len() - session.system.len();
        session.system = "x".repeat(MAX_SESSION_BYTES - overhead);
        checkpoint.save(&session).unwrap();
        let previous = fs::read(&path).unwrap();
        assert_eq!(previous.len(), MAX_SESSION_BYTES);
        assert_eq!(checkpoint.load().unwrap().system, session.system);
        assert!(Checkpoint::open(&path, false).is_err());
        session.system.push('x');
        assert!(checkpoint.save(&session).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        let late = directory.path().join("late.json");
        let mut new = Checkpoint::open(&late, false).unwrap();
        fs::write(&late, "existing").unwrap();
        assert!(new.save(&completed()).is_err());
        assert_eq!(fs::read_to_string(&late).unwrap(), "existing");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = directory.path().join("link");
            symlink(&path, &link).unwrap();
            assert!(Checkpoint::open(&link, true).is_err());
            assert!(Checkpoint::open(&link, false).is_err());
            assert!(Checkpoint::open(Path::new("/dev/null"), true).is_err());
            fs::remove_file(&path).unwrap();
            assert!(Checkpoint::open(&link, true).is_err());
            assert!(Checkpoint::open(&link, false).is_err());
        }
    }
}

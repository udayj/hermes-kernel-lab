use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::read,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const MAX_BYTES: usize = 32 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    entries: BTreeMap<String, String>,
}

pub(super) struct Memory {
    path: PathBuf,
    store: Store,
}

impl Memory {
    pub fn open(directory: &Path) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|_| "memory directory must exist")?;
        if !directory.is_dir() {
            return Err("memory directory must be a directory".into());
        }
        let mut memory = Self {
            path: directory.join("memory.json"),
            store: Store {
                version: 1,
                entries: BTreeMap::new(),
            },
        };
        let bytes = match read(&memory.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(memory),
            Err(_) => return Err("could not read memory.json".into()),
        };
        if bytes.len() > MAX_BYTES {
            return Err("memory.json exceeds 32 KiB".into());
        }
        memory.store =
            serde_json::from_slice(&bytes).map_err(|_| "invalid memory JSON or schema")?;
        encode(&memory.store)?;
        Ok(memory)
    }

    pub fn list(&self) -> Result<String, String> {
        serde_json::to_string(&self.store.entries)
            .map_err(|_| "could not encode memory listing".into())
    }

    pub fn set(&mut self, key: String, value: String) -> Result<String, String> {
        validate_text(&key, 64, "key")?;
        validate_text(&value, 1024, "value")?;
        if self.store.entries.get(&key) == Some(&value) {
            return Ok("Memory unchanged.".into());
        }
        let mut next = self.store.clone();
        next.entries.insert(key, value);
        self.commit(next)?;
        Ok("Memory saved.".into())
    }

    pub fn delete(&mut self, key: &str) -> Result<String, String> {
        validate_text(key, 64, "key")?;
        if !self.store.entries.contains_key(key) {
            return Ok("Memory unchanged.".into());
        }
        let mut next = self.store.clone();
        next.entries.remove(key);
        self.commit(next)?;
        Ok("Memory deleted.".into())
    }

    fn commit(&mut self, next: Store) -> Result<(), String> {
        let bytes = encode(&next)?;
        // The directory is trusted and has one writer. No fallible work follows publication.
        let mut staged = NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|_| "could not stage memory")?;
        staged
            .write_all(&bytes)
            .map_err(|_| "could not write memory")?;
        staged
            .flush()
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|_| "could not sync memory")?;
        staged
            .persist(&self.path)
            .map_err(|_| "could not publish memory")?;
        self.store = next;
        Ok(())
    }
}

fn validate_text(text: &str, limit: usize, field: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > limit {
        return Err(format!(
            "memory {field} must be nonblank and at most {limit} UTF-8 bytes"
        ));
    }
    Ok(())
}

fn encode(store: &Store) -> Result<Vec<u8>, String> {
    if store.version != 1 {
        return Err("unsupported memory version".into());
    }
    if store.entries.len() > 32 {
        return Err("memory exceeds 32 entries".into());
    }
    for (key, value) in &store.entries {
        validate_text(key, 64, "key")?;
        validate_text(value, 1024, "value")?;
    }
    let bytes = serde_json::to_vec(store).map_err(|_| "could not encode memory")?;
    if bytes.len() > MAX_BYTES {
        return Err("serialized memory exceeds 32 KiB".into());
    }
    Ok(bytes)
}

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const MAX_BYTES: usize = 32 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    #[serde(with = "serde_with::rust::maps_duplicate_key_is_error")]
    entries: BTreeMap<String, String>,
}

pub(super) struct Memory {
    path: PathBuf,
    store: Store,
    existing: bool,
}

impl Memory {
    pub fn open(directory: &Path) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|_| "memory directory must exist")?;
        let root = Dir::open_ambient_dir(&directory, ambient_authority())
            .map_err(|_| "memory directory must be readable")?;
        let mut memory = Self {
            path: directory.join("memory.json"),
            store: Store {
                version: 1,
                entries: BTreeMap::new(),
            },
            existing: false,
        };
        match root.symlink_metadata("memory.json") {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(memory),
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err("memory.json must be a readable regular file, not a symlink".into()),
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = root
            .open_with("memory.json", &options)
            .map_err(|_| "could not open memory.json")?;
        if !file
            .metadata()
            .map_err(|_| "could not inspect memory.json")?
            .is_file()
        {
            return Err("memory.json must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "could not read memory.json")?;
        if bytes.len() > MAX_BYTES {
            return Err("memory.json exceeds 32 KiB".into());
        }
        memory.store =
            serde_json::from_slice(&bytes).map_err(|_| "invalid memory JSON or schema")?;
        encode(&memory.store)?;
        memory.existing = true;
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
        if self.existing {
            if !std::fs::symlink_metadata(&self.path)
                .map_err(|_| "could not inspect memory destination")?
                .is_file()
            {
                return Err("memory destination must remain a regular file".into());
            }
            staged.persist(&self.path)
        } else {
            staged.persist_noclobber(&self.path)
        }
        .map_err(|_| "could not publish memory")?;
        self.store = next;
        self.existing = true;
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

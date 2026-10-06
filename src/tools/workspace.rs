use crate::bounded::{ReadError, read_bounded};
use cap_std::{ambient_authority, fs::Dir};
use serde::Serialize;
use serde_json::{to_string, to_vec};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 32 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 200;
const MAX_DIRECTORY_OUTPUT_BYTES: usize = 32 * 1024;

pub struct ReadOnlyDirectory {
    root: Dir,
}

impl ReadOnlyDirectory {
    pub fn open(path: &Path) -> Result<Self, String> {
        let root = Dir::open_ambient_dir(path, ambient_authority())
            .map_err(|_| "root must be an existing readable directory")?;
        Ok(Self { root })
    }

    pub fn list(&self, path: &str) -> Result<String, String> {
        let entries = self.entries(path)?;
        to_string(&entries).map_err(|_| "could not encode the directory listing".into())
    }

    pub(super) fn entries(&self, path: &str) -> Result<Vec<DirectoryEntry>, String> {
        let path = self.resolve(Path::new(path))?;
        let directory = self
            .root
            .open_dir(path)
            .map_err(|_| "could not open the requested directory")?;
        let mut entries = Vec::new();
        let mut examined = 0;
        let mut encoded_bytes = 2;

        for entry in directory
            .entries()
            .map_err(|_| "could not enumerate the requested directory")?
        {
            examined += 1;
            if examined > MAX_DIRECTORY_ENTRIES {
                return Err("directory exceeds the 200-entry examination limit".into());
            }
            let entry = entry.map_err(|_| "could not examine a directory entry")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "directory contains a name that is not valid Unicode")?;
            if name.starts_with('.') {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|_| "could not determine a directory entry type")?;
            let kind = if file_type.is_file() {
                "file"
            } else if file_type.is_dir() {
                "directory"
            } else if file_type.is_symlink() {
                "symlink"
            } else {
                "other"
            };
            let item = DirectoryEntry { name, kind };
            let item_bytes = to_vec(&item)
                .map_err(|_| "could not encode the directory listing")?
                .len();
            encoded_bytes += item_bytes + usize::from(!entries.is_empty());
            if encoded_bytes > MAX_DIRECTORY_OUTPUT_BYTES {
                return Err("serialized directory listing exceeds the 32 KiB limit".into());
            }
            entries.push(item);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    pub fn default_instructions(&self) -> Result<Option<String>, String> {
        match self.root.symlink_metadata("AGENTS.md") {
            Ok(_) => self.read("AGENTS.md").map(Some),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err("could not inspect root AGENTS.md".into()),
        }
    }

    fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        check_path(path, false)?;
        let resolved = self
            .root
            .canonicalize(path)
            .map_err(|_| "could not resolve the requested workspace path")?;
        check_path(&resolved, false)?;
        Ok(resolved)
    }

    fn open_file(&self, path: &str) -> Result<cap_std::fs::File, String> {
        let path = self.resolve(Path::new(path))?;
        // Inspect before opening: opening a FIFO could otherwise block.
        if !self
            .root
            .metadata(&path)
            .map_err(|_| "could not inspect the requested file")?
            .is_file()
        {
            return Err("requested path is not a regular file".into());
        }
        self.root
            .open(path)
            .map_err(|_| "could not open the requested file".into())
    }

    pub fn read(&self, path: &str) -> Result<String, String> {
        let bytes =
            read_bounded(self.open_file(path)?, MAX_FILE_BYTES).map_err(|error| match error {
                ReadError::Io => "could not read the requested file",
                ReadError::TooLarge => "file exceeds the 32 KiB limit",
            })?;
        String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8 text".into())
    }
}

// Skills keep the read-only capability above. Workspace authority is invocation-local.
pub struct Workspace {
    pub root: PathBuf,
    directory: ReadOnlyDirectory,
    pub writable: bool,
}

impl Workspace {
    pub fn open(path: &Path) -> Result<Self, String> {
        let root = path.canonicalize().map_err(|_| "workspace must exist")?;
        if ["/", "/Users", "/private", "/private/tmp", "/private/var"]
            .iter()
            .any(|p| root == Path::new(p))
            || std::env::temp_dir().canonicalize().ok().as_deref() == Some(&root)
        {
            return Err("workspace must be a narrow project directory".into());
        }
        let directory = ReadOnlyDirectory::open(&root)?;
        Ok(Self {
            root,
            directory,
            writable: false,
        })
    }

    pub fn list(&self, path: &str) -> Result<String, String> {
        self.directory.list(path)
    }

    pub fn default_instructions(&self) -> Result<Option<String>, String> {
        self.directory.default_instructions()
    }

    pub fn instructions(&self, path: &str) -> Result<String, String> {
        self.directory.read(path)
    }

    pub fn read(&self, path: &str) -> Result<String, String> {
        self.directory.read(path)
    }

    pub fn read_lines(&self, path: &str, start: usize, end: usize) -> Result<String, String> {
        if start == 0 || end < start || end - start >= 1000 {
            return Err("line range must be one-based, ordered, and at most 1,000 lines".into());
        }
        let file = self.directory.open_file(path)?;
        let mut reader = BufReader::new(file.take(8 * 1024 * 1024 + 1));
        let mut scanned = 0;
        let mut output = String::new();
        let mut found = false;
        for number in 1..=end {
            let mut line = Vec::new();
            let size = reader
                .read_until(b'\n', &mut line)
                .map_err(|_| "could not read line range")?;
            scanned += size;
            if scanned > 8 * 1024 * 1024 {
                return Err("line range exceeds the 8 MiB scan limit".into());
            }
            if size == 0 {
                break;
            }
            let text = std::str::from_utf8(&line).map_err(|_| "file is not valid UTF-8 text")?;
            if number >= start {
                found = true;
                if output.len() + text.len() > MAX_FILE_BYTES as usize {
                    return Err("line range exceeds the 32 KiB output limit".into());
                }
                output.push_str(text);
            }
        }
        if !found {
            return Err("start_line is beyond end of file".into());
        }
        Ok(output)
    }

    pub fn patch(&self, path: &str, old: &str, new: &str) -> Result<String, String> {
        if old.is_empty() {
            return Err("old_text must not be empty".into());
        }
        let original = self.read(path)?;
        // Include overlapping occurrences: 'aaa' contains two matches for 'aa'.
        let matches: Vec<_> = original
            .char_indices()
            .filter_map(|(i, _)| original[i..].starts_with(old).then_some(i))
            .take(2)
            .collect();
        if matches.len() != 1 {
            return Err("patch requires exactly one occurrence of old_text".into());
        }
        let i = matches[0];
        let next = format!("{}{}{}", &original[..i], new, &original[i + old.len()..]);
        self.write(path, &next, true)
    }

    pub fn write(&self, path: &str, content: &str, overwrite: bool) -> Result<String, String> {
        if !self.writable {
            return Err("workspace writes are disabled".into());
        }
        if content.len() > MAX_FILE_BYTES as usize {
            return Err("write exceeds the 32 KiB file limit".into());
        }
        let path = Path::new(path);
        check_path(path, true)?;
        let leaf = path.file_name().ok_or("path must name a file")?;
        let target = match self.directory.root.canonicalize(path) {
            Ok(target) => {
                check_path(&target, true)?;
                if !self
                    .directory
                    .root
                    .metadata(&target)
                    .map_err(|_| "could not inspect destination")?
                    .is_file()
                {
                    return Err("destination must be a regular file".into());
                }
                target
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                self.directory.resolve(parent)?.join(leaf)
            }
            Err(_) => return Err("could not resolve workspace destination".into()),
        };
        check_path(&target, true)?;
        let destination = self.root.join(target);
        let mut staged = tempfile::NamedTempFile::new_in(destination.parent().unwrap())
            .map_err(|_| "could not stage workspace write")?;
        staged
            .write_all(content.as_bytes())
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|_| "could not synchronize workspace write")?;
        if overwrite {
            staged
                .persist(destination)
                .map_err(|_| "could not publish workspace write")?;
        } else {
            staged
                .persist_noclobber(destination)
                .map_err(|_| "destination exists or could not publish workspace write")?;
        }
        Ok("Workspace file published.".into())
    }
}

fn check_path(path: &Path, mutation: bool) -> Result<(), String> {
    for component in path.components() {
        if let Component::Normal(name) = component {
            if name.to_string_lossy().starts_with('.') {
                return Err("dot-prefixed path components are not accessible".into());
            }
            if mutation && name == "AGENTS.md" {
                return Err("AGENTS.md is read-only".into());
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub(super) struct DirectoryEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn workspace(root: &Path) -> Workspace {
        let mut workspace = Workspace::open(root).unwrap();
        workspace.writable = true;
        workspace
    }

    #[test]
    fn write_publishes_and_preserves_an_existing_file_on_failure() {
        let root = tempdir().unwrap();
        let workspace = workspace(root.path());
        workspace.write("file.txt", "amber", false).unwrap();
        assert!(workspace.write("file.txt", "bad", false).is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file.txt")).unwrap(),
            "amber"
        );
        fs::write(root.path().join("AGENTS.md"), "instructions").unwrap();
        assert!(workspace.write("AGENTS.md", "bad", true).is_err());
    }

    #[test]
    fn patch_replaces_one_match_and_preserves_content_on_ambiguity() {
        let root = tempdir().unwrap();
        let workspace = workspace(root.path());
        workspace.write("file.txt", "amber", false).unwrap();
        workspace.patch("file.txt", "amber", "blue").unwrap();
        assert_eq!(workspace.read("file.txt").unwrap(), "blue");
        workspace.write("file.txt", "aaa", true).unwrap();
        assert!(workspace.patch("file.txt", "aa", "bad").is_err());
        assert_eq!(workspace.read("file.txt").unwrap(), "aaa");
    }

    #[test]
    fn line_range_reads_selected_lines_and_rejects_invalid_bounds() {
        let root = tempdir().unwrap();
        let workspace = workspace(root.path());
        fs::write(root.path().join("file.txt"), "first\nsecond\nthird\n").unwrap();
        assert_eq!(
            workspace.read_lines("file.txt", 2, 3).unwrap(),
            "second\nthird\n"
        );
        assert!(workspace.read_lines("file.txt", 0, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn contained_symlinks_follow_the_target_protection_policy() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let workspace = workspace(root.path());
        fs::write(root.path().join("file.txt"), "content").unwrap();
        symlink("file.txt", root.path().join("alias")).unwrap();
        assert_eq!(workspace.read("alias").unwrap(), "content");
        workspace.write("alias", "updated", true).unwrap();
        assert_eq!(workspace.read("file.txt").unwrap(), "updated");
        fs::write(root.path().join(".hidden"), "hidden").unwrap();
        symlink(".hidden", root.path().join("hidden-alias")).unwrap();
        assert!(workspace.read("hidden-alias").is_err());
        fs::write(root.path().join("AGENTS.md"), "instructions").unwrap();
        symlink("AGENTS.md", root.path().join("agents-alias")).unwrap();
        assert!(workspace.write("agents-alias", "bad", true).is_err());
    }
}

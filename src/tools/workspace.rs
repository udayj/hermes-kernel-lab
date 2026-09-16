use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::Serialize;
use std::{
    io::Read,
    path::{Component, Path},
};

const MAX_FILE_BYTES: u64 = 32 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 200;
const MAX_DIRECTORY_OUTPUT_BYTES: usize = 32 * 1024;

pub(super) struct Workspace {
    root: Dir,
}

impl Workspace {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let root = Dir::open_ambient_dir(path, ambient_authority())
            .map_err(|_| "workspace must be an existing readable directory")?;
        Ok(Self { root })
    }

    pub(super) fn list(&self, path: &str) -> Result<String, String> {
        let components = validate_path(path, true)?;
        let directory = self.open_directory(&components)?;
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
            let item_bytes = serde_json::to_vec(&item)
                .map_err(|_| "could not encode the directory listing")?
                .len();
            encoded_bytes += item_bytes + usize::from(!entries.is_empty());
            if encoded_bytes > MAX_DIRECTORY_OUTPUT_BYTES {
                return Err("serialized directory listing exceeds the 32 KiB limit".into());
            }
            entries.push(item);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        serde_json::to_string(&entries).map_err(|_| "could not encode the directory listing".into())
    }

    pub(super) fn read(&self, path: &str) -> Result<String, String> {
        let components = validate_path(path, false)?;
        let (parent_components, leaf) = components.split_at(components.len() - 1);
        let parent = self.open_directory(parent_components)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = parent
            .open_with(&leaf[0], &options)
            .map_err(|_| "could not open the requested file")?;
        let metadata = file
            .metadata()
            .map_err(|_| "could not inspect the requested file")?;
        if !metadata.is_file() {
            return Err("requested path is not a regular file".into());
        }

        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "could not read the requested file")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("file exceeds the 32 KiB limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8 text".into())
    }

    fn open_directory(&self, components: &[String]) -> Result<Dir, String> {
        let mut directory = self
            .root
            .try_clone()
            .map_err(|_| "could not access the authorized workspace")?;
        for component in components {
            directory = directory
                .open_dir_nofollow(component)
                .map_err(|_| "could not open the requested directory without following symlinks")?;
        }
        Ok(directory)
    }
}

fn validate_path(path: &str, allow_root: bool) -> Result<Vec<String>, String> {
    if allow_root && path == "." {
        return Ok(Vec::new());
    }
    if path.is_empty() {
        return Err("path must not be empty".into());
    }
    let mut validated = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or_else(|| "path must be valid Unicode".to_string())?;
                if name.starts_with('.') {
                    return Err("dot-prefixed path components are not accessible".into());
                }
                validated.push(name.to_owned());
            }
            Component::CurDir => return Err("'.' is only valid as the workspace root".into()),
            Component::ParentDir => return Err("parent traversal is not allowed".into()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute paths are not allowed".into());
            }
        }
    }
    if validated.is_empty() {
        return Err("path must name a workspace entry".into());
    }
    Ok(validated)
}

#[derive(Serialize)]
struct DirectoryEntry {
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{fs, io::Write};
    use tempfile::TempDir;

    fn temporary_workspace() -> (TempDir, Workspace) {
        let temporary = TempDir::new().unwrap();
        let workspace = Workspace::open(temporary.path()).unwrap();
        (temporary, workspace)
    }

    #[test]
    fn lists_one_directory_in_sorted_order_and_marks_symlinks() {
        let (temporary, workspace) = temporary_workspace();
        fs::write(temporary.path().join("z.txt"), "z").unwrap();
        fs::create_dir(temporary.path().join("a-dir")).unwrap();
        fs::write(temporary.path().join(".env"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("z.txt", temporary.path().join("m-link")).unwrap();

        let entries: Value = serde_json::from_str(&workspace.list(".").unwrap()).unwrap();
        #[cfg(unix)]
        assert_eq!(
            entries,
            json!([
                {"name":"a-dir","type":"directory"},
                {"name":"m-link","type":"symlink"},
                {"name":"z.txt","type":"file"}
            ])
        );
        #[cfg(not(unix))]
        assert_eq!(
            entries,
            json!([
                {"name":"a-dir","type":"directory"},
                {"name":"z.txt","type":"file"}
            ])
        );
    }

    #[test]
    fn directory_limits_fail_instead_of_returning_partial_results() {
        let (temporary, workspace) = temporary_workspace();
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            fs::write(temporary.path().join(format!("entry-{index:03}")), "").unwrap();
        }
        let error = workspace.list(".").unwrap_err();
        assert!(error.contains("200-entry"));

        let (temporary, workspace) = temporary_workspace();
        let long = "x".repeat(240);
        for index in 0..150 {
            fs::write(temporary.path().join(format!("{index:03}-{long}")), "").unwrap();
        }
        let error = workspace.list(".").unwrap_err();
        assert!(error.contains("32 KiB"));
    }

    #[test]
    fn reads_exact_empty_unicode_and_boundary_contents() {
        let (temporary, workspace) = temporary_workspace();
        for (name, content) in [
            ("exact.txt", "  first\nsecond\n  ".to_string()),
            ("empty.txt", String::new()),
            ("unicode.txt", "नमस्ते 世界".to_string()),
            ("boundary.txt", "x".repeat(MAX_FILE_BYTES as usize)),
        ] {
            fs::write(temporary.path().join(name), &content).unwrap();
            assert_eq!(workspace.read(name).unwrap(), content);
        }
        fs::write(
            temporary.path().join("large.txt"),
            "x".repeat(MAX_FILE_BYTES as usize + 1),
        )
        .unwrap();
        assert!(workspace.read("large.txt").unwrap_err().contains("32 KiB"));
    }

    #[test]
    fn rejects_missing_invalid_utf8_and_denied_paths() {
        let (temporary, workspace) = temporary_workspace();
        fs::write(temporary.path().join("invalid.bin"), [0xff, 0xfe]).unwrap();
        fs::write(temporary.path().join(".hidden"), "hidden").unwrap();
        fs::create_dir(temporary.path().join("dir")).unwrap();
        for path in [
            "missing",
            "invalid.bin",
            ".hidden",
            "dir/.hidden",
            "../outside",
            "/absolute",
            "./relative",
            ".",
            "dir",
        ] {
            assert!(
                workspace.read(path).is_err(),
                "path {path} unexpectedly succeeded"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_traversal_and_special_files_on_the_tested_host() {
        let (temporary, workspace) = temporary_workspace();
        fs::create_dir(temporary.path().join("real-dir")).unwrap();
        fs::write(temporary.path().join("real-dir/file.txt"), "content").unwrap();
        std::os::unix::fs::symlink("real-dir", temporary.path().join("dir-link")).unwrap();
        std::os::unix::fs::symlink("real-dir/file.txt", temporary.path().join("file-link"))
            .unwrap();

        for path in ["dir-link/file.txt", "file-link"] {
            assert!(workspace.read(path).is_err());
        }
        assert!(workspace.list("dir-link").is_err());

        let devices = Workspace::open(Path::new("/dev")).unwrap();
        let error = devices.read("null").unwrap_err();
        assert!(error.contains("not a regular file"));
    }

    #[test]
    fn workspace_must_exist_and_be_a_directory() {
        let temporary = TempDir::new().unwrap();
        let file = temporary.path().join("file");
        fs::File::create(&file).unwrap().write_all(b"x").unwrap();
        assert!(Workspace::open(&file).is_err());
        assert!(Workspace::open(&temporary.path().join("missing")).is_err());
    }
}

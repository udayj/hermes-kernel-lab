use super::workspace::ReadOnlyDirectory;
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_SKILLS: usize = 20;
const MAX_LIST_BYTES: usize = 32 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    name: String,
    description: String,
}

struct Skill {
    metadata: Metadata,
    document: String,
}

pub(super) struct Skills {
    skills: Vec<Skill>,
    listing: String,
}

impl Skills {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::load(path).map_err(|error| format!("could not load skills catalog: {error}"))
    }

    fn load(path: &Path) -> Result<Self, String> {
        let directory = ReadOnlyDirectory::open(path)?;
        let mut skills = Vec::new();
        // Shared enumeration is bounded, sorted, and excludes hidden entries.
        for entry in directory.entries(".")? {
            match entry.kind {
                "file" => continue,
                "directory" => {}
                _ => return Err("skill root contains a symlink or special entry".into()),
            }
            validate_name(&entry.name)?;
            if skills.len() == MAX_SKILLS {
                return Err("catalog exceeds the 20-skill limit".into());
            }
            let document = directory.read(&format!("{}/SKILL.md", entry.name))?;
            let metadata = parse_metadata(&document)?;
            if metadata.name != entry.name {
                return Err("skill name must match its directory name".into());
            }
            skills.push(Skill { metadata, document });
        }
        let listing = serde_json::to_string(
            &skills
                .iter()
                .map(|skill| &skill.metadata)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| "could not encode skills listing")?;
        if listing.len() > MAX_LIST_BYTES {
            return Err("serialized skills listing exceeds the 32 KiB limit".into());
        }
        Ok(Self { skills, listing })
    }

    pub fn list(&self) -> &str {
        &self.listing
    }

    pub fn view(&self, name: &str) -> Result<&str, String> {
        validate_name(name)?;
        self.skills
            .iter()
            .find(|skill| skill.metadata.name == name)
            .map(|skill| skill.document.as_str())
            .ok_or_else(|| "unknown skill name".into())
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(
            "skill name must be 1-64 lowercase ASCII letters/digits with single interior hyphens"
                .into(),
        );
    }
    Ok(())
}

fn parse_metadata(document: &str) -> Result<Metadata, String> {
    let mut lines = document.split_inclusive('\n');
    let opening = lines.next().ok_or("skill is missing YAML frontmatter")?;
    if opening.trim_end_matches(['\r', '\n']) != "---" {
        return Err("skill must start with a --- frontmatter delimiter".into());
    }
    let mut end = opening.len();
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let metadata: Metadata = serde_yaml_ng::from_str(&document[opening.len()..end])
                .map_err(
                    |_| "skill metadata must contain exactly name and description text fields",
                )?;
            validate_name(&metadata.name)?;
            if metadata.description.trim().is_empty()
                || document[end + line.len()..].trim().is_empty()
            {
                return Err("skill description and Markdown body must not be blank".into());
            }
            return Ok(metadata);
        }
        end += line.len();
    }
    Err("skill is missing its closing --- frontmatter delimiter".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::{create_dir_all, write};
    use tempfile::tempdir;

    fn fixture(root: &Path, name: &str, description: &str, body: &str) -> String {
        create_dir_all(root.join(name)).unwrap();
        // JSON-quoted strings are also YAML strings, including escapes.
        let document = format!(
            "---\nname: {name}\ndescription: {}\n---\n{body}",
            json!(description)
        );
        write(root.join(name).join("SKILL.md"), &document).unwrap();
        document
    }

    #[test]
    fn sorted_metadata_and_selected_documents_are_snapshotted() {
        let root = tempdir().unwrap();
        assert_eq!(Skills::open(root.path()).unwrap().list(), "[]");
        let z = fixture(root.path(), "z", "Last", "Synthetic Z body\n");
        let a = fixture(root.path(), "a", "First", "Synthetic A body\n");
        create_dir_all(root.path().join(".hidden")).unwrap();
        write(root.path().join("README.md"), "ignored").unwrap();
        let skills = Skills::open(root.path()).unwrap();
        assert_eq!(
            skills.list(),
            r#"[{"name":"a","description":"First"},{"name":"z","description":"Last"}]"#
        );
        assert_eq!(skills.view("a").unwrap(), a);
        assert_eq!(skills.view("z").unwrap(), z);
        let changed = fixture(root.path(), "a", "New", "Synthetic edited body");
        assert_eq!(skills.view("a").unwrap(), a);
        assert_eq!(
            Skills::open(root.path()).unwrap().view("a").unwrap(),
            changed
        );
    }

    #[test]
    fn malformed_metadata_rejects_unknown_fields() {
        assert!(
            parse_metadata("---\nname: sample\ndescription: Synthetic\nextra: ignored\n---\nBody")
                .is_err()
        );
    }
}

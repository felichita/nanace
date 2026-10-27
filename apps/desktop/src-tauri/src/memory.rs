//! Read-only access to Claude Code memory files for the import dialog.
//!
//! Only `<home>/.claude/projects/*/memory/*.md` is read. Symlinks are skipped
//! so the scan cannot be pointed elsewhere, and sizes are capped.

use std::fs;
use std::path::Path;

use serde::Serialize;

const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_FILES: usize = 2000;
const INDEX_FILE: &str = "MEMORY.md";

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct MemoryFile {
    pub name: String,
    pub content: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct MemoryProject {
    /// Directory name under `projects/`, e.g. `-Users-me-Develop-team-apps`.
    pub id: String,
    /// Short readable name, e.g. `team-apps`.
    pub label: String,
    /// `MEMORY.md`, which maps files to titles.
    pub index: Option<String>,
    pub files: Vec<MemoryFile>,
}

/// `-Users-me-Develop-team-apps` -> `team-apps`: drops the home directory and
/// the first folder below it (`Develop`, `code`, ...) when more follows.
pub fn project_label(id: &str, home: &Path) -> String {
    let home_prefix = home.to_string_lossy().replace(['/', '.'], "-");
    let rest = id.strip_prefix(&home_prefix).unwrap_or(id).trim_start_matches('-');
    match rest.split_once('-') {
        Some((_, tail)) if !tail.is_empty() => tail.to_owned(),
        _ if rest.is_empty() => id.to_owned(),
        _ => rest.to_owned(),
    }
}

fn is_plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Projects under `<home>/.claude/projects` that have memory files, sorted by
/// label.
pub fn scan(home: &Path) -> Result<Vec<MemoryProject>, String> {
    let root = home.join(".claude").join("projects");
    if !is_plain_dir(&root) {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&root).map_err(|err| format!("cannot read {}: {err}", root.display()))?;
    let mut projects = Vec::new();
    let mut total = 0;
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path().join("memory");
        if !is_plain_dir(&entry.path()) || !is_plain_dir(&dir) {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else { continue };
        let mut project = MemoryProject {
            label: project_label(&id, home),
            id,
            index: None,
            files: Vec::new(),
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Ok(meta) = fs::symlink_metadata(file.path()) else {
                continue;
            };
            if !meta.is_file() || !name.ends_with(".md") || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(content) = fs::read_to_string(file.path()) else {
                continue;
            };
            if name == INDEX_FILE {
                project.index = Some(content);
            } else if total < MAX_FILES {
                total += 1;
                project.files.push(MemoryFile { name, content });
            }
        }
        if !project.files.is_empty() {
            project.files.sort_by(|a, b| a.name.cmp(&b.name));
            projects.push(project);
        }
    }
    projects.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(projects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_drop_home_and_top_folder() {
        let home = Path::new("/Users/me");
        assert_eq!(project_label("-Users-me-Develop-team-apps", home), "team-apps");
        assert_eq!(project_label("-Users-me-Develop", home), "Develop");
        assert_eq!(project_label("-Users-other-x", home), "other-x");
    }

    #[test]
    fn scans_only_memory_markdown_and_skips_symlinks() {
        let home = std::env::temp_dir().join(format!("nanace-memory-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let mem = home.join(".claude/projects/-x-Develop-app/memory");
        fs::create_dir_all(&mem).expect("mkdir");
        fs::write(mem.join("note.md"), "body").expect("write");
        fs::write(mem.join("MEMORY.md"), "- [Note](note.md)").expect("write");
        fs::write(mem.join("notes.txt"), "ignored").expect("write");
        fs::write(home.join("secret.md"), "outside").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.join("secret.md"), mem.join("link.md")).expect("symlink");
        fs::create_dir_all(home.join(".claude/projects/empty/memory")).expect("mkdir");

        let projects = scan(&home).expect("scan");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].index.as_deref(), Some("- [Note](note.md)"));
        assert_eq!(
            projects[0].files,
            vec![MemoryFile {
                name: "note.md".to_owned(),
                content: "body".to_owned()
            }]
        );
        let _ = fs::remove_dir_all(&home);
    }
}

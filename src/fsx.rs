use ignore::WalkBuilder;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for e in walker.flatten() {
        if e.depth() == 0 {
            continue;
        }
        let path = e.path().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = e
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(Entry { name, path, is_dir });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    out
}

pub fn find_files(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let q = query.to_lowercase();
    if q.is_empty() {
        return out;
    }
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != ".git" && n != "target" && n != "node_modules"
        })
        .build();
    for e in walker.flatten() {
        if out.len() >= limit {
            break;
        }
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains(&q) {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap_or_else(|_| e.path())
                    .to_string_lossy()
                    .to_string();
                out.push(rel);
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct LocalMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

pub fn search_local(root: &Path, query: &str, limit: usize) -> Vec<LocalMatch> {
    let mut out = Vec::new();
    let q = query.to_lowercase();
    if q.is_empty() {
        return out;
    }
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "target" && e.file_name() != "node_modules")
        .build();
    for e in walker.flatten() {
        if out.len() >= limit {
            break;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(true) {
            continue;
        }
        let path = e.path();
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() > 512 * 1024 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let path_str = path.to_string_lossy().to_string();
        for (i, line) in content.lines().enumerate() {
            if line.to_lowercase().contains(&q) {
                out.push(LocalMatch {
                    path: path_str.clone(),
                    line: (i + 1) as u64,
                    text: line.trim_end().to_string(),
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_files_matches_names_and_is_bounded() {
        let dir = std::env::temp_dir().join(format!("theta-fsx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "x").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "x").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();
        let hits = find_files(&dir, "main", 10);
        assert_eq!(hits, vec!["src/main.rs".to_string()]);
        let rs = find_files(&dir, ".rs", 1);
        assert_eq!(rs.len(), 1, "limit respected");
        assert!(find_files(&dir, "", 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_dir_sorts_dirs_first() {
        let dir = std::env::temp_dir().join(format!("theta-fsx-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("zzz_dir")).unwrap();
        std::fs::write(dir.join("aaa.txt"), "x").unwrap();
        let entries = list_dir(&dir);
        assert!(entries[0].is_dir, "directories sort first");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Filesystem helpers: directory listing (gitignore-aware) and local search.

use ignore::WalkBuilder;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// List one directory level, dirs first, gitignored entries skipped.
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
    out.sort_by(|a, b| match (b.is_dir, a.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    out
}

#[derive(Debug, Clone)]
pub struct LocalMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// Fallback content search when the OpenCode server search is unavailable.
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

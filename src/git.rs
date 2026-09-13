//! Minimal Git integration via the `git` CLI (async, cached).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub staged: u32,
    pub modified: u32,
    pub deleted: u32,
    pub untracked: u32,
}

impl GitInfo {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.staged > 0 {
            parts.push(format!("+{}", self.staged));
        }
        if self.modified > 0 {
            parts.push(format!("~{}", self.modified));
        }
        if self.deleted > 0 {
            parts.push(format!("-{}", self.deleted));
        }
        if self.untracked > 0 {
            parts.push(format!("?{}", self.untracked));
        }
        parts.join(" ")
    }

    pub fn dirty(&self) -> bool {
        self.staged + self.modified + self.deleted + self.untracked > 0
    }
}

pub struct GitCache {
    entries: std::collections::HashMap<PathBuf, (Instant, Option<GitInfo>)>,
    ttl: Duration,
}

impl GitCache {
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            ttl: Duration::from_secs(3),
        }
    }

    pub async fn get(&mut self, dir: &Path) -> Option<GitInfo> {
        if let Some((at, info)) = self.entries.get(dir) {
            if at.elapsed() < self.ttl {
                return info.clone();
            }
        }
        let info = query_status(dir).await.ok();
        self.entries
            .insert(dir.to_path_buf(), (Instant::now(), info.clone()));
        info
    }

    pub fn invalidate(&mut self, dir: &Path) {
        self.entries.remove(dir);
    }
}

async fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("git exited with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn query_status(dir: &Path) -> Result<GitInfo> {
    let text = run(
        dir,
        &["status", "--porcelain=v2", "--branch", "--untracked-files=normal"],
    )
    .await?;
    let mut info = GitInfo::default();
    for line in text.lines() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            info.branch = Some(b.trim().to_string());
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // XY fields: index status then worktree status
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 2 {
                let xy = fields[1];
                let xy: Vec<char> = xy.chars().collect();
                let x = xy.first().copied().unwrap_or('.');
                let y = xy.get(1).copied().unwrap_or('.');
                match x {
                    'A' => info.staged += 1,
                    'M' | 'R' | 'C' => info.staged += 1,
                    'D' => info.deleted += 1,
                    _ => match y {
                        'M' | 'R' | 'C' => info.modified += 1,
                        'D' => info.deleted += 1,
                        _ => {}
                    },
                }
                if y == 'M' && x != 'D' {
                    // counted above
                }
            }
        } else if line.starts_with("? ") {
            info.untracked += 1;
        }
    }
    if info.branch.is_none() {
        anyhow::bail!("not a git repo");
    }
    Ok(info)
}

pub async fn recent_commits(dir: &Path, n: u32) -> Result<Vec<String>> {
    let text = run(
        dir,
        &["log", "--pretty=%h %s", "-n", &n.to_string()],
    )
    .await?;
    Ok(text.lines().map(|l| l.to_string()).collect())
}

pub async fn file_diff(dir: &Path, file: &str) -> Result<String> {
    let mut diff = run(dir, &["diff", "HEAD", "--", file]).await?;
    if diff.trim().is_empty() {
        diff = run(dir, &["diff", "--cached", "--", file]).await?;
    }
    Ok(diff)
}

pub async fn workspace_diff(dir: &Path) -> Result<String> {
    let mut diff = run(dir, &["diff", "HEAD"]).await?;
    if diff.trim().is_empty() {
        diff = run(dir, &["diff", "--cached"]).await?;
    }
    Ok(diff)
}

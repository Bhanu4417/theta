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

pub async fn remote_repo(dir: &Path) -> Option<String> {
    let url = run(dir, &["remote", "get-url", "origin"]).await.ok()?;
    Some(parse_repo(url.trim()))
}

fn parse_repo(url: &str) -> String {
    let s = url.trim().trim_end_matches(".git");
    if let Some(rest) = s.strip_prefix("git@") {
        if let Some((_host, path)) = rest.split_once(':') {
            return path.to_string();
        }
    }
    if let Some((_scheme, rest)) = s.split_once("://") {
        if let Some((_host, path)) = rest.split_once('/') {
            return path.to_string();
        }
    }
    s.to_string()
}

fn auto_message(staged: &str) -> String {
    let files: Vec<&str> = staged.lines().filter(|l| !l.trim().is_empty()).collect();
    match files.len() {
        0 => "update".to_string(),
        1 => format!("update {}", files[0]),
        2 => format!("update {} and {}", files[0], files[1]),
        n => format!("update {n} files"),
    }
}

#[derive(Debug, Clone)]
pub struct PushOutcome {
    pub repo: String,
    pub subject: String,
}

pub async fn commit_and_push(dir: &Path, message: &str) -> Result<PushOutcome> {
    run(dir, &["add", "-A"]).await?;
    let staged = run(dir, &["diff", "--cached", "--name-only"]).await?;
    if staged.trim().is_empty() {
        anyhow::bail!("nothing to commit");
    }
    let subject = if message.trim().is_empty() {
        auto_message(&staged)
    } else {
        message.trim().to_string()
    };
    run(dir, &["commit", "-m", &subject]).await?;
    if run(dir, &["push"]).await.is_err() {
        run(dir, &["push", "-u", "origin", "HEAD"]).await?;
    }
    Ok(PushOutcome {
        repo: remote_repo(dir).await.unwrap_or_else(|| "remote".into()),
        subject,
    })
}

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
    /// Lines added and removed against `HEAD`, as `git diff --shortstat`
    /// reports them. These are what a reader recognizes as the work so far:
    /// counting changed *files* conflates a one-line typo fix with a rewritten
    /// module, and shows nothing at all for a single small edit.
    pub insertions: u32,
    pub deletions: u32,
    /// Commits ahead of and behind the upstream branch.
    pub ahead: u32,
    pub behind: u32,
}

impl GitInfo {
    /// The branch's state for the status bar: `↑2 ↓1 +48 -12 ?3`.
    ///
    /// Zero counts are omitted, so a clean checkout yields an empty string and
    /// the separator that would follow it is skipped too.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.ahead > 0 {
            parts.push(format!("↑{}", self.ahead));
        }
        if self.behind > 0 {
            parts.push(format!("↓{}", self.behind));
        }
        if self.insertions > 0 {
            parts.push(format!("+{}", self.insertions));
        }
        if self.deletions > 0 {
            parts.push(format!("-{}", self.deletions));
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
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            // "+2 -1": two ahead, one behind.
            let mut it = ab.split_whitespace();
            if let Some(a) = it.next().and_then(|v| v.strip_prefix('+')) {
                info.ahead = a.parse().unwrap_or(0);
            }
            if let Some(b) = it.next().and_then(|v| v.strip_prefix('-')) {
                info.behind = b.parse().unwrap_or(0);
            }
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
            }
        } else if line.starts_with("? ") {
            info.untracked += 1;
        }
    }
    if info.branch.is_none() {
        anyhow::bail!("not a git repo");
    }

    // Line counts against HEAD, covering staged and unstaged work. A repository
    // with no commits yet has no HEAD, so this is best effort and never fatal —
    // the branch still shows.
    if let Ok(stat) = run(dir, &["diff", "--shortstat", "HEAD"]).await {
        let (ins, del) = parse_shortstat(&stat);
        info.insertions = ins;
        info.deletions = del;
    }
    Ok(info)
}

/// Read `48 insertions(+), 12 deletions(-)` out of `git diff --shortstat`.
///
/// Either half can be absent — a pure addition has no deletions clause, and git
/// omits a clause whose count is zero — so the two are found independently
/// rather than assuming both appear.
pub fn parse_shortstat(text: &str) -> (u32, u32) {
    let mut insertions = 0u32;
    let mut deletions = 0u32;
    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if w.starts_with("insertion") {
            insertions = i
                .checked_sub(1)
                .and_then(|j| words[j].parse().ok())
                .unwrap_or(0);
        } else if w.starts_with("deletion") {
            deletions = i
                .checked_sub(1)
                .and_then(|j| words[j].parse().ok())
                .unwrap_or(0);
        }
    }
    (insertions, deletions)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortstat_parses_gits_actual_output_shapes() {
        // A single change uses the singular noun, and a clause whose count is
        // zero is omitted entirely, so both forms must be handled.
        assert_eq!(
            parse_shortstat(" 1 file changed, 48 insertions(+), 12 deletions(-)"),
            (48, 12)
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+), 1 deletion(-)"),
            (1, 1)
        );
        assert_eq!(parse_shortstat(" 1 file changed, 5 insertions(+)"), (5, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 3 deletions(-)"), (0, 3));
        assert_eq!(
            parse_shortstat(" 12 files changed, 1204 insertions(+), 88 deletions(-)"),
            (1204, 88)
        );
        assert_eq!(parse_shortstat(""), (0, 0));
    }

    #[test]
    fn summary_reads_like_a_reviewers_branch_state() {
        let mut info = GitInfo::default();
        // A clean checkout says nothing, so the separator after it is skipped.
        assert_eq!(info.summary(), "");

        info.branch = Some("main".into());
        info.insertions = 48;
        info.deletions = 12;
        assert_eq!(info.summary(), "+48 -12");

        info.ahead = 2;
        info.untracked = 3;
        assert_eq!(info.summary(), "↑2 +48 -12 ?3");

        info.behind = 1;
        assert_eq!(info.summary(), "↑2 ↓1 +48 -12 ?3");

        let only_deletions = GitInfo { deletions: 7, ..Default::default() };
        assert_eq!(only_deletions.summary(), "-7");
    }

    #[test]
    fn summary_shows_ahead_and_behind_without_any_diff() {
        // A committed-but-unpushed branch has no line changes to report, and
        // still needs to say that it is ahead.
        let info = GitInfo { ahead: 2, ..Default::default() };
        assert_eq!(info.summary(), "↑2");
    }
}

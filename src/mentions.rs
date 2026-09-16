use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub label: String,
    pub mime: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub token: String,
    pub path: PathBuf,
    pub label: String,
}

pub fn active_query(text: &str) -> Option<(usize, String)> {
    let token_start = text
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let token = &text[token_start..];
    let query = token.strip_prefix('@')?;
    if query.contains(char::is_whitespace) {
        return None;
    }
    Some((token_start, query.to_string()))
}

pub fn complete(text: &str, label: &str) -> String {
    match active_query(text) {
        Some((start, _)) => format!("{}@{} ", &text[..start], label),
        None => text.to_string(),
    }
}

pub fn extract(text: &str, dir: &Path) -> Vec<Mention> {
    let mut out: Vec<Mention> = Vec::new();
    for token in text.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let raw = raw.trim_end_matches([',', '.', ';']);
        if raw.is_empty() {
            continue;
        }
        let candidate = {
            let p = Path::new(raw);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                dir.join(p)
            }
        };
        if candidate.is_file() && !out.iter().any(|m| m.path == candidate) {
            let label = candidate
                .strip_prefix(dir)
                .unwrap_or(&candidate)
                .to_string_lossy()
                .to_string();
            out.push(Mention {
                token: format!("@{raw}"),
                path: candidate,
                label,
            });
        }
    }
    out
}

pub fn to_attachments(mentions: &[Mention]) -> Vec<Attachment> {
    mentions
        .iter()
        .map(|m| Attachment {
            label: m.label.clone(),
            mime: mime_for(&m.path).to_string(),
            url: file_url(&m.path),
        })
        .collect()
}

pub fn inline_attachments(text: &str, attachments: &[Attachment], cap: usize) -> String {
    let mut out = String::new();
    for a in attachments {
        match a.url.strip_prefix("file://") {
            Some(path) => {
                let body = std::fs::read_to_string(path).unwrap_or_default();
                let clipped: String = body.chars().take(cap).collect();
                let truncated = body.chars().count() > clipped.chars().count();
                out.push_str(&format!(
                    "<attached-file path=\"{}\">\n{}{}\n</attached-file>\n",
                    a.label,
                    clipped,
                    if truncated { "\n… [truncated]" } else { "" }
                ));
            }
            None => out.push_str(&format!(
                "<attached-file path=\"{}\" mime=\"{}\"/>\n",
                a.label, a.mime
            )),
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
    out
}

pub fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") | Some("toml") => "text/plain",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("yaml") | Some("yml") => "application/yaml",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") | Some("mjs") => "text/javascript",
        Some("ts") | Some("tsx") => "text/typescript",
        Some("py") => "text/x-python",
        Some("sh") => "text/x-shellscript",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        _ => "text/plain",
    }
}

pub fn file_url(path: &Path) -> String {
    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    format!("file://{}", abs.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("theta-mentions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn active_query_detects_a_trailing_at_token() {
        assert_eq!(active_query("@src"), Some((0, "src".into())));
        assert_eq!(active_query("look at @main.rs"), Some((8, "main.rs".into())));
        assert_eq!(active_query("no mention"), None);
        assert_eq!(active_query("@done plus text"), None);
    }

    #[test]
    fn completion_replaces_the_query() {
        let out = complete("check @ma", "src/main.rs");
        assert_eq!(out, "check @src/main.rs ");
    }

    #[test]
    fn extract_resolves_existing_files_only() {
        let dir = tmp("extract");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        let mentions = extract("see @src/main.rs and @nope.rs please", &dir);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].label, "src/main.rs");
        assert!(mentions[0].path.ends_with("src/main.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_and_file_url() {
        assert_eq!(mime_for(Path::new("x.json")), "application/json");
        assert_eq!(mime_for(Path::new("x.rs")), "text/plain");
        let url = file_url(Path::new("/tmp/x.rs"));
        assert!(url.starts_with("file://"));
    }
}

//! Skills and prompt packs.
//!
//! Theta's lightweight take on Pi's extension surface: markdown resources that
//! shape the agent without recompiling. A registry is discovered from:
//! - `<root>/skills/<name>/SKILL.md` or `<root>/skills/<name>.md`
//! - `<root>/prompts/<name>.md`
//!
//! Skills are appended to the system prompt as an index (name + description)
//! so the model knows what is available; prompt packs are addressable by name.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    pub skills: Vec<Skill>,
    pub prompts: Vec<PromptTemplate>,
}

impl Registry {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.prompts.is_empty()
    }

    /// Discover resources under each root (missing roots are ignored).
    pub fn discover(roots: &[PathBuf]) -> Self {
        let mut reg = Registry::default();
        for root in roots {
            reg.scan_skills(&root.join("skills"));
            reg.scan_prompts(&root.join("prompts"));
        }
        reg.skills.sort_by(|a, b| a.name.cmp(&b.name));
        reg.prompts.sort_by(|a, b| a.name.cmp(&b.name));
        reg.skills.dedup_by(|a, b| a.name == b.name);
        reg.prompts.dedup_by(|a, b| a.name == b.name);
        reg
    }

    /// Default discovery locations for the current user.
    pub fn default_roots(cwd: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(cfg) = dirs::config_dir() {
            roots.push(cfg.join("theta"));
        }
        roots.push(cwd.join(".theta"));
        roots.push(cwd.join(".agents"));
        roots
    }

    fn scan_skills(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if let Ok(body) = std::fs::read_to_string(&skill_md) {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.skills.push(make_skill(&name, &body));
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if let Ok(body) = std::fs::read_to_string(&path) {
                    self.skills.push(make_skill(&name, &body));
                }
            }
        }
    }

    fn scan_prompts(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if let Ok(body) = std::fs::read_to_string(&path) {
                    self.prompts.push(PromptTemplate { name, body });
                }
            }
        }
    }

    pub fn find_prompt(&self, name: &str) -> Option<&PromptTemplate> {
        self.prompts.iter().find(|p| p.name == name)
    }

    pub fn find_skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Markdown block appended to the system prompt describing available
    /// skills. Empty when there are none.
    pub fn system_appendix(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let mut out = String::from("Available skills:\n");
        for s in &self.skills {
            if s.description.is_empty() {
                out.push_str(&format!("- {}\n", s.name));
            } else {
                out.push_str(&format!("- {}: {}\n", s.name, s.description));
            }
        }
        out
    }
}

fn make_skill(name: &str, body: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: first_description(body),
        body: body.to_string(),
    }
}

/// Best-effort description: a `description:` frontmatter line, else the first
/// non-heading, non-empty line.
fn first_description(body: &str) -> String {
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("description:") {
            return rest.trim().trim_matches('"').to_string();
        }
    }
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("---") {
            continue;
        }
        return t.chars().take(120).collect();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("theta-ext-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discovers_skills_and_prompts() {
        let root = tempdir("disc");
        std::fs::create_dir_all(root.join("skills/review")).unwrap();
        std::fs::write(
            root.join("skills/review/SKILL.md"),
            "# Review\ndescription: Review code for bugs\nSteps...",
        )
        .unwrap();
        std::fs::write(root.join("skills/hotfix.md"), "# Hotfix\nFast patching").unwrap();
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::write(root.join("prompts/pr.md"), "Open a PR for {{task}}").unwrap();

        let reg = Registry::discover(&[root.clone()]);
        assert_eq!(reg.skills.len(), 2);
        assert_eq!(reg.find_skill("review").unwrap().description, "Review code for bugs");
        assert_eq!(reg.find_skill("hotfix").unwrap().description, "Fast patching");
        assert_eq!(reg.find_prompt("pr").unwrap().body.trim(), "Open a PR for {{task}}");
        let appendix = reg.system_appendix();
        assert!(appendix.contains("review"));
        assert!(appendix.contains("hotfix"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_roots_are_ignored() {
        let reg = Registry::discover(&[PathBuf::from("/definitely/not/here")]);
        assert!(reg.is_empty());
        assert_eq!(reg.system_appendix(), "");
    }
}

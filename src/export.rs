use crate::harness::transcript::{Message, PartKind, Role};

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "you",
        Role::Assistant => "assistant",
    }
}

pub fn markdown(messages: &[Message]) -> String {
    let mut out = String::from("# Theta session\n\n");
    for m in messages {
        out.push_str(&format!("## {}\n\n", role_label(m.role)));
        for p in &m.parts {
            match &p.kind {
                PartKind::Text { text, synthetic } if !synthetic && !text.trim().is_empty() => {
                    out.push_str(text.trim_end());
                    out.push_str("\n\n");
                }
                PartKind::Tool(t) => {
                    let title = t.display_title();
                    out.push_str(&format!("- **{}** `{}`\n", t.tool, title));
                    if let Some(outp) = &t.output {
                        let first: String = outp.lines().next().unwrap_or("").chars().take(160).collect();
                        if !first.trim().is_empty() {
                            out.push_str(&format!("  ```\n  {first}\n  ```\n"));
                        }
                    }
                    if let Some(err) = &t.error {
                        out.push_str(&format!("  ! {}\n", err.lines().next().unwrap_or("")));
                    }
                    out.push('\n');
                }
                PartKind::Compaction { tokens_before } => {
                    out.push_str(&format!("---\n\n*conversation compacted ({tokens_before} tokens)*\n\n"));
                }
                _ => {}
            }
        }
        if let Some(err) = &m.error {
            out.push_str(&format!("> error: {err}\n\n"));
        }
    }
    out
}

pub fn jsonl(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        if let Ok(line) = serde_json::to_string(m) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

pub fn default_filename(session_name: &str, jsonl_out: bool) -> String {
    let mut slug: String = session_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "session".into();
    }
    format!("theta-{slug}.{}", if jsonl_out { "jsonl" } else { "md" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::{Part, ToolInfo, ToolStatus};
    use serde_json::json;

    fn msg(role: Role, parts: Vec<Part>) -> Message {
        Message {
            id: "m".into(),
            role,
            error: None,
            completed: Some(1),
            created: None,
            cost: None,
            tokens: None,
            parts,
        }
    }
    fn text(id: &str, body: &str) -> Part {
        Part {
            id: id.into(),
            message_id: "m".into(),
            kind: PartKind::Text { text: body.into(), synthetic: false },
        }
    }

    #[test]
    fn markdown_includes_roles_text_and_tools() {
        let msgs = vec![
            msg(Role::User, vec![text("p1", "do the thing")]),
            msg(
                Role::Assistant,
                vec![
                    text("p2", "working on it"),
                    Part {
                        id: "p3".into(),
                        message_id: "m".into(),
                        kind: PartKind::Tool(ToolInfo {
                            tool: "bash".into(),
                            call_id: "c".into(),
                            status: ToolStatus::Completed,
                            title: Some("Run tests".into()),
                            input: json!({"command": "cargo test"}),
                            output: Some("ok\nmore".into()),
                            error: None,
                            metadata: json!({}),
                            start_ms: None,
                        }),
                    },
                ],
            ),
        ];
        let md = markdown(&msgs);
        assert!(md.contains("## you"));
        assert!(md.contains("do the thing"));
        assert!(md.contains("## assistant"));
        assert!(md.contains("**bash**"));
        assert!(md.contains("ok"));
    }

    #[test]
    fn jsonl_round_trips() {
        let msgs = vec![msg(Role::User, vec![text("p1", "hello")])];
        let out = jsonl(&msgs);
        assert_eq!(out.lines().count(), 1);
        let back: Message = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(back.role, Role::User);
    }

    #[test]
    fn default_filename_is_slugged() {
        assert_eq!(default_filename("My Session!", false), "theta-My-Session.md");
        assert_eq!(default_filename("", true), "theta-session.jsonl");
    }
}

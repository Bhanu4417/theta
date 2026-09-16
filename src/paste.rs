use crate::mentions::Attachment;

pub const LONG_PASTE_CHARS: usize = 150;

#[derive(Debug, Clone, PartialEq)]
pub enum PasteContent {
    Text(String),
    File { mime: String, filename: String, url: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PastePart {
    pub placeholder: String,
    pub content: PasteContent,
}

pub fn is_long(text: &str) -> bool {
    let lines = text.lines().count();
    lines >= 3 || text.chars().count() > LONG_PASTE_CHARS
}

pub fn text_placeholder(text: &str) -> String {
    let lines = text.lines().count().max(1);
    format!("[Pasted ~{lines} lines]")
}

pub fn file_placeholder(index: usize, mime: &str) -> String {
    let kind = if mime == "application/pdf" {
        "PDF"
    } else {
        "Image"
    };
    format!("[{kind} {index}]")
}

pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64(bytes))
}

pub fn expand(text: &str, parts: &[PastePart]) -> String {
    let mut out = text.to_string();
    for p in parts {
        if let PasteContent::Text(body) = &p.content {
            if out.contains(&p.placeholder) {
                out = out.replacen(&p.placeholder, body, 1);
            }
        }
    }
    out
}

pub fn attachments(text: &str, parts: &[PastePart]) -> Vec<Attachment> {
    parts
        .iter()
        .filter(|p| text.contains(&p.placeholder))
        .filter_map(|p| match &p.content {
            PasteContent::File { mime, filename, url } => Some(Attachment {
                label: filename.clone(),
                mime: mime.clone(),
                url: url.clone(),
            }),
            PasteContent::Text(_) => None,
        })
        .collect()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}


#[derive(Debug, Clone, PartialEq)]
pub enum Clipboard {
    Text(String),
    Image { mime: String, bytes: Vec<u8> },
}

pub fn pick_image_mime(types: &[String]) -> Option<String> {
    types
        .iter()
        .find(|t| t.starts_with("image/"))
        .cloned()
}

pub fn read_clipboard() -> Option<Clipboard> {
    if let Some(c) = read_wl_paste() {
        return Some(c);
    }
    read_xclip()
}

fn run(cmd: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn read_wl_paste() -> Option<Clipboard> {
    let list = run("wl-paste", &["--list-types"])?;
    let types: Vec<String> = String::from_utf8_lossy(&list)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(mime) = pick_image_mime(&types) {
        if let Some(bytes) = run("wl-paste", &["-t", &mime]) {
            if !bytes.is_empty() {
                return Some(Clipboard::Image { mime, bytes });
            }
        }
    }
    if types.iter().any(|t| t.starts_with("text/")) {
        if let Some(bytes) = run("wl-paste", &["--no-newline", "-t", "text/plain"]) {
            return Some(Clipboard::Text(String::from_utf8_lossy(&bytes).to_string()));
        }
    }
    None
}

fn read_xclip() -> Option<Clipboard> {
    let targets = run("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"])?;
    let types: Vec<String> = String::from_utf8_lossy(&targets)
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    if let Some(mime) = pick_image_mime(&types) {
        if let Some(bytes) = run("xclip", &["-selection", "clipboard", "-t", &mime, "-o"]) {
            if !bytes.is_empty() {
                return Some(Clipboard::Image { mime, bytes });
            }
        }
    }
    if let Some(bytes) = run("xclip", &["-selection", "clipboard", "-o"]) {
        return Some(Clipboard::Text(String::from_utf8_lossy(&bytes).to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paste_detection_and_placeholder() {
        assert!(!is_long("short"));
        assert!(is_long("a\nb\nc"));
        assert!(is_long(&"x".repeat(200)));
        assert_eq!(text_placeholder("a\nb\nc"), "[Pasted ~3 lines]");
        assert_eq!(file_placeholder(1, "image/png"), "[Image 1]");
        assert_eq!(file_placeholder(2, "application/pdf"), "[PDF 2]");
    }

    #[test]
    fn expand_replaces_text_placeholders() {
        let parts = vec![PastePart {
            placeholder: "[Pasted ~3 lines]".into(),
            content: PasteContent::Text("real\nbody\nhere".into()),
        }];
        let out = expand("review [Pasted ~3 lines] please", &parts);
        assert_eq!(out, "review real\nbody\nhere please");
    }

    #[test]
    fn attachments_only_include_named_file_parts() {
        let parts = vec![
            PastePart {
                placeholder: "[Image 1]".into(),
                content: PasteContent::File {
                    mime: "image/png".into(),
                    filename: "clipboard.png".into(),
                    url: "data:image/png;base64,AAAA".into(),
                },
            },
            PastePart {
                placeholder: "[Pasted ~2 lines]".into(),
                content: PasteContent::Text("text".into()),
            },
        ];
        let atts = attachments("see [Image 1]", &parts);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].mime, "image/png");
        assert!(attachments("no placeholder", &parts).is_empty());
    }

    #[test]
    fn data_url_is_base64() {
        let url = data_url("image/png", b"ABC");
        assert_eq!(url, "data:image/png;base64,QUJD");
    }

    #[test]
    fn picks_first_image_mime() {
        let types = vec!["text/plain".into(), "image/png".into(), "image/jpeg".into()];
        assert_eq!(pick_image_mime(&types).as_deref(), Some("image/png"));
        assert_eq!(pick_image_mime(&["text/plain".into()]), None);
    }
}

//! Opt-in debug logging (`theta --log`).
//!
//! Writes a timestamped line per event to a file under the user's data dir,
//! so it works from any working directory. Disabled by default and cheap to
//! check, so instrumentation can stay in the hot paths.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

/// Directory holding `--log` files.
pub fn logs_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join("theta").join("logs")
}

/// Where `--log` writes by default: `<data>/theta/logs/theta-<epoch>.log`.
pub fn default_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    logs_dir().join(format!("theta-{secs}.log"))
}

/// The most recently modified `.log` file, for `theta --log`.
pub fn newest_log(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let t = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
            best = Some((t, path));
        }
    }
    best.map(|(_, p)| p)
}

pub fn init(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = LOG.set(Mutex::new(file));
    Ok(())
}

pub fn enabled() -> bool {
    LOG.get().is_some()
}

fn stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60,
        ms
    )
}

pub fn write(line: &str) {
    if let Some(m) = LOG.get() {
        if let Ok(mut f) = m.lock() {
            let _ = writeln!(f, "[{}] {}", stamp(), line);
        }
    }
}

/// The last `max` lines of `path`, reading at most the trailing 4 MiB so a
/// long-running log cannot stall the render/tick path.
pub fn tail(path: &Path, max: usize) -> Vec<String> {
    use std::io::Read;
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    let mut data = String::new();
    if let Ok(mut f) = File::open(path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len > MAX_BYTES {
            use std::io::{Seek, SeekFrom};
            let _ = f.seek(SeekFrom::Start(len - MAX_BYTES));
            let mut buf = Vec::new();
            let _ = f.read_to_end(&mut buf);
            data = String::from_utf8_lossy(&buf).into_owned();
        } else {
            let _ = f.read_to_string(&mut data);
        }
    }
    let mut lines: Vec<String> = data.lines().map(str::to_string).collect();
    if lines.len() > max {
        lines.drain(..lines.len() - max);
    }
    lines
}

/// Log a line when `--log` is active (formats lazily).
#[macro_export]
macro_rules! tlog {
    ($($arg:tt)*) => {
        if $crate::logging::enabled() {
            $crate::logging::write(&format!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_timestamped_lines() {
        let path = std::env::temp_dir().join(format!("theta-log-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        init(&path).expect("init log");
        assert!(enabled());
        write("hello world");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hello world"));
        assert!(text.trim_start().starts_with('['));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tail_reads_the_last_lines() {
        let path = std::env::temp_dir().join(format!("theta-tail-{}.log", std::process::id()));
        std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
        assert_eq!(tail(&path, 2), vec!["c".to_string(), "d".to_string()]);
        assert_eq!(tail(&path, 9).len(), 4);
        let _ = std::fs::remove_file(&path);
    }

}

//! Stamps each build with a short id (git hash + local build time) so the
//! running binary can show which build it is — handy after `/refresh`.

use std::process::Command;

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    // Re-run on source changes so the stamp advances after each update.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");

    let hash = run(Command::new("git").args(["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "nogit".into());
    let time = run(Command::new("date").arg("+%H:%M:%S"))
        .unwrap_or_else(|| "??:??:??".into());

    println!("cargo:rustc-env=THETA_BUILD_HASH={hash}");
    println!("cargo:rustc-env=THETA_BUILD_TIME={time}");
}

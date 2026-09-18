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
    // Deliberately no `rerun-if-changed`.
    //
    // It was `cargo:rerun-if-changed=src`, which watches the *directory*: cargo
    // compares the mtime of that path, and editing a file inside a directory
    // does not change the directory's own mtime. Only adding or removing a file
    // does. So the script did not re-run after an ordinary edit, and the stamp
    // stayed on an older commit — a binary built after commit 2e53d7b reported
    // 7125c87, the commit before it.
    //
    // Declaring no rerun-if-changed makes cargo use its default and re-run when
    // any file in the package changes, which is what a build stamp needs.

    let hash = run(Command::new("git").args(["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "nogit".into());
    let time = run(Command::new("date").arg("+%H:%M:%S"))
        .unwrap_or_else(|| "??:??:??".into());

    println!("cargo:rustc-env=THETA_BUILD_HASH={hash}");
    println!("cargo:rustc-env=THETA_BUILD_TIME={time}");
}

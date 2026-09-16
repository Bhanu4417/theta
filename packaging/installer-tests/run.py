#!/usr/bin/env python3
"""End-to-end tests for the installers.

Serves a fake release from an in-process HTTP server and runs the real installer
against it. The server runs in a thread rather than as a background process,
which is what made the earlier shell-based CI steps flaky: a backgrounded
`python -m http.server` sometimes failed to come up, and "download failed" then
looked like a pass for the wrong reason.

Usage:
    python3 run.py --installer sh|ps1 [--binary path/to/theta]

Exit status is non-zero if any check fails.
"""
import argparse
import functools
import hashlib
import http.server
import os
import pathlib
import platform
import shutil
import socketserver
import subprocess
import sys
import tarfile
import threading
import zipfile

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]


def _version_from_manifest():
    """The version under test, read from Cargo.toml.

    Hardcoding it meant bumping the release broke these tests: the harness
    asserted the binary printed the old number, so a version bump turned into a
    red CI for no real reason.
    """
    import re
    manifest = (REPO_ROOT / "Cargo.toml").read_text()
    m = re.search(r'^version\s*=\s*"([^"]+)"', manifest, re.M)
    if not m:
        raise SystemExit("error: could not read version from Cargo.toml")
    return m.group(1)


VERSION = _version_from_manifest()

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    if not ok and detail:
        for line in str(detail).strip().splitlines()[-8:]:
            print(f"        {line}")


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def serve(directory):
    handler = functools.partial(Handler, directory=str(directory))
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", 0), handler)
    httpd.daemon_threads = True
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{port}"


def host_target():
    """The Rust target triple for this machine, matching the release workflow."""
    m = platform.machine().lower()
    if sys.platform == "darwin":
        return "aarch64-apple-darwin" if m in ("arm64", "aarch64") else "x86_64-apple-darwin"
    if os.name == "nt":
        return "x86_64-pc-windows-msvc"
    return "aarch64-unknown-linux-gnu" if m in ("aarch64", "arm64") else "x86_64-unknown-linux-gnu"


def build_release(root, target, binary, zip_style):
    """Lay out the release the way the workflow would.

    `zip_style` follows the installer being tested rather than the host, so the
    PowerShell installer can be exercised on any platform.
    """
    rel = root / f"v{VERSION}"
    rel.mkdir(parents=True, exist_ok=True)
    stage = root / "_stage"
    stage.mkdir(exist_ok=True)

    if zip_style:
        asset = f"theta-{target}.zip"
        shutil.copy(binary, stage / "theta.exe")
        with zipfile.ZipFile(rel / asset, "w", zipfile.ZIP_DEFLATED) as z:
            z.write(stage / "theta.exe", "theta.exe")
    else:
        asset = f"theta-{target}.tar.gz"
        shutil.copy(binary, stage / "theta")
        os.chmod(stage / "theta", 0o755)
        with tarfile.open(rel / asset, "w:gz") as tf:
            tf.add(stage / "theta", arcname="theta")

    digest = hashlib.sha256((rel / asset).read_bytes()).hexdigest()
    (rel / f"{asset}.sha256").write_text(f"{digest}  {asset}\n")
    return asset, digest


def run_sh(args, base, dest):
    env = dict(os.environ)
    env.update({"THETA_BASE_URL": base, "THETA_INSTALL_DIR": dest})
    p = subprocess.run(["sh", str(REPO_ROOT / "install.sh"), *args],
                       capture_output=True, text=True, env=env, timeout=180)
    return p.returncode, p.stdout + p.stderr


def run_ps1(args, base, dest):
    env = dict(os.environ)
    env.update({"THETA_BASE_URL": base, "THETA_INSTALL_DIR": dest,
                "LOCALAPPDATA": dest})
    # The installer selects its build from this, and it is absent off-Windows.
    env.setdefault("PROCESSOR_ARCHITECTURE", "AMD64")
    pwsh = shutil.which("pwsh") or shutil.which("powershell") or "pwsh"
    p = subprocess.run([pwsh, "-NoProfile", "-File",
                        str(REPO_ROOT / "install.ps1"), *args],
                       capture_output=True, text=True, env=env, timeout=180)
    return p.returncode, p.stdout + p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--installer", choices=["sh", "ps1"], required=True)
    ap.add_argument("--binary", default=None)
    args = ap.parse_args()

    # The asset name must match what the installer under test asks for, not the
    # host: install.ps1 always requests the windows-msvc build.
    target = "x86_64-pc-windows-msvc" if args.installer == "ps1" else host_target()
    want_exe = args.installer == "ps1"
    binary = pathlib.Path(args.binary) if args.binary else \
        REPO_ROOT / "target" / "release" / ("theta.exe" if want_exe else "theta")
    if not binary.exists() and want_exe:
        # Built on Linux, there is no .exe; the archive only needs a file with
        # that name, and the installer checks the hash before using it.
        binary = REPO_ROOT / "target" / "release" / "theta"
    if not binary.exists():
        print(f"error: binary not found at {binary}; build it first", file=sys.stderr)
        return 2

    root = pathlib.Path("/tmp" if os.name != "nt" else os.environ.get("TEMP", ".")) \
        / "theta-installer-tests"
    shutil.rmtree(root, ignore_errors=True)
    asset, digest = build_release(root, target, binary, args.installer == "ps1")

    httpd, base = serve(root)
    print(f"installer: {args.installer}")
    print(f"target:    {target}")
    print(f"serving:   {root} at {base}\n")

    run = run_sh if args.installer == "sh" else run_ps1
    exe_name = "theta.exe" if args.installer == "ps1" else "theta"
    tmp = pathlib.Path(os.environ.get("RUNNER_TEMP", "/tmp"))

    def dest(name):
        d = tmp / f"theta-it-{name}"
        shutil.rmtree(d, ignore_errors=True)
        return d

    try:
        print("=== 1. happy path ===")
        d = dest("happy")
        rc, out = run(["--version", VERSION] if args.installer == "sh"
                      else ["-Version", VERSION], base, str(d))
        exe = d / exe_name
        check("installer succeeds", rc == 0, out)
        check("binary installed", exe.exists(), out)
        check("checksum verified", "ok" in out, out)
        if exe.exists():
            v = subprocess.run([str(exe), "--version"], capture_output=True, text=True)
            check("installed binary runs", VERSION in (v.stdout + v.stderr),
                  v.stdout + v.stderr)

        print("\n=== 2. tampered checksum is refused ===")
        sumfile = root / f"v{VERSION}" / f"{asset}.sha256"
        good = sumfile.read_text()
        sumfile.write_text("deadbeef" * 8 + f"  {asset}\n")
        d = dest("bad")
        rc, out = run(["--version", VERSION] if args.installer == "sh"
                      else ["-Version", VERSION], base, str(d))
        check("refused", rc != 0, out)
        check("reports a mismatch", "checksum mismatch" in out.lower(), out)
        check("nothing installed", not (d / exe_name).exists())
        sumfile.write_text(good)

        print("\n=== 3. missing release ===")
        d = dest("404")
        rc, out = run(["--version", "9.9.9"] if args.installer == "sh"
                      else ["-Version", "9.9.9"], base, str(d))
        check("fails", rc != 0, out)
        check("nothing half-installed", not (d / exe_name).exists())

        print("\n=== 4. no published checksum warns and proceeds ===")
        held = sumfile.with_suffix(".held")
        sumfile.rename(held)
        d = dest("nosum")
        rc, out = run(["--version", VERSION] if args.installer == "sh"
                      else ["-Version", VERSION], base, str(d))
        check("warns", "no checksum published" in out.lower(), out)
        check("installs anyway", (d / exe_name).exists(), out)
        held.rename(sumfile)

        print("\n=== 5. corrupt archive ===")
        junk_root = root / "junk"
        junk = junk_root / f"v{VERSION}"
        junk.mkdir(parents=True, exist_ok=True)
        (junk / asset).write_bytes(b"this is not an archive")
        (junk / f"{asset}.sha256").write_text(
            f"{hashlib.sha256((junk / asset).read_bytes()).hexdigest()}  {asset}\n")
        httpd2, base2 = serve(junk_root)
        d = dest("junk")
        rc, out = run(["--version", VERSION] if args.installer == "sh"
                      else ["-Version", VERSION], base2, str(d))
        check("rejected", rc != 0, out)
        check("nothing installed", not (d / exe_name).exists())
        httpd2.shutdown()

        print("\n=== 6. flags ===")
        d = dest("dry")
        rc, out = run((["--version", VERSION, "--dry-run"] if args.installer == "sh"
                       else ["-Version", VERSION, "-DryRun"]), base, str(d))
        check("dry run succeeds", rc == 0, out)
        check("dry run installs nothing", not (d / exe_name).exists(), out)
        bad_flag = "--nonsense" if args.installer == "sh" else "-Nonsense"
        rc, _ = run([bad_flag], base, str(dest("flag")))
        check("unknown flag rejected", rc != 0)

        print("\n=== 7. re-install replaces in place ===")
        d = dest("reinstall")
        run(["--version", VERSION] if args.installer == "sh" else ["-Version", VERSION],
            base, str(d))
        (d / exe_name).write_text("stale")
        rc, out = run(["--version", VERSION] if args.installer == "sh"
                      else ["-Version", VERSION], base, str(d))
        v = subprocess.run([str(d / exe_name), "--version"], capture_output=True, text=True)
        check("replaced with a working binary", rc == 0 and VERSION in v.stdout,
              out + v.stdout)
        leftovers = [p.name for p in d.iterdir() if p.name.startswith(".theta.new")]
        check("no temp files left behind", not leftovers, leftovers)
    finally:
        httpd.shutdown()

    passed = sum(1 for _, ok in results if ok)
    failed = len(results) - passed
    print(f"\n==================== {passed} passed, {failed} failed ====================")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

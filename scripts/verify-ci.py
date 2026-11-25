#!/usr/bin/env python3
"""Cross-platform static checks and standalone Rust verification for Draconium."""

from __future__ import annotations

import argparse
import html
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]


class Verification:
    def __init__(self, root: Path, temp_dir: Path) -> None:
        self.root = root
        self.temp_dir = temp_dir
        self.failures: list[str] = []
        self.passes = 0

    def command(self, label: str, args: list[str], cwd: Path | None = None) -> bool:
        print(f"[RUN] {label}")
        try:
            result = subprocess.run(
                args,
                cwd=cwd or self.root,
                text=True,
                encoding="utf-8",
                errors="replace",
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
        except OSError as error:
            print(f"[FAIL] {label}: could not start {args[0]!r}: {error}")
            self.failures.append(label)
            return False

        if result.returncode == 0:
            print(f"[PASS] {label}")
            self.passes += 1
            return True

        print(f"[FAIL] {label} (exit {result.returncode})")
        if result.stdout:
            print(result.stdout.rstrip())
        if result.stderr:
            print(result.stderr.rstrip(), file=sys.stderr)
        self.failures.append(label)
        return False

    def check(self, label: str, problems: list[str]) -> bool:
        if problems:
            print(f"[FAIL] {label} ({len(problems)} issue(s))")
            for problem in problems:
                print(f"  {problem}")
            self.failures.append(label)
            return False
        print(f"[PASS] {label}")
        self.passes += 1
        return True


def strip_markdown_code(text: str) -> str:
    """Blank fenced and inline code while preserving line numbers."""
    output: list[str] = []
    fence_char: str | None = None
    fence_size = 0
    for line in text.splitlines(keepends=True):
        fence = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence_char is not None:
            if fence:
                marker = fence.group(1)
                if marker[0] == fence_char and len(marker) >= fence_size:
                    fence_char = None
                    fence_size = 0
            output.append("\n" if line.endswith("\n") else "")
            continue
        if fence:
            marker = fence.group(1)
            fence_char = marker[0]
            fence_size = len(marker)
            output.append("\n" if line.endswith("\n") else "")
            continue
        output.append(line)

    no_fences = "".join(output)
    inline_code = re.compile(r"(`+)[\s\S]*?\1")
    return inline_code.sub(lambda match: "\n" * match.group(0).count("\n"), no_fences)


def parse_inline_destination(line: str, start: int) -> str | None:
    index = start
    while index < len(line) and line[index].isspace():
        index += 1
    if index >= len(line) or line[index] == ")":
        return None

    if line[index] == "<":
        index += 1
        destination: list[str] = []
        while index < len(line):
            char = line[index]
            if char == ">":
                return "".join(destination)
            if char == "\\" and index + 1 < len(line):
                index += 1
                char = line[index]
            destination.append(char)
            index += 1
        return None

    destination = []
    paren_depth = 0
    while index < len(line):
        char = line[index]
        if char == "\\" and index + 1 < len(line):
            index += 1
            destination.append(line[index])
        elif char == "(":
            paren_depth += 1
            destination.append(char)
        elif char == ")":
            if paren_depth == 0:
                break
            paren_depth -= 1
            destination.append(char)
        elif char.isspace() and paren_depth == 0:
            break
        else:
            destination.append(char)
        index += 1
    return "".join(destination) or None


def markdown_destinations(text: str) -> list[tuple[int, str]]:
    clean_text = strip_markdown_code(text)
    destinations: list[tuple[int, str]] = []
    inline_start = re.compile(r"!?\[[^\]\n]*\]\(")
    reference_definition = re.compile(
        r"^ {0,3}\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))", re.IGNORECASE
    )

    for line_number, line in enumerate(clean_text.splitlines(), start=1):
        for match in inline_start.finditer(line):
            destination = parse_inline_destination(line, match.end())
            if destination:
                destinations.append((line_number, destination))
        definition = reference_definition.match(line)
        if definition:
            destinations.append((line_number, definition.group(1) or definition.group(2)))
    return destinations


def check_markdown_links(root: Path) -> list[str]:
    markdown_files = set(root.glob("*.md"))
    markdown_files.update((root / "docs").glob("*.md"))
    markdown_files.update((root / "measurements").glob("*.md"))
    broken: list[str] = []

    for markdown_file in sorted(markdown_files):
        try:
            content = markdown_file.read_text(encoding="utf-8-sig", errors="replace")
        except OSError as error:
            broken.append(f"{markdown_file.relative_to(root)}: cannot read file: {error}")
            continue

        for line_number, raw_destination in markdown_destinations(content):
            destination = html.unescape(raw_destination).strip()
            if not destination or destination.startswith("#"):
                continue
            if destination.startswith(("//", "/")):
                continue

            parsed = urlsplit(destination)
            if parsed.scheme or parsed.netloc:
                continue
            relative_path = unquote(parsed.path)
            if not relative_path:
                continue
            target = (markdown_file.parent / relative_path).resolve()
            if not target.exists():
                try:
                    source_name = markdown_file.relative_to(root)
                except ValueError:
                    source_name = markdown_file
                broken.append(
                    f"{source_name}:{line_number}: {raw_destination}"
                )

    return broken


def phase_static(verification: Verification) -> None:
    print("\n== Phase 1: Static checks ==")
    root = verification.root
    commands = [
        (
            "cargo fmt --check (draconium-host)",
            [
                "cargo",
                "fmt",
                "--check",
                "--manifest-path",
                "crates/draconium-host/Cargo.toml",
            ],
        ),
        (
            "cargo fmt --check (draconium)",
            ["cargo", "fmt", "--check", "--manifest-path", "probes/servo/Cargo.toml"],
        ),
        (
            "cargo fmt --check (draconium-webview2)",
            ["cargo", "fmt", "--check", "--manifest-path", "probes/webview2/Cargo.toml"],
        ),
        (
            "rustfmt --check (win32 live probe)",
            ["rustfmt", "--edition", "2024", "--check", "scripts/win32_live_probe.rs"],
        ),
    ]
    for label, command in commands:
        verification.command(label, command, cwd=root)

    for probe in ("servo", "webview2"):
        verification.command(
            f"cargo metadata --locked --offline ({probe})",
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--offline",
                "--no-deps",
                "--manifest-path",
                f"probes/{probe}/Cargo.toml",
            ],
            cwd=root,
        )

    for directory in ("probes/servo/ui", "ui"):
        js_files = sorted((root / directory).glob("*.js"))
        if not js_files:
            verification.check(f"Node syntax checks ({directory})", ["no *.js files found"])
        for js_file in js_files:
            relative_path = js_file.relative_to(root).as_posix()
            verification.command(
                f"node --check {relative_path}",
                ["node", "--check", relative_path],
                cwd=root,
            )

    verification.check("Relative Markdown links", check_markdown_links(root))


def executable_path(directory: Path, name: str) -> Path:
    return directory / (name + (".exe" if os.name == "nt" else ""))


def phase_standalone_tests(verification: Verification) -> None:
    print("\n== Phase 2: Standalone Rust unit tests ==")
    sources = [
        "crates/draconium-host/src/unix_platform.rs",
        "crates/draconium-host/src/window_state.rs",
        "crates/draconium-host/src/bridge.rs",
        "crates/draconium-host/src/protocol.rs",
        "crates/draconium-host/src/policy.rs",
        "probes/servo/src/workspace.rs",
        "probes/servo/src/starter.rs",
        "probes/servo/src/shot.rs",
        "probes/servo/vendor/servo-paint/wheel_scroll.rs",
    ]
    for source in sources:
        source_path = verification.root / source
        name = source_path.stem.replace("-", "_")
        binary = executable_path(verification.temp_dir, f"unit_{name}")
        label = f"rustc unit tests ({source})"
        compiled = verification.command(
            f"compile {label}",
            [
                "rustc",
                "--edition",
                "2024",
                "--test",
                "-D",
                "warnings",
                str(source_path),
                "-o",
                str(binary),
            ],
        )
        if compiled:
            verification.command(f"run {label}", [str(binary)])


def rust_path_literal(path: Path) -> str:
    return path.resolve().as_posix()


def platform_harness_source(root: Path) -> str:
    sources = {
        "embedder_ui": rust_path_literal(root / "crates/draconium-host/src/embedder_ui.rs"),
        "tray": rust_path_literal(root / "crates/draconium-host/src/tray.rs"),
        "unix_platform": rust_path_literal(root / "crates/draconium-host/src/unix_platform.rs"),
        "win_platform": rust_path_literal(root / "crates/draconium-host/src/win_platform.rs"),
    }
    return f'''

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[path = "{sources["embedder_ui"]}"]
mod embedder_ui;

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[path = "{sources["tray"]}"]
mod tray;

#[path = "{sources["unix_platform"]}"]
mod unix_platform;

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[path = "{sources["win_platform"]}"]
mod win_platform;

pub use unix_platform::{{PlatformCapabilities, platform_capabilities}};
pub use tray::TrayHandle;
pub use win_platform::SingleInstanceLock;

pub mod platform {{
    pub use crate::unix_platform::{{PlatformCapabilities, platform_capabilities}};
    pub use crate::win_platform::{{
        opacity_to_alpha, parse_hotkey, validate_custom_uri_scheme, validate_devtools_addr,
    }};
}}

// `host.rs` is deliberately absent from this lightweight crate root. Expose a
// thin `pub fn` touching the exact host-facing wrappers in `embedder_ui`,
// `tray`, and `win_platform` so `--crate-type lib -D warnings` exercises the
// full `embedder_ui`/`tray`/`win_platform` -> `unix_platform` call graph on
// macOS, Linux, and other Unix without hiding real dead code inside
// `unix_platform.rs`.
#[cfg(not(test))]
pub fn __ci_host_surface() {{
    let _start: fn(tray::TrayConfig, fn(tray::TrayAction)) -> Result<tray::TrayHandle, String> =
        tray::TrayHandle::start;
    let _ = (
        _start,
        embedder_ui::show_alert as fn(isize, &str, &str, bool),
        embedder_ui::show_confirm as fn(isize, &str, &str, bool) -> bool,
        embedder_ui::show_open_file_dialog
            as fn(isize, &[String], bool, bool) -> Option<Vec<std::path::PathBuf>>,
        embedder_ui::show_save_file_dialog
            as fn(isize, &str, &[String], bool) -> Option<std::path::PathBuf>,
        embedder_ui::show_select_folder_dialog
            as fn(isize, &str, bool) -> Option<std::path::PathBuf>,
        embedder_ui::show_color_picker
            as fn(isize, (u8, u8, u8), bool) -> Option<(u8, u8, u8)>,
        embedder_ui::open_external_url as fn(&str) -> Result<(), String>,
        tray::TrayHandle::has_tray_icon as fn(&tray::TrayHandle) -> bool,
        tray::TrayHandle::show_notification as fn(&tray::TrayHandle, &str, &str) -> bool,
        win_platform::SingleInstanceLock::acquire
            as fn(&str) -> Result<Option<win_platform::SingleInstanceLock>, String>,
        win_platform::forward_to_running_instance as fn(&str, &str) -> bool,
        win_platform::detect_system_dark_mode as fn() -> Option<bool>,
        win_platform::clipboard_read_text as fn(isize) -> Result<String, String>,
        win_platform::clipboard_write_text as fn(isize, &str) -> Result<(), String>,
        win_platform::clipboard_read_html as fn(isize) -> Result<String, String>,
        win_platform::clipboard_write_html as fn(isize, &str) -> Result<(), String>,
        win_platform::clipboard_read_image_json as fn(isize) -> Result<String, String>,
        win_platform::clipboard_write_image_dib as fn(isize, &[u8]) -> Result<(), String>,
        win_platform::clipboard_write_image_rgba as fn(isize, u32, u32, &[u8]) -> Result<(), String>,
        win_platform::clipboard_clear as fn(isize) -> Result<(), String>,
        win_platform::register_uri_scheme_for_exe
            as fn(&str, &str, &std::path::Path, &[&str]) -> Result<(), String>,
        win_platform::unregister_uri_scheme as fn(&str) -> Result<(), String>,
        win_platform::autostart_is_enabled as fn(&str) -> Result<bool, String>,
        win_platform::autostart_set_enabled_for_exe
            as fn(&str, bool, &std::path::Path, &[&str]) -> Result<(), String>,
    );
}}

#[cfg(test)]
mod ci_integration_tests {{
    use super::*;

    #[test]
    fn platform_capabilities_match_the_running_os() {{
        let capabilities = unix_platform::platform_capabilities();
        #[cfg(target_os = "windows")]
        assert_eq!(capabilities.os, "windows");
        #[cfg(target_os = "macos")]
        assert_eq!(capabilities.os, "macos");
        #[cfg(target_os = "linux")]
        assert_eq!(capabilities.os, "linux");
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        assert!(capabilities.clipboard_image);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        assert!(capabilities.custom_uri_scheme);
        assert_eq!(platform_capabilities(), capabilities);
    }}

    #[cfg(unix)]
    #[test]
    fn unix_lock_forwarding_cleanup_and_stale_socket_recovery() {{
        use std::fs;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;
        use std::time::{{Duration, SystemTime, UNIX_EPOCH}};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        let tag = format!("dn-ci-{{}}-{{nonce}}", std::process::id());
        let lock_path = unix_platform::unix_instance_lock_path(&tag).expect("lock path");
        let socket_path = unix_platform::unix_instance_socket_path(&tag).expect("IPC path");
        assert_eq!(lock_path.extension().unwrap(), "lock");
        assert_eq!(lock_path.parent(), socket_path.parent());
        let dir_meta = fs::symlink_metadata(socket_path.parent().expect("IPC directory")).unwrap();
        assert!(dir_meta.is_dir());
        assert_eq!(dir_meta.mode() & 0o777, 0o700);

        let acquire = || win_platform::SingleInstanceLock::acquire(&tag);
        let lock = acquire().expect("acquire first lock").expect("first lock is available");
        assert!(acquire().expect("check duplicate lock").is_none());
        let lock_meta = fs::symlink_metadata(&lock_path).expect("lock file metadata");
        assert!(lock_meta.is_file());
        assert_eq!(lock_meta.mode() & 0o777, 0o600);
        assert_eq!(lock_meta.uid(), dir_meta.uid());

        drop(lock);
        let lock = acquire().expect("reacquire lock").expect("lock released on drop");
        assert_eq!(fs::metadata(&lock_path).unwrap().ino(), lock_meta.ino());
        assert!(acquire().expect("check reacquired lock").is_none());

        let start_server = || {{
            let (action_tx, action_rx) = mpsc::channel();
            let tray = tray::TrayHandle::start(
                tray::TrayConfig {{
                    tooltip: "Draconium CI".to_owned(),
                    presentations: false,
                    show_tray_icon: false,
                    hotkey: None,
                    instance_window_tag: tag.clone(),
                }},
                move |action| {{
                    let _ = action_tx.send(action);
                }},
            )?;
            Ok::<_, String>((tray, action_rx))
        }};
        let check_forwarding = |action_rx: &mpsc::Receiver<tray::TrayAction>| {{
            assert_eq!(fs::metadata(&socket_path).expect("IPC socket metadata").mode() & 0o777, 0o600);
            let payload = format!(r#"{{{{"source":"verify-ci","pid":{{}}}}}}"#, std::process::id());
            assert!(win_platform::forward_to_running_instance(&tag, &payload));
            assert_eq!(
                action_rx.recv_timeout(Duration::from_secs(3)).expect("receive forwarded payload"),
                tray::TrayAction::SecondInstance(payload)
            );
        }};
        let (tray, action_rx) = start_server().expect("start Unix IPC");
        assert!(
            unix_platform::UnixInstanceServer::start(&tag, |_| {{}}).is_err(),
            "a second server cannot replace a live socket"
        );
        check_forwarding(&action_rx);
        drop(tray);
        assert!(!socket_path.exists(), "IPC socket is removed on drop");

        drop(UnixListener::bind(&socket_path).expect("bind stale IPC fixture"));
        assert!(socket_path.exists());
        let (recovered, action_rx) = start_server().expect("recover stale IPC socket");
        check_forwarding(&action_rx);
        drop(recovered);
        assert!(!socket_path.exists());

        drop(lock);
        fs::remove_file(&lock_path).expect("remove CI-specific lock fixture");
        fs::remove_file(socket_path.with_extension("ipc.lock")).expect("remove CI-specific IPC lock fixture");
    }}
}}
'''


def phase_platform_harness(verification: Verification, run_win32_probe: bool) -> None:
    print("\n== Phase 3: Cross-platform OS integration harness ==")
    harness = verification.temp_dir / "platform_harness.rs"
    harness.write_text(platform_harness_source(verification.root), encoding="utf-8")

    library = verification.temp_dir / ("platform_harness.lib" if os.name == "nt" else "libplatform_harness.rlib")
    library_ok = verification.command(
        "compile platform harness library (-D warnings)",
        [
            "rustc",
            "--edition",
            "2024",
            "--crate-type",
            "lib",
            "-D",
            "warnings",
            str(harness),
            "-o",
            str(library),
        ],
    )

    test_binary = executable_path(verification.temp_dir, "platform_harness_tests")
    test_compiled = verification.command(
        "compile platform harness tests (-D warnings)",
        [
            "rustc",
            "--edition",
            "2024",
            "--test",
            "-D",
            "warnings",
            str(harness),
            "-o",
            str(test_binary),
        ],
    )
    if test_compiled:
        verification.command("run platform harness tests", [str(test_binary)])

    if run_win32_probe:
        probe_binary = executable_path(verification.temp_dir, "win32_live_probe")
        probe_source = verification.root / "scripts" / "win32_live_probe.rs"
        probe_compiled = verification.command(
            "compile Win32 live probe (-D warnings)",
            [
                "rustc",
                "--edition",
                "2024",
                "-D",
                "warnings",
                str(probe_source),
                "-o",
                str(probe_binary),
            ],
        )
        if probe_compiled:
            probe_output = verification.temp_dir / "win32-live-probe-output"
            verification.command(
                "run Win32 live probe",
                [str(probe_binary), str(probe_output)],
            )

    if not library_ok:
        print("[INFO] Platform test execution continues even though the library check failed.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--win32-probe",
        action="store_true",
        help="compile and run the Windows-only Win32 live integration probe",
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="draconium-verify-ci-") as temp_name:
        verification = Verification(ROOT, Path(temp_name))
        phase_static(verification)
        phase_standalone_tests(verification)
        phase_platform_harness(verification, args.win32_probe)

    print("\n== Verification summary ==")
    print(f"[PASS] {verification.passes} check(s)")
    if verification.failures:
        print(f"[FAIL] {len(verification.failures)} check(s):")
        for failure in verification.failures:
            print(f"  - {failure}")
        return 1
    print("[PASS] All requested CI verification checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

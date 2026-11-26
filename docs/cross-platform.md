# Cross-Platform Host Support (`draconium-host`)

`crates/draconium-host` separates platform-independent host logic (Servo 0.8.0 embedding, `winit` windowing, `dn://app/` protocol, `Capabilities` ACL, preload bridge, PDF/PNG/HTML export, crash reporting) from OS-specific desktop integrations:

- **Windows**: direct Win32 / DWM / Shell / DPAPI FFI ([`platform_services.rs`](../crates/draconium-host/src/platform_services.rs), [`embedder_ui.rs`](../crates/draconium-host/src/embedder_ui.rs), [`tray.rs`](../crates/draconium-host/src/tray.rs)).
- **macOS & Linux**: zero-extra-dependency Rust `std` backends ([`unix_platform.rs`](../crates/draconium-host/src/unix_platform.rs)) using native OS utilities, standard user-session files (`LaunchAgents` / XDG Autostart), and local Unix domain sockets.

---

## 1. Capability Support Matrix

| Capability | JS / Rust Field | Windows | macOS | Linux |
| --- | --- | --- | --- | --- |
| Servo 0.8.0 WebView, `dn://app/`, `Capabilities`, `invoke`/`invokeAsync`, child windows, PNG/HTML/PDF export, DevTools RDP | *(core)* | Supported | Supported | Supported |
| Native media backend (optional Cargo feature) | `mediaGstreamer` / `media_gstreamer` | `false` by default; `true` with `media-gstreamer` | `false` by default; `true` with `media-gstreamer` | `false` by default; `true` with `media-gstreamer` |
| Native dialogs (Alert, Confirm, Open/Save File, Folder Picker, Color Picker) | `nativeDialogs` / `native_dialogs` | Win32 (`MessageBoxW`, `GetOpenFileNameW`, `GetSaveFileNameW`, `IFileOpenDialog`, `ChooseColorW`) | `osascript` (AppleScript) | `zenity` (fallback: `kdialog`) |
| Anchored popup menus for HTML selects and context menus | `nativePopupMenus` / `native_popup_menus` | `true`, Win32 popup menu | `false`, selection dialog | `false`, selection dialog |
| Clipboard (Text & Clear) | `clipboardText` / `clipboard_text` | Win32 (`CF_UNICODETEXT`) | `pbpaste` / `pbcopy` (`LC_CTYPE=UTF-8`) | `wl-paste` / `wl-copy` (fallback: `xclip`, `xsel`) |
| Clipboard (HTML) | `clipboardHtml` / `clipboard_html` | Win32 (`CF_HTML`) | `osascript` (`«class HTML»`) | `wl-paste` / `wl-copy` (fallback: `xclip`) |
| Clipboard (Image) | `clipboardImage` / `clipboard_image` | Win32 (`CF_DIB`) | PNG via `osascript` (`PNGf`) | PNG via `wl-paste` / `wl-copy` (fallback: `xclip`) |
| Desktop notifications | `notifications` | Win32 `Shell_NotifyIconW` balloon | `osascript` (`display notification`) | `notify-send` |
| Login autostart | `autostart` | `HKCU\...\Run` and Task Manager startup status | `~/Library/LaunchAgents/io.draconium.<id>.plist` and launchd override | `$XDG_CONFIG_HOME/autostart/<id>.desktop` (`~/.config/autostart/`) and desktop-entry flags |
| Single-instance lock & forwarding (`--single-instance`) | `singleInstance` / `single_instance` | Named mutex + `WM_COPYDATA`, with startup retry | Advisory file lock + Unix IPC socket | Advisory file lock + Unix IPC socket |
| System Light/Dark theme detection | `systemTheme` / `system_theme` | `AppsUseLightTheme` + `WM_SETTINGCHANGE` | Background sampling with `defaults` | Background sampling with `gsettings` and `GTK_THEME` fallback |
| External URL (`https:` / `mailto:`) | `externalUrl` / `external_url` | `ShellExecuteW` | `open -- <url>` | `xdg-open <url>` |
| Native system tray (`--tray`) | `systemTray` / `system_tray` | Win32 `Shell_NotifyIconW` | Not yet implemented | Not yet implemented |
| Global hotkey (`--hotkey`) | `globalHotkey` / `global_hotkey` | Win32 `RegisterHotKey` | Not yet implemented | Not yet implemented |
| OS credential encryption (`safeStorage`) | `safeStorage` / `safe_storage` | Win32 DPAPI (`CryptProtectData`) | Not yet implemented | Not yet implemented |
| Layered window opacity (`--opacity`) | `windowOpacity` / `window_opacity` | Win32 `SetLayeredWindowAttributes` | Not yet implemented | Not yet implemented |
| Mouse click-through | `clickThrough` / `click_through` | Win32 `WS_EX_TRANSPARENT` | Not yet implemented | Not yet implemented |
| Custom URI scheme registration | `customUriScheme` / `custom_uri_scheme` | `HKCU\Software\Classes\<scheme>` | Not yet implemented | XDG `.desktop` handler (`xdg-mime`, best effort) |

---

## 2. Runtime Feature Detection

Hosted web UIs and Rust callers can query the exact compiled OS and supported host capabilities at runtime rather than sniffing user-agent strings.

### JavaScript (`window.Draconium.window.getPlatform()`)

Available on all pages (including `Capabilities::restricted()` and attenuated child windows):

```js
const res = window.Draconium.window.getPlatform();
if (res.ok) {
  const platform = res.data;
  // Example on Windows: { os: "windows", nativeDialogs: true, safeStorage: true, mediaGstreamer: false, ... }
  // mediaGstreamer is true only when the optional Cargo feature was compiled in.
  if (!platform.safeStorage) {
    // Gracefully hide or fall back when OS-native secret storage is unavailable
  }
}
```

The GStreamer runtime and platform plugin requirements are described in [Optional Native Media (`media-gstreamer`)](media-gstreamer.md).

### Rust (`draconium_host::platform::platform_capabilities()`)

```rust
use draconium_host::platform::platform_capabilities;

let caps = platform_capabilities();
if caps.single_instance {
    println!("Running on {} with single-instance support", caps.os);
}
```

---

## 3. External OS Tooling Notes

No additional Rust crates are required for the OS utility backends on macOS or Linux. The optional native media backend has separate GStreamer crate and system-library requirements; see [Optional Native Media (`media-gstreamer`)](media-gstreamer.md).

- **macOS**: Uses built-in system binaries (`osascript`, `open`, `pbcopy`, `pbpaste`, `defaults`, `plutil`, `launchctl`) and user-writable `~/Library/LaunchAgents/`.
- **Linux**: Uses standard freedesktop / desktop utilities when present:
  - Dialogs: `zenity` (automatic fallback to `kdialog` when `zenity` is absent)
  - Clipboard: `wl-copy` / `wl-paste` (Wayland), falling back to `xclip` or `xsel` (X11)
  - Notifications: `notify-send` (`libnotify-bin`)
  - External URLs: `xdg-open` (`xdg-utils`)
  - URI scheme registration: XDG applications `.desktop` entries, with best-effort `xdg-mime` and `update-desktop-database` refreshes
  - System theme: `gsettings` (falling back to `GTK_THEME`)
- **Single-instance Unix locks & IPC sockets**:
  - Uses `0o600` advisory file locks (`File::try_lock()`) alongside a local domain socket (`0o600`) inside a private `0o700` user directory (`$XDG_RUNTIME_DIR` when absolute and within path limits, otherwise `/tmp/dn-<uid>`): `dn-<id>.lock` for single-instance exclusion, an `.ipc.lock` lockfile guarding socket binding/recovery, and `dn-<id>.sock` for second-instance JSON payload forwarding (up to 64 KiB, with brief startup connect retries).
  - Long identifiers are deterministically hash-shortened so generated socket paths remain at most 96 bytes (fitting POSIX/macOS `sun_path` bounds), and stale sockets from crashed processes are cleaned up only when `AddrInUse` probes return `ConnectionRefused` while holding `.ipc.lock`.

---

## 4. Verification Status

| Platform | Verification Level |
| --- | --- |
| **Windows x64** | Verified on real Windows hardware via `scripts/verify-windows.ps1 -Build` (release build with 0 warnings, standalone unit tests, live Win32 probe, 5 E2E self-tests including `getPlatform()`, screenshot non-blank check, DevTools RDP, tray/single-instance test, portable packaging smoke test). |
| **ubuntu-latest / macos-latest / windows-latest CI** | On every push and pull request, the CI matrix runs `scripts/verify-ci.py` on all three runners (Windows also runs the Win32 probe). Pure-Rust platform helpers and `PlatformCapabilities` serialization are compiled and unit-tested with `-D warnings`. Live macOS/Linux GUI rendering and execution still await desktop runs. |

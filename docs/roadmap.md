# Draconium Roadmap

## Vision & Positioning

Draconium aims to be a **lightweight, self-contained Rust desktop WebView host** powered by Servo — enabling standard HTML, CSS, and JavaScript desktop applications without bundling Chromium or relying on OS WebView runtimes.

- **Scope**: Focused desktop host (similar in architecture to Tauri, backed by embedded Servo instead of system WebViews) rather than Node.js / full Chromium runtime compatibility.
- **Benchmark Baseline**: Windows WebView2 (`probes/webview2`, via `wry`) is maintained strictly as a reproducible comparison target, never as a runtime fallback.
- **Engineering Principles (KISS / YAGNI)**: Keep host modules pure Rust where possible, minimize upstream Servo patches, and validate every capability with automated tests.

---

## Completed Foundations

### Engine & Input
- Migrated to **Servo 0.8.0** (`Stylo 0.23.0`, `WebRender 0.70.1`, `skrifa 0.49.0`, `taffy 0.15.1`), removing upstreamed patches (`servo-paint-api` `#48636`, `servo-script` `#48690`).
- Maintained a single minimal patch in [`probes/servo/vendor/servo-paint`](../probes/servo/vendor/servo-paint/README.draconium.md) for 120 ms smooth line-delta wheel interpolation (`2^-10 px` quantization) and `< 1.0 px` subpixel scroll-remainder normalization.
- Aligned Windows IME commit handling (`CompositionState::Update` -> `CompositionState::End`) with Servo 0.8.0 `servoshell`.

### Reusable Library Crate (`draconium-host`) & Security Boundaries
- Extracted the host runtime into a standalone library crate ([`crates/draconium-host`](../crates/draconium-host/Cargo.toml)) exporting `HostConfig`, `WindowConfig`, `HostHandler`, `Capabilities`, `EmbeddedAsset`, `SavedWindowState`, `resolve_app_url_from`, and `run`, plus a minimal standalone example ([`examples/hello.rs`](../crates/draconium-host/examples/hello.rs)) and API reference ([docs/host-api.md](host-api.md)).
- Decoupled app-specific UI assets (`EmbeddedAsset` passed by callers via `.with_embedded_assets(...)`) and capability builders (`Capabilities::restricted()` / `Capabilities::standard_desktop()`), leaving `probes/servo` as a thin binary consumer of `draconium-host`.
- Implemented `dn://app/` custom protocol routing with directory-traversal protection, compiled-in `include_bytes!` fallback, and strict `Content-Security-Policy` headers (`--csp-wasm` optional).
- Implemented declarative per-app [`Capabilities`](../crates/draconium-host/src/policy.rs) ACLs, child-window privilege attenuation (`attenuate_for_child()`), per-launch 128-bit session-token preload hardening (`Object.freeze`), and loopback `--dev-url` support.

### Native Desktop Integration & Cross-Platform Layer
- **Windows (Verified)**: Frameless custom chrome (`drag`, 8-edge `dragResize`, minimize, maximize, rounded transparent surface), Win11 DWM rounded corners / dark titlebar, layered opacity (`--opacity`), click-through, system tray (`--tray`), global hotkeys (`--hotkey`), single-instance `WM_COPYDATA` forwarding (`--single-instance`), custom URI schemes (`--deep-link`), `HKCU` autostart, OS power/theme events, all 6 `show_embedder_control` handlers, `IFileOpenDialog` folder picker, capability-scoped drag-and-drop file bridge (`dn:file-drop`), rich clipboard (Text / `CF_HTML` / `CF_DIB`), DPAPI `safeStorage`, raster PDF / HTML / PNG export, structured `.crash.json` reports, and loopback Firefox DevTools RDP (`--devtools`).
- **macOS & Linux Backends + Capability Introspection**: Implemented zero-extra-dependency Rust `std` backends in [`crates/draconium-host/src/unix_platform.rs`](../crates/draconium-host/src/unix_platform.rs) for native dialogs (`osascript` / `zenity` + `kdialog`), text & HTML clipboard (`pbcopy`/`pbpaste` / `wl-copy`/`xclip`/`xsel`), notifications (`osascript` / `notify-send`), autostart (`LaunchAgents` `.plist` / XDG `.desktop`), Unix advisory file locks and domain socket payload forwarding, system Light/Dark theme detection, and external URL opening (`open` / `xdg-open`), paired with runtime `PlatformCapabilities` (`Draconium.window.getPlatform()`). See [docs/cross-platform.md](cross-platform.md).

### Track A · Native desktop integration

- Added `clipboardImage` on macOS and Linux through their native clipboard tools. Raw RGBA writes use PNG directly on both platforms and preserve alpha.
- Added Linux XDG `customUriScheme` registration, with distinct desktop IDs for schemes such as `foo+bar` and `foo-bar` and cleanup of the registered handler in `mimeapps.list`.

### Track B · Window controls and Folio

- Added cross-platform window controls for sizing, bounds, focus, attention, always-on-top, titles, and fullscreen. F11 toggles fullscreen in eligible main and child windows, and custom chrome follows the observed OS state.
- Added Servo history navigation and availability queries to the host bridge. Folio keeps its native window title in sync with the active note and its save state.
- Hardened Folio's editor and saved-note transitions. A failed save keeps the current edits in the editor, and an empty workspace disables its note fields.

### Review hardening

Security and reliability hardening covers Unix clipboard process handling and read limits, file dialog path parsing, private IPC directories and crash-safe locks, XDG desktop entry escaping, window geometry validation, and staged Windows packaging. Media builds now check for GStreamer 1.20 or later.

### Public Showcase Applications & Benchmarks
- **Draconium Folio (`--workspace`)**: Local-first Markdown workspace showcasing custom frameless chrome, live split rendering, atomic `.md` persistence, drag-and-drop import, native export, OS theme sync, and an on-demand 60-frame Render Lab.
- **Draconium Starter (`--starter`)**: Minimal developer template for wiring new Rust + HTML/CSS/JS apps.
- **Servo 0.8.0 vs WebView2 Benchmarks**: Recorded reproducible resource, startup, and binary size measurements in [measurements/README.md](../measurements/README.md).

### Optional Media & CI Wiring
- Added the opt-in Servo 0.8.0 `media-gstreamer` Cargo feature, runtime capability reporting, Windows portable packaging support, and a workflow-dispatch switch for feature-enabled builds. The default remains `DummyBackend` with the GStreamer feature off; see [docs/media-gstreamer.md](media-gstreamer.md).
- Added cross-platform CI verification on `ubuntu-latest`, `macos-latest`, and `windows-latest` for every push and pull request.

---

## Upcoming Exploration Areas

| Topic | Direction |
| --- | --- |
| **Optional Native Media Runtime Validation** | Build with `media-gstreamer` on a host with the required GStreamer development SDK, then validate native audio/video playback and plugin loading with GStreamer 1.20+ on each target OS. |
| **Live macOS/Linux GUI Validation** | Exercise the Unix backends and Track B window controls on macOS and Linux desktops. Record platform and compositor differences before committing to more parity work. |
| **Upstream Servo Boundaries** | Track upstream Servo progress on isolated preload JS realms (`contextBridge` parity), native HTTP download manager APIs, DOM `DataTransfer` file drop integration, and vector PDF printing. |

---

## Explicit Non-Goals

- Node.js runtime compatibility or native npm C++ addons inside the renderer
- Automatic fallback to Chromium / WebView2
- Chrome extension (`chrome.*` / MV3) or Chrome DevTools Protocol (CDP) compatibility
- Widevine / EME DRM media playback

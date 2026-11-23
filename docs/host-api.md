# Draconium Host API Specification (`draconium-host`)

Draconium's host layer lives in the standalone library crate [`crates/draconium-host`](../crates/draconium-host/Cargo.toml) ([`src/lib.rs`](../crates/draconium-host/src/lib.rs) / [`src/host.rs`](../crates/draconium-host/src/host.rs)), enabling external Rust projects to embed **Servo 0.8.0** as a desktop WebView host via `Cargo.toml`.

See [`crates/draconium-host/examples/hello.rs`](../crates/draconium-host/examples/hello.rs) for a complete minimal standalone example.

---

## 1. Adding `draconium-host` to a Project

```toml
[dependencies]
draconium-host = { path = "../Draconium/crates/draconium-host" }

[patch.crates-io]
servo-paint = { path = "../Draconium/probes/servo/vendor/servo-paint" }
```

Native GStreamer media is opt-in: add `features = ["media-gstreamer"]` to the `draconium-host` dependency to select Servo 0.8.0's GStreamer backend. Build and runtime prerequisites are documented in [Optional Native Media (`media-gstreamer`)](media-gstreamer.md).

---

## 2. Rust Host API (`draconium_host`)

### 2.1 Entry Points and Core Types

| Symbol | Responsibility |
| --- | --- |
| `HostConfig` | Launch configuration (window title, URL, `WindowConfig`, `Capabilities`, `dn://` asset root, caller-supplied `EmbeddedAsset` slice, tray, single-instance tag, crash directory, DevTools, deep link). Construct via `HostConfig::new(title, url)` and customize with fluent `with_*` builders. |
| `WindowConfig` | Native window attributes (decorations, alpha transparency, always-on-top, taskbar visibility, logical size/position and size limits, system tray, layered opacity, global hotkey). |
| `HostHandler` | Trait implemented by each hosted application to handle IPC commands and optional window/background hooks. |
| `Capabilities` | Declarative per-app security ACL (`Capabilities::restricted()`, `Capabilities::standard_desktop()`, fluent `with_*` builders, `attenuate_for_child()`). |
| `EmbeddedAsset` | `{ path: &'static str, bytes: &'static [u8] }` entry supplied via `HostConfig::with_embedded_assets(...)` for single-binary `dn://app/` fallback. |
| `resolve_ui_path_from(base_dir, exe_rel, manifest_rel)` | Resolves UI files relative to the executable first (portable distributions), then current working directory, then `base_dir` (typically `Path::new(env!("CARGO_MANIFEST_DIR"))`). |
| `resolve_app_url_from(...)` / `try_resolve_app_url_from(...)` | Resolves `--dev-url` (validating a loopback `http://` `DevOrigin`), `dn://app/<file>` (with compiled-in fallback), or local `file://` startup URL and optional `asset_root`. Use `try_resolve_app_url_from` when handling untrusted URL input without panicking. |
| `run(config, handler, self_test)` | Validates configuration (including loopback enforcement on `devtools_addr`) and starts the winit event loop and embedded Servo 0.8.0 runtime. |
| `platform::*` | Runtime capability introspection (`PlatformCapabilities`, `platform_capabilities`) and validation/parsing helpers (`parse_hotkey`, `opacity_to_alpha`, `validate_custom_uri_scheme`, `validate_devtools_addr`). See [docs/cross-platform.md](cross-platform.md). |
| Window geometry types | `LogicalSize`, `LogicalPosition`, `Presentation`, `SavedWindowState`, `DEFAULT_WINDOW_SIZE`, `CHIPS_SIZE`, `FLYOUT_SIZE`. |

### 2.2 `WindowConfig` Presets and Modifiers

| Preset | Configuration | Typical Use |
| --- | --- | --- |
| `WindowConfig::default()` | Decorated, `1000×720`, opaque | Standard utility or comparison probe (`--probe`) |
| `WindowConfig::standard(w, h)` | Decorated, explicit logical size | Standard desktop tools (`--starter` `760×560`) |
| `WindowConfig::custom_chrome(w, h)` | Frameless (`decorations: false`) + alpha transparent (`transparent: true`) | Custom-drawn titlebar & rounded window shell (`--workspace` `1240×800`) |
| `WindowConfig::frameless_hud(size, pos)` | Frameless, always-on-top, taskbar-free, compact/expanded presentation switching | Resident HUD / overlay widgets |

Modifiers:
- `.with_tray(bool)`: enables native Win32 system tray icon and close-to-tray behavior
- `.with_transparent(bool)`: enables transparent surface and zero-alpha clear color
- `.with_min_size(Option<HostLogicalSize>)` / `.with_max_size(Option<HostLogicalSize>)`: constrain the logical inner window size
- `.with_opacity(Option<f64>)`: sets Win32 layered window opacity (`0.1..=1.0`)
- `.with_global_hotkey(Option<(u32, u32)>)`: registers a system-wide toggle hotkey

### 2.3 `HostConfig` Builders

`HostConfig::new(title, url)` initializes with conservative, locked-down defaults (`Capabilities::restricted()`, `WindowConfig::default()`, external navigation blocked, permissions denied by default).

```rust
use std::path::Path;
use draconium_host::{
    Capabilities, EmbeddedAsset, HostConfig, WindowConfig, resolve_app_url_from, run,
};

const EMBEDDED_ASSETS: &[EmbeddedAsset] = &[
    EmbeddedAsset { path: "app.html", bytes: include_bytes!("../ui/app.html") },
    EmbeddedAsset { path: "app.css", bytes: include_bytes!("../ui/app.css") },
    EmbeddedAsset { path: "app.js", bytes: include_bytes!("../ui/app.js") },
];

let (url, asset_root) = resolve_app_url_from(
    Path::new(env!("CARGO_MANIFEST_DIR")),
    dn_assets,
    dev_url.as_deref(),
    "app.html",
    "ui/app.html",
    "ui/app.html",
);

let caps = Capabilities::standard_desktop()
    .with_autostart(true)
    .with_site_data(true)
    .with_app_commands(["list_notes", "load_note", "save_note"]);

let config = HostConfig::new("My Desktop App", url)
    .with_window(WindowConfig::custom_chrome(1240, 800))
    .with_asset_root(asset_root)
    .with_embedded_assets(EMBEDDED_ASSETS)
    .with_capabilities(caps)
    .with_crash_dir(data_dir.join("crashes"))
    .with_single_instance("Local\\MyApp.Instance", true)
    .with_autostart("MyApp", vec!["--single-instance"]);

run(config, MyHandler::new(data_dir), None)?;
```

Additional builders:
- `.with_devtools(Option<String>)`: loopback-only Firefox DevTools RDP (`127.0.0.1:<port>`, `localhost:<port>`, or `[::1]:<port>` — validated in `run()` for library callers too)
- `.with_screenshot(Option<PathBuf>)`: automated first-paint PNG capture and non-blank analysis
- `.with_deep_link(Option<String>)` / `.with_raw_args(Vec<String>)`: custom URI scheme startup and single-instance forwarding
- `.with_dev_origin(Option<DevOrigin>)`: loopback dev server origin allowed by `--dev-url`
- `.with_wasm_csp(bool)`: appends `'wasm-unsafe-eval'` to `script-src` in `dn://app/` CSP headers
- `.with_reload_shortcuts(bool)`: enables or disables host interception of `F5`, `Ctrl+R`, `Ctrl+F5`, and `Ctrl+Shift+R` reload shortcuts (default `true`; set `false` to forward them to page JS)

### 2.4 `HostHandler` Trait

```rust
pub trait HostHandler {
    /// Handles `Draconium.invoke(command, payload)` and `invokeAsync(command, payload)`.
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String>;

    /// Called once a window move burst settles (default: ignore).
    fn on_window_moved(&mut self, _position: LogicalPosition) {}

    /// Called after a HUD presentation switch; may return a saved position to restore.
    fn on_presentation_changed(
        &mut self,
        _presentation: Presentation,
    ) -> Option<LogicalPosition> {
        None
    }

    /// Opt in to periodic 1 Hz background polling (default: false -> 0% idle CPU).
    fn wants_background_polling(&self) -> bool {
        false
    }

    /// Optional background event pushed to JS listeners via `Draconium.on(event, fn)`.
    fn poll_background_event(&mut self) -> Option<(&'static str, String)> {
        None
    }
}
```

---

## 3. Security Boundaries & `Capabilities` ACL

All privileged operations are gated by [`crates/draconium-host/src/policy.rs`](../crates/draconium-host/src/policy.rs), OS CSPRNG-backed 128-bit per-launch session-token authentication, and initial-origin binding:

- **Origin-bound capabilities & preload**: `window.Draconium` preload injection (`bridge::render_preload_script_for_origin`) and navigation checks bind each WebView (primary and child) strictly to its own launch origin (`dn://app/`, the configured loopback `--dev-url`, or canonicalized filesystem paths under the initial `file://` directory). Non-origin URLs such as `about:blank` receive neither a preload bridge nor bridge IPC access.
- **`window.prompt()` bridge semantics**: Draconium intercepts all `window.prompt()` calls in hosted WebViews (standard browser text prompts are not used). The per-launch session token (`dn-tok:<nonce>:`) authenticates `dn-window:*`, `dn-window-async:*`, and `dn-app-async:*` calls from the frozen preload, while synchronous `Draconium.invoke` app commands are further constrained to `Capabilities.allowed_app_commands`. Note that Servo 0.8.0's `PromptDialog` does not carry per-frame sender URLs and `WebView::url()` updates asynchronously from history, so cross-origin/subframe isolation relies on strict `request_navigation` origin confinement plus top-frame origin-guarded preload injection.
- **File access tokens**: `Draconium.files.readText*` / `readBase64*` are permitted when **either** `Capabilities.allow_file_drop` **or** `Capabilities.allow_file_dialogs` is enabled, and only read OS-issued `dnfile-<rand64>-<id>` tokens (per-token 64-bit OS random component; drop/dialog batches are capped at 64 entries per batch so all returned tokens remain live).
- **Child window attenuation & close behavior**: Secondary windows created via `window.open` or `Draconium.window.openChild` (up to 4 windows, with `openChild` restricted to `dn://app/`, `about:blank`, the configured dev origin, or canonically contained same-directory `file://` URLs; each child gets its own `UserContentManager` and target-window-isolated async IPC routing) automatically receive `Capabilities::attenuate_for_child()`, stripping file access, clipboard, DPAPI, autostart, session control, and application commands. Child windows close immediately on OS close or `dn-window:close` without invoking `__draconiumBeforeClose` (reserved for the primary window).
- **Navigation & permissions**: `request_navigation` confines each WebView (including subframes) strictly to its own initial origin (blocking cross-origin switches such as `dn://app/` -> `file://` or `about:blank`, UNC paths, and symlinks/junctions escaping a `file://` app directory). `request_permission` denies all requests by default.
- **`dn://app/` serving & CSP**: [`crates/draconium-host/src/protocol.rs`](../crates/draconium-host/src/protocol.rs) and [`crates/draconium-host/src/dn_protocol.rs`](../crates/draconium-host/src/dn_protocol.rs) reject directory traversal/dotfiles, confine disk lookups via `canonicalize()`, fall back to caller-supplied `EmbeddedAsset` bytes, attach `X-Content-Type-Options: nosniff`, and attach strict `Content-Security-Policy` headers on HTML, SVG, and XML documents (`default-src 'self' dn://app; script-src 'self' dn://app; style-src 'self' dn://app; ...; object-src 'none'`). Note that `file://` mode does not inject HTTP CSP headers — prefer `dn://app/` for packaged apps.
- **Single-instance forwarding trust boundary**: Unix single-instance IPC uses `0700` user directories, `0600` sockets/locks, and UID checks (`unix_platform.rs`). Windows `WM_COPYDATA` second-instance/deep-link forwarding does not authenticate sender identity beyond the desktop session, so apps should treat forwarded arguments and deep links as untrusted local input.

---

## 4. JavaScript Preload API (`window.Draconium`)

Injected automatically before page scripts via `UserContentManager` and recursively frozen with `Object.freeze` ([`crates/draconium-host/src/bridge.rs`](../crates/draconium-host/src/bridge.rs)).

### 4.1 Application IPC & Events

- `Draconium.invoke(command, payloadString)` -> `{ ok: boolean, data?: string, error?: string }`
- `Draconium.invokeAsync(command, payloadString)` -> `Promise<{ ok: boolean, data?: string, error?: string }>`
- `Draconium.on(eventName, listener)` / `Draconium.off(eventName, listener)`
  - Built-in host events: `state-updated`, `file-drop-hover`, `file-drop-cancel`, `file-drop`, `theme-changed`, `fullscreen-changed` (`"true"` / `"false"`), `power-event`, `global-hotkey`, `capture-saved`, `second-instance`, `open-url`, `notification-clicked`

> [!IMPORTANT]
> Pass `payload` as a string (for example `JSON.stringify(obj)`), check `res.ok`, and parse `res.data` with `JSON.parse(res.data)` when expecting structured JSON.

### 4.2 Native Desktop Namespaces

| Namespace | Methods |
| --- | --- |
| `Draconium.window` | `drag()`, `dragResize(edge)`, `minimize()`, `toggleMaximize()`, `isMaximized()`, `setTitle(title)`, `setFullscreen(enabled)`, `isFullscreen()`, `setAlwaysOnTop(enabled)`, `focus()`, `requestUserAttention(level)`, `getBounds()`, `setSize(width, height)`, `goBack()`, `goForward()`, `canGoBack()`, `canGoForward()`, `showChips()`, `showFlyout()`, `hide()`, `show()`, `close()`, `reload()`, `hardReload()`, `zoomIn()`, `zoomOut()`, `zoomReset()`, `openExternal(url)`, `setOpacity(v)`, `setClickThrough(b)`, `capturePng(name)`, `capturePngAsync(name)`, `exportHtml(name)`, `exportHtmlAsync(name)`, `exportPdf(name)`, `exportPdfAsync(name)`, `openChild(opts)`, `getTheme()`, `getPlatform()` |
| `Draconium.dialog` | `openFile(opts)`, `openFileAsync(opts)`, `selectFolder(opts)`, `selectFolderAsync(opts)`, `saveTextFile(opts)`, `saveTextFileAsync(opts)`, `saveBase64File(opts)`, `saveBase64FileAsync(opts)` |
| `Draconium.files` | `readText(token)`, `readTextAsync(token)`, `readBase64(token)`, `readBase64Async(token)` (capability-scoped tokens only) |
| `Draconium.clipboard` | `readText()`, `writeText(s)`, `readHtml()`, `writeHtml(h)`, `readImage()`, `writeImage(opts)`, `clear()` (plus `*Async` variants) |
| `Draconium.safeStorage` | `encrypt(s)`, `decrypt(b64)` (plus `*Async` variants backed by Win32 DPAPI) |
| `Draconium.notify` | `notify({ title, body })` |
| `Draconium.autostart` | `isEnabled()`, `setEnabled(bool)` |
| `Draconium.protocolClient` | `register(scheme)`, `unregister(scheme)` |
| `Draconium.session` | `listSiteData()`, `clearSiteData()`, `clearCookies(opts)`, `clearCache()` (plus `*Async` variants) |

**Window geometry and state**

- `getBounds()` returns a JSON object. Its `x` and `y` values are the outer window position in logical screen coordinates, or both `null` when the OS or Wayland compositor hides the position. The `width` and `height` values are the logical inner window size. The object also includes `maximized`, `fullscreen`, and `scaleFactor`.
- `setSize(width, height)` accepts positive integers and clamps them to 200–7680 wide by 120–4320 high, further constrained by the configured minimum and maximum inner sizes. The reply is a "widthxheight" string in logical pixels. It reports the applied size when winit applies it synchronously, or the clamped request while the OS processes it.
- `setTitle(title)` sets an explicit per-window title that survives page title changes. An empty or whitespace-only title clears the override and restores that window's configured title. When there is no override, page title notifications can update the native title.
- `focus()` and `setAlwaysOnTop(enabled)` are best effort on Wayland. The compositor can ignore activation or stacking requests.
- `isFullscreen()` reports the observed fullscreen state as "true" or "false". Subscribe to `fullscreen-changed` for observed changes and query `isFullscreen()` on page load to initialize custom chrome.
- `Draconium.session.listSiteData()` returns the cached snapshot synchronously while queuing a background refresh; use `Draconium.session.listSiteDataAsync()` when you need an up-to-date listing directly from Servo's `SiteDataManager`.

### 4.3 Custom Frameless Window Chrome

Apps configured with `WindowConfig::custom_chrome(width, height)` (such as [`workspace.html`](../probes/servo/ui/workspace.html) / [`workspace.js`](../probes/servo/ui/workspace.js)) wire native window behavior cleanly from HTML/CSS/JS:

1. Mark the titlebar container with `data-dn-drag` (and interactive controls with `data-dn-no-drag`).
2. Attach `mousedown` handlers to 8 border handles calling `Draconium.window.dragResize('n'|'s'|'e'|'w'|'ne'|'nw'|'se'|'sw')`.
3. Wire window controls to `minimize()`, `toggleMaximize()`, `isMaximized()`, and `close()`.
4. Initialize custom chrome with `isFullscreen()` and update it on `fullscreen-changed`. `F11` toggles borderless fullscreen in interactive main and child windows when `WindowSizing` is enabled. Style `.window-shell.is-fullscreen` with square corners and no outer border, as for maximized windows.
5. Style `html, body { background: transparent; }` with a rounded inner `.window-shell` container that switches to square corners when maximized.
6. For inline `<svg>` icons inside Servo 0.8.0 (`resvg`), prefer explicit color attributes over `currentColor` when switching between light and dark themes.

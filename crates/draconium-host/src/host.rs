/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Reusable Servo 0.8.0 desktop WebView host (public API stabilized in Milestone D).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use euclid::Scale;
use servo::user_contents::UserStyleSheet;
use servo::{
    AllowOrDenyRequest, CompositionEvent, CompositionState, ConsoleLogLevel, ContextMenuAction,
    ContextMenuItem, CreateNewWebViewRequest, Cursor, DeviceIntPoint, DeviceIntRect, DeviceIntSize,
    DevicePoint, EmbedderControl, EmbedderControlId, EventLoopWaker, ImeEvent, InputEvent, JSValue,
    LoadStatus, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent,
    NavigationRequest, Notification, PermissionRequest, Preferences, RenderingContext, RgbColor,
    ScreenGeometry, SelectElementOptionOrOptgroup, Servo, ServoBuilder, ServoDelegate, ServoError,
    SimpleDialog, SiteData, StorageType, Theme, UserContentManager, UserScript, WebView,
    WebViewBuilder, WebViewDelegate, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey as WinitNamedKey, PhysicalKey};
#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;
#[cfg(target_os = "windows")]
use winit::raw_window_handle::RawWindowHandle;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{
    Fullscreen, ResizeDirection, UserAttentionType, Window, WindowId, WindowLevel,
};

use crate::bridge::{self, BridgeCommand, DevOrigin, WindowCommand};
use crate::embedder_ui::{self, PopupMenuEntry};
use crate::input;
use crate::tray::{TrayAction, TrayConfig, TrayHandle};
use crate::win_platform::{
    self, FileAccessRegistry, MAX_CAPABILITY_FILE_BYTES, SingleInstanceLock,
};
use crate::window_state;

const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// Ignore `WindowEvent::Moved` shortly after startup or programmatic moves so
/// OS placement events do not clobber the user's saved position.
const PROGRAMMATIC_MOVE_GRACE: Duration = Duration::from_millis(300);
/// Hard cap on simultaneously open child windows per host process.
const MAX_CHILD_WINDOWS: usize = 4;

/// Public re-exports forming the stable host API surface for hosted apps.
pub use crate::policy::Capabilities;
pub use crate::window_state::{
    LogicalPosition as HostLogicalPosition, LogicalSize as HostLogicalSize, Presentation,
};

/// Configuration required by the Draconium WebView host to launch a local UI.
///
/// Construct with [`HostConfig::new`] (which sets minimal, locked-down defaults)
/// and customize via the fluent `with_*` builders before passing to [`run`].
pub struct HostConfig {
    pub title: &'static str,
    pub url: Url,
    pub window: WindowConfig,
    /// Directory holding the bundled UI assets. When set, the host serves
    /// `dn://app/...` from this directory (falling back to embedded assets).
    pub asset_root: Option<PathBuf>,
    /// Caller-supplied compiled-in fallback assets (`include_bytes!`) served
    /// under `dn://app/` when on-disk assets are absent.
    pub embedded_assets: &'static [crate::protocol::EmbeddedAsset],
    /// Optional PNG output path for automated screenshot capture (Milestone C).
    pub screenshot_path: Option<PathBuf>,
    /// Optional loopback-validated `host:port` for Servo's Firefox DevTools RDP server.
    pub devtools_addr: Option<String>,
    /// Optional Win32 mutex name to enforce a single running instance (`--single-instance`).
    pub single_instance_name: Option<String>,
    /// Deterministic instance tag used to name the primary OS event window for `WM_COPYDATA`.
    pub instance_tag: String,
    /// Optional initial deep-link URI passed on the command line (`--deep-link <uri>`).
    pub initial_deep_link: Option<String>,
    /// Raw CLI arguments forwarded to the primary instance when `--single-instance` exits early.
    pub raw_args: Vec<String>,
    /// Registry value name under `HKCU\...\Run` for `Draconium.autostart`.
    pub autostart_app_name: &'static str,
    /// Default CLI flags appended to the executable path when registering autostart / URI schemes.
    pub autostart_args: Vec<&'static str>,
    /// Directory where structured `.crash.json` reports are written on WebView crash or Rust panic.
    pub crash_dir: PathBuf,
    /// Declarative per-app security capability policy (Stage 1).
    pub capabilities: Capabilities,
    /// Optional loopback dev-server origin allowed by `--dev-url`.
    pub dev_origin: Option<DevOrigin>,
    /// Whether `'wasm-unsafe-eval'` is appended to `script-src` in the `dn://` CSP header.
    pub allow_wasm_csp: bool,
    /// Enables Servo's CSS backdrop filters.
    pub backdrop_filter: bool,
    /// Enables built-in reload shortcuts (`F5`, `Ctrl+R`, `Ctrl+F5`, `Ctrl+Shift+R`).
    pub allow_reload_shortcuts: bool,
}

impl HostConfig {
    /// Creates a host configuration with conservative defaults: a standard
    /// 1000x720 window, minimal capabilities, and no privileged OS hooks.
    pub fn new(title: &'static str, url: Url) -> Self {
        Self {
            title,
            url,
            window: WindowConfig::default(),
            asset_root: None,
            embedded_assets: &[],
            screenshot_path: None,
            devtools_addr: None,
            single_instance_name: None,
            instance_tag: "Local\\Draconium".to_owned(),
            initial_deep_link: None,
            raw_args: Vec::new(),
            autostart_app_name: "Draconium",
            autostart_args: Vec::new(),
            crash_dir: std::env::temp_dir().join("draconium-crashes"),
            capabilities: Capabilities::restricted(),
            dev_origin: None,
            allow_wasm_csp: false,
            backdrop_filter: true,
            allow_reload_shortcuts: true,
        }
    }

    pub fn with_window(mut self, window: WindowConfig) -> Self {
        self.window = window;
        self
    }

    pub fn with_asset_root(mut self, asset_root: Option<PathBuf>) -> Self {
        self.asset_root = asset_root;
        self
    }

    pub fn with_embedded_assets(
        mut self,
        embedded_assets: &'static [crate::protocol::EmbeddedAsset],
    ) -> Self {
        self.embedded_assets = embedded_assets;
        self
    }

    pub fn with_screenshot(mut self, screenshot_path: Option<PathBuf>) -> Self {
        self.screenshot_path = screenshot_path;
        self
    }

    /// Configures the local Firefox DevTools RDP server listen address
    /// (`127.0.0.1:<port>`, `localhost:<port>`, or `[::1]:<port>`).
    /// Non-loopback addresses are rejected by [`run`].
    pub fn with_devtools(mut self, devtools_addr: Option<String>) -> Self {
        self.devtools_addr = devtools_addr;
        self
    }

    pub fn with_single_instance(
        mut self,
        instance_tag: impl Into<String>,
        enforce_lock: bool,
    ) -> Self {
        let tag = instance_tag.into();
        self.single_instance_name = if enforce_lock {
            Some(tag.clone())
        } else {
            None
        };
        self.instance_tag = tag;
        self
    }

    pub fn with_deep_link(mut self, initial_deep_link: Option<String>) -> Self {
        self.initial_deep_link = initial_deep_link;
        self
    }

    pub fn with_raw_args(mut self, raw_args: Vec<String>) -> Self {
        self.raw_args = raw_args;
        self
    }

    pub fn with_autostart(mut self, app_name: &'static str, args: Vec<&'static str>) -> Self {
        self.autostart_app_name = app_name;
        self.autostart_args = args;
        self
    }

    pub fn with_crash_dir(mut self, crash_dir: PathBuf) -> Self {
        self.crash_dir = crash_dir;
        self
    }

    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_dev_origin(mut self, dev_origin: Option<DevOrigin>) -> Self {
        self.dev_origin = dev_origin;
        self
    }

    pub fn with_wasm_csp(mut self, allow_wasm_csp: bool) -> Self {
        self.allow_wasm_csp = allow_wasm_csp;
        self
    }

    pub fn with_backdrop_filter(mut self, enabled: bool) -> Self {
        self.backdrop_filter = enabled;
        self
    }

    pub fn with_reload_shortcuts(mut self, enabled: bool) -> Self {
        self.allow_reload_shortcuts = enabled;
        self
    }
}

/// Native window attributes for a hosted Draconium application.
///
/// The defaults create a decorated 1000x720 window. Use
/// [`WindowConfig::standard`], [`WindowConfig::custom_chrome`], or
/// [`WindowConfig::frameless_hud`] for common desktop window archetypes.
#[derive(Clone, Copy)]
pub struct WindowConfig {
    pub decorations: bool,
    pub always_on_top: bool,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub skip_taskbar: bool,
    pub initial_size: window_state::LogicalSize,
    pub initial_position: Option<window_state::LogicalPosition>,
    pub min_size: Option<window_state::LogicalSize>,
    pub max_size: Option<window_state::LogicalSize>,
    pub start_hidden: bool,
    /// Whether the app uses compact/expanded HUD presentations, enabling the
    /// `dn-window:chips` / `dn-window:flyout` bridge commands.
    pub presentations: bool,
    /// Enable the native system tray icon (Windows) for resident restore/quit.
    pub enable_tray: bool,
    /// Hide the window instead of quitting on `CloseRequested` when the system
    /// tray is active.
    pub close_to_tray: bool,
    /// Request a transparent window surface (`with_transparent(true)` +
    /// `shell_background_color_rgba = [0, 0, 0, 0]`).
    pub transparent: bool,
    /// Optional initial layered window opacity (`0.1..=1.0`, Windows).
    pub opacity: Option<f64>,
    /// Optional global hotkey `(win32_modifiers, win32_vk)` to toggle visibility.
    pub global_hotkey: Option<(u32, u32)>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            decorations: true,
            always_on_top: false,
            skip_taskbar: false,
            initial_size: window_state::DEFAULT_WINDOW_SIZE,
            initial_position: None,
            min_size: None,
            max_size: None,
            start_hidden: false,
            presentations: false,
            enable_tray: false,
            close_to_tray: false,
            transparent: false,
            opacity: None,
            global_hotkey: None,
        }
    }
}

impl WindowConfig {
    /// Decorated desktop window with an explicit logical size.
    pub fn standard(width: u32, height: u32) -> Self {
        Self {
            initial_size: HostLogicalSize::new(width, height),
            ..Self::default()
        }
    }

    /// Frameless transparent window for apps that draw their own titlebar,
    /// corner radius, and resize borders (e.g. `--workspace`).
    pub fn custom_chrome(width: u32, height: u32) -> Self {
        Self {
            decorations: false,
            transparent: true,
            initial_size: HostLogicalSize::new(width, height),
            ..Self::default()
        }
    }

    /// Frameless, always-on-top, taskbar-free resident HUD with compact/expanded
    /// presentation switching.
    pub fn frameless_hud(
        initial_size: HostLogicalSize,
        initial_position: Option<HostLogicalPosition>,
    ) -> Self {
        Self {
            decorations: false,
            always_on_top: true,
            skip_taskbar: true,
            initial_size,
            initial_position,
            presentations: true,
            ..Self::default()
        }
    }

    pub fn with_tray(mut self, enabled: bool) -> Self {
        self.enable_tray = enabled;
        self.close_to_tray = enabled;
        self
    }

    pub fn with_transparent(mut self, enabled: bool) -> Self {
        self.transparent = enabled;
        self
    }

    pub fn with_min_size(mut self, min_size: Option<window_state::LogicalSize>) -> Self {
        self.min_size = min_size;
        self
    }

    pub fn with_max_size(mut self, max_size: Option<window_state::LogicalSize>) -> Self {
        self.max_size = max_size;
        self
    }

    pub fn with_opacity(mut self, opacity: Option<f64>) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn with_global_hotkey(mut self, global_hotkey: Option<(u32, u32)>) -> Self {
        self.global_hotkey = global_hotkey;
        self
    }
}

/// Application-specific command handler invoked from JavaScript via the host bridge.
pub trait HostHandler {
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String>;

    /// Called by the host with the window's latest logical position after a
    /// move burst settles, and once more with the final position before a
    /// presentation switch, reload, or close. Default: ignore.
    fn on_window_moved(&mut self, _position: window_state::LogicalPosition) {}

    /// Called by the host after it switches the window presentation.
    /// Returns the saved position to restore for the new presentation, if
    /// any. Default: no restore.
    fn on_presentation_changed(
        &mut self,
        _presentation: Presentation,
    ) -> Option<window_state::LogicalPosition> {
        None
    }

    /// Whether this handler wants periodic [`Self::poll_background_event`]
    /// ticks from the host event loop. Apps that return `false` keep the event
    /// loop completely idle when nothing is animating.
    fn wants_background_polling(&self) -> bool {
        false
    }

    /// Optional background poll hook returning `(event_name, json_payload)` to
    /// push into the page via `window.Draconium.on(event_name, ...)`.
    fn poll_background_event(&mut self) -> Option<(&'static str, String)> {
        None
    }
}

/// Resolves a UI asset relative to the executable first (for packaged builds),
/// then the current working directory, falling back to `base_dir` (typically
/// `Path::new(env!("CARGO_MANIFEST_DIR"))` in the caller crate).
pub fn resolve_ui_path_from(
    base_dir: &Path,
    exe_relative: &str,
    manifest_relative: &str,
) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let packaged = dir.join(exe_relative);
        if packaged.is_file() {
            return packaged;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for candidate in [
            cwd.join(manifest_relative),
            cwd.join("probes").join("servo").join(manifest_relative),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    base_dir.join(manifest_relative)
}

/// Resolves a UI asset relative to the executable or current working directory.
pub fn resolve_ui_path(exe_relative: &str, manifest_relative: &str) -> PathBuf {
    resolve_ui_path_from(Path::new("."), exe_relative, manifest_relative)
}

/// Fallible counterpart of [`resolve_app_url_from`] that validates `dev_url`
/// (requiring a loopback `http://` origin) instead of panicking on invalid input.
pub fn try_resolve_app_url_from(
    base_dir: &Path,
    dn_assets: bool,
    dev_url: Option<&str>,
    file_name: &'static str,
    exe_relative: &str,
    manifest_relative: &str,
) -> Result<(Url, Option<PathBuf>), String> {
    let resolved = resolve_ui_path_from(base_dir, exe_relative, manifest_relative);
    let canonical = resolved.canonicalize().ok();
    let root = canonical
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(raw_dev) = dev_url {
        let origin = DevOrigin::parse(raw_dev)?;
        let url = Url::parse(&origin.initial_url)
            .map_err(|err| format!("--dev-url is not a valid URL: {err}"))?;
        return Ok((url, root));
    }
    if dn_assets || canonical.is_none() {
        let url = Url::parse(&crate::protocol::asset_url(file_name))
            .map_err(|err| format!("invalid dn:// asset URL: {err}"))?;
        Ok((url, root))
    } else {
        let url = Url::from_file_path(canonical.unwrap())
            .map_err(|()| "Invalid UI file path".to_owned())?;
        Ok((url, root))
    }
}

/// Resolves an app's startup URL and optional `dn://app/` asset root directory
/// using the caller crate's `base_dir` (typically `Path::new(env!("CARGO_MANIFEST_DIR"))`):
/// - `dev_url` (`http://127.0.0.1:<port>/...`) when provided,
/// - `dn://app/<file_name>` when `dn_assets` is true or disk assets are absent
///   (single-binary embedded fallback),
/// - `file://` by default during local disk checkout runs.
pub fn resolve_app_url_from(
    base_dir: &Path,
    dn_assets: bool,
    dev_url: Option<&str>,
    file_name: &'static str,
    exe_relative: &str,
    manifest_relative: &str,
) -> (Url, Option<PathBuf>) {
    try_resolve_app_url_from(
        base_dir,
        dn_assets,
        dev_url,
        file_name,
        exe_relative,
        manifest_relative,
    )
    .expect("validated --dev-url or local UI asset resolves")
}

/// Resolves an app's startup URL and optional `dn://app/` asset root directory
/// relative to the executable or current working directory.
pub fn resolve_app_url(
    dn_assets: bool,
    dev_url: Option<&str>,
    file_name: &'static str,
    exe_relative: &str,
    manifest_relative: &str,
) -> (Url, Option<PathBuf>) {
    resolve_app_url_from(
        Path::new("."),
        dn_assets,
        dev_url,
        file_name,
        exe_relative,
        manifest_relative,
    )
}

/// Starts the single-window Servo host with the given UI configuration and request handler.
pub fn run(
    mut config: HostConfig,
    handler: impl HostHandler + 'static,
    self_test: Option<String>,
) -> Result<(), Box<dyn Error>> {
    if let Some(raw_addr) = config.devtools_addr.take() {
        config.devtools_addr = Some(win_platform::validate_devtools_addr(&raw_addr)?);
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let crash_dir_for_panic = config.crash_dir.clone();
    let app_title_for_panic = config.title;
    let url_for_panic = config.url.to_string();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !servo::should_panic_hook_suppress_termination() {
            let reason = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_owned()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_owned()
            };
            let location = info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()));
            let report = win_platform::CrashReport::new(
                app_title_for_panic,
                &url_for_panic,
                "rust-panic",
                &reason,
                location,
            );
            let _ = win_platform::write_crash_report(&crash_dir_for_panic, &report);
        }
        prev_hook(info);
    }));

    let event_loop = EventLoop::<HostEvent>::with_user_event().build()?;
    let screenshot_job = config.screenshot_path.clone().map(ScreenshotJob::new);
    let self_test = self_test.map(|name| SelfTestRunner::new(&name));
    let mut app = App {
        waker: Waker(event_loop.create_proxy()),
        config: Some(config),
        handler: Some(Box::new(handler)),
        self_test,
        screenshot_job,
        state: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App {
    waker: Waker,
    config: Option<HostConfig>,
    handler: Option<Box<dyn HostHandler>>,
    self_test: Option<SelfTestRunner>,
    screenshot_job: Option<ScreenshotJob>,
    state: Option<State>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureFormat {
    Png,
    Pdf,
}

struct PendingCapture {
    format: CaptureFormat,
    suggested: String,
    call_id: Option<u64>,
}

enum PendingSessionOp {
    ListSiteData {
        call_id: Option<u64>,
    },
    ClearSiteData {
        call_id: Option<u64>,
    },
    ClearCookies {
        session_only: bool,
        call_id: Option<u64>,
    },
    ClearCache {
        call_id: Option<u64>,
    },
}

enum PendingChildWindow {
    FromBridge {
        origin_window: WindowId,
        url: Url,
        title: String,
        width: u32,
        height: u32,
        call_id: Option<u64>,
    },
    FromServoRequest(CreateNewWebViewRequest),
}

enum PendingAsyncTask {
    App {
        call_id: u64,
        command: String,
        payload: String,
    },
    Window {
        call_id: u64,
        command: WindowCommand,
        payload: String,
        parse_json: bool,
    },
}

struct ChildWindowEntry {
    window_id: WindowId,
    // Drop `webview` before `_user_content_manager`, `rendering_context`, and `servo`.
    webview: WebView,
    _user_content_manager: Rc<UserContentManager>,
    rendering_context: Rc<WindowRenderingContext>,
    delegate: Rc<Delegate>,
    mouse_position: DevicePoint,
    modifiers: ModifiersState,
    composing: bool,
}

struct State {
    // Drop all WebViews (primary and child) before `_user_content_manager`, rendering contexts, and Servo.
    webview: WebView,
    child_windows: Vec<ChildWindowEntry>,
    _user_content_manager: Rc<UserContentManager>,
    servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    delegate: Rc<Delegate>,
    // Keep the single-instance lock alive for the lifetime of the window.
    _single_instance: Option<SingleInstanceLock>,
    // Keep the tray/OS-event window alive for the lifetime of the window.
    _tray: Option<Rc<TrayHandle>>,
    background_polling: bool,
    next_bg_poll: Instant,
    mouse_position: DevicePoint,
    modifiers: ModifiersState,
    composing: bool,
    hovering_files: bool,
    pending_dropped_files: Vec<PathBuf>,
}

// The delegate does not own the WebView, avoiding any reference cycle.
struct Delegate {
    default_title: String,
    explicit_title: RefCell<Option<String>>,
    last_observed_fullscreen: Cell<bool>,
    consumed_f11: Cell<Option<PhysicalKey>>,
    min_size: window_state::LogicalSize,
    max_size: window_state::LogicalSize,
    window: Rc<Window>,
    waker: Waker,
    handler: Rc<RefCell<Box<dyn HostHandler>>>,
    session_token: String,
    capabilities: Capabilities,
    dev_origin: Option<DevOrigin>,
    initial_url: Url,
    initial_file_dir: Option<PathBuf>,
    autostart_app_name: &'static str,
    autostart_args: Vec<&'static str>,
    crash_dir: PathBuf,
    files: RefCell<FileAccessRegistry>,
    tray: Option<Rc<TrayHandle>>,
    presentations: bool,
    tray_active: bool,
    close_to_tray: bool,
    non_interactive: bool,
    allow_reload_shortcuts: bool,
    dark_theme: Cell<bool>,
    window_visible: Cell<bool>,
    input_method: Cell<Option<EmbedderControlId>>,
    animating: Cell<bool>,
    load_complete: Cell<bool>,
    has_painted: Cell<bool>,
    close_in_progress: Cell<bool>,
    close_started_at: Cell<Option<Instant>>,
    should_exit: Cell<bool>,
    exit_requested: Cell<bool>,
    reload_in_progress: Cell<bool>,
    reload_requested: Cell<bool>,
    hard_reload_requested: Cell<bool>,
    primary_down: Cell<bool>,
    pending_move: Cell<Option<(window_state::LogicalPosition, Instant)>>,
    ignore_move_until: Cell<Option<Instant>>,
    pending_captures: RefCell<VecDeque<PendingCapture>>,
    pending_session_ops: RefCell<VecDeque<PendingSessionOp>>,
    pending_child_windows: RefCell<VecDeque<PendingChildWindow>>,
    pending_async_tasks: RefCell<VecDeque<PendingAsyncTask>>,
    pending_deep_links: RefCell<VecDeque<String>>,
    cached_site_data_json: RefCell<String>,
}

const MIN_SET_WINDOW_SIZE: window_state::LogicalSize = window_state::LogicalSize::new(200, 120);
const MAX_SET_WINDOW_SIZE: window_state::LogicalSize = window_state::LogicalSize::new(7680, 4320);

/// How long a move burst must be quiet before its final position is saved.
const MOVE_SAVE_QUIET: Duration = Duration::from_millis(400);

fn hwnd_of(window: &Window) -> isize {
    #[cfg(target_os = "windows")]
    {
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(win32) = handle.as_raw()
        {
            return win32.hwnd.get();
        }
        0
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        0
    }
}

fn monitor_rects(window: &Window) -> Vec<(i32, i32, u32, u32)> {
    window
        .available_monitors()
        .map(|monitor| {
            let origin = monitor.position();
            let size = monitor.size();
            (origin.x, origin.y, size.width, size.height)
        })
        .collect()
}

fn physical_rect_visible(window: &Window, x: i32, y: i32, width: u32, height: u32) -> bool {
    let monitors = monitor_rects(window);
    window_state::window_rect_visible_on_any_monitor(x, y, width, height, &monitors)
}

fn logical_rect_visible(
    window: &Window,
    position: window_state::LogicalPosition,
    size: window_state::LogicalSize,
) -> bool {
    let scale = window.scale_factor();
    let physical_x = (f64::from(position.x) * scale).round() as i32;
    let physical_y = (f64::from(position.y) * scale).round() as i32;
    let physical_w = ((f64::from(size.width) * scale).round().max(1.0)) as u32;
    let physical_h = ((f64::from(size.height) * scale).round().max(1.0)) as u32;
    physical_rect_visible(window, physical_x, physical_y, physical_w, physical_h)
}

fn format_site_data_json(entries: &[SiteData]) -> String {
    let mut out = String::from("[");
    for (idx, entry) in entries.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        let name = entry.name();
        let types = entry.storage_types();
        let mut flags = Vec::new();
        if types.contains(StorageType::Cookies) {
            flags.push("\"cookies\"");
        }
        if types.contains(StorageType::Local) {
            flags.push("\"local\"");
        }
        if types.contains(StorageType::Session) {
            flags.push("\"session\"");
        }
        out.push_str(&format!(
            "{{\"name\":{},\"storage\":[{}]}}",
            win_platform::json_string(&name),
            flags.join(",")
        ));
    }
    out.push(']');
    out
}

impl Delegate {
    fn sync_fullscreen(&self, webview: &WebView) {
        let observed = self.window.fullscreen().is_some();
        if self.last_observed_fullscreen.replace(observed) != observed {
            let data = if observed { "\"true\"" } else { "\"false\"" };
            webview
                .evaluate_javascript(bridge::dispatch_script("fullscreen-changed", data), |_| {});
        }
    }

    fn set_fullscreen(&self, webview: &WebView, enabled: bool) {
        self.window
            .set_fullscreen(enabled.then_some(Fullscreen::Borderless(None)));
        self.sync_fullscreen(webview);
    }

    fn handle_fullscreen_shortcut(
        &self,
        webview: &WebView,
        event: &KeyEvent,
        modifiers: ModifiersState,
        composing: bool,
        is_synthetic: bool,
    ) -> bool {
        let consumed = self.consumed_f11.get() == Some(event.physical_key);
        if event.repeat || is_synthetic {
            return consumed;
        }
        if event.state == ElementState::Released {
            if consumed {
                self.consumed_f11.set(None);
            }
            return consumed;
        }
        if !matches!(event.logical_key, WinitKey::Named(WinitNamedKey::F11)) {
            return false;
        }
        if composing
            || !modifiers.is_empty()
            || self.non_interactive
            || !self.capabilities.allow_window_sizing
        {
            self.consumed_f11.set(None);
            return false;
        }
        self.consumed_f11.set(Some(event.physical_key));
        self.set_fullscreen(webview, self.window.fullscreen().is_none());
        true
    }

    /// Deliver the pending window position to the app handler. With
    /// `force`, save immediately (presentation switch / reload / close);
    /// otherwise save only once the move burst has been quiet for a while.
    fn flush_pending_move(&self, force: bool) {
        let Some((position, at)) = self.pending_move.get() else {
            return;
        };
        if !force && at.elapsed() < MOVE_SAVE_QUIET {
            return;
        }
        self.pending_move.set(None);
        self.handler.borrow_mut().on_window_moved(position);
    }

    fn set_window_visible(&self, visible: bool) {
        self.window_visible.set(visible);
        self.window.set_visible(visible);
        if visible {
            self.window.focus_window();
        }
    }

    fn deliver_notification(&self, title: &str, body: &str) {
        if !self.capabilities.allow_notifications {
            return;
        }
        if self.non_interactive {
            eprintln!("[notification] {title}: {body}");
            return;
        }
        let shown = self
            .tray
            .as_ref()
            .is_some_and(|tray| tray.show_notification(title, body));
        if !shown {
            eprintln!("[notification] {title}: {body}");
        }
    }

    fn queue_capture(
        &self,
        format: CaptureFormat,
        payload: &str,
        call_id: Option<u64>,
    ) -> Result<String, String> {
        let (default_name, ext) = match format {
            CaptureFormat::Png => ("capture.png", ".png"),
            CaptureFormat::Pdf => ("capture.pdf", ".pdf"),
        };
        let raw_name = win_platform::json_get_string(payload, "name")
            .unwrap_or_else(|| default_name.to_owned());
        let mut suggested = win_platform::sanitize_suggested_filename(&raw_name);
        if !suggested.to_ascii_lowercase().ends_with(ext) {
            suggested.push_str(ext);
        }
        if self.non_interactive {
            if let Some(cid) = call_id {
                let _ = self.waker.0.send_event(HostEvent::AsyncIpcReply {
                    target_window: self.window.id(),
                    call_id: cid,
                    ok: true,
                    payload: "dry-run".to_owned(),
                    is_json: false,
                });
            }
            return Ok("dry-run".to_owned());
        }
        self.pending_captures
            .borrow_mut()
            .push_back(PendingCapture {
                format,
                suggested,
                call_id,
            });
        self.waker.wake();
        Ok("queued".to_owned())
    }

    /// Perform a host window/platform command sent through the authenticated
    /// preload bridge (`dn-tok:<nonce>:dn-window:*`).
    ///
    /// These commands are intercepted before the application handler: window
    /// and OS platform management is a host responsibility, identical for
    /// every app. Close, reload, capture, session ops, and child windows are
    /// queued here — the prompt is answered first, and `about_to_wait` runs
    /// the guarded/async flow.
    fn handle_window_command(
        &self,
        command: WindowCommand,
        payload: &str,
        call_id: Option<u64>,
        webview: &WebView,
    ) -> Result<String, String> {
        self.capabilities
            .check_window_command_name(command.as_str())?;
        let hwnd = hwnd_of(&self.window);
        match command {
            WindowCommand::Drag => {
                // `drag_window` is only reliable while the primary button
                // press that asked for the drag is still held.
                if !self.primary_down.get() {
                    return Err(
                        "dn-window:drag はマウスボタンを押したまま呼ぶ必要があります".to_owned(),
                    );
                }
                self.window
                    .drag_window()
                    .map_err(|err| format!("ウィンドウのドラッグ開始に失敗: {err:?}"))?;
                // On Windows the OS modal move loop consumes the matching
                // mouse-up, so clear the button state immediately once the
                // drag starts.
                self.primary_down.set(false);
            }
            WindowCommand::DragResize => {
                if !self.primary_down.get() {
                    return Err(
                        "dn-window:drag-resize はマウスボタンを押したまま呼ぶ必要があります"
                            .to_owned(),
                    );
                }
                let dir = match payload.trim().to_ascii_lowercase().as_str() {
                    "n" | "north" => ResizeDirection::North,
                    "s" | "south" => ResizeDirection::South,
                    "e" | "east" => ResizeDirection::East,
                    "w" | "west" => ResizeDirection::West,
                    "ne" | "northeast" => ResizeDirection::NorthEast,
                    "nw" | "northwest" => ResizeDirection::NorthWest,
                    "se" | "southeast" => ResizeDirection::SouthEast,
                    "sw" | "southwest" => ResizeDirection::SouthWest,
                    other => return Err(format!("不正なリサイズ方向です: {other}")),
                };
                self.window
                    .drag_resize_window(dir)
                    .map_err(|err| format!("ウィンドウのリサイズ開始に失敗: {err:?}"))?;
                self.primary_down.set(false);
            }
            WindowCommand::Minimize => {
                self.window.set_minimized(true);
            }
            WindowCommand::ToggleMaximize => {
                let next = !self.window.is_maximized();
                self.window.set_maximized(next);
                return Ok(if next { "true" } else { "false" }.to_owned());
            }
            WindowCommand::IsMaximized => {
                return Ok(if self.window.is_maximized() {
                    "true"
                } else {
                    "false"
                }
                .to_owned());
            }
            WindowCommand::SetTitle => {
                let title = if payload.trim().is_empty() {
                    self.explicit_title.replace(None);
                    self.default_title.clone()
                } else {
                    let title = bridge::sanitize_window_title(payload, &self.default_title);
                    self.explicit_title.replace(Some(title.clone()));
                    title
                };
                self.window.set_title(&title);
                return Ok(title);
            }
            WindowCommand::SetFullscreen => {
                let enabled = match payload.trim() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(format!("invalid boolean value: {payload}")),
                };
                if !self.non_interactive {
                    self.set_fullscreen(webview, enabled);
                }
                return Ok(enabled.to_string());
            }
            WindowCommand::IsFullscreen => {
                return Ok(self.window.fullscreen().is_some().to_string());
            }
            WindowCommand::SetAlwaysOnTop => {
                let enabled = match payload.trim() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(format!("invalid boolean value: {payload}")),
                };
                if self.non_interactive {
                    return Ok(enabled.to_string());
                }
                self.window.set_window_level(if enabled {
                    WindowLevel::AlwaysOnTop
                } else {
                    WindowLevel::Normal
                });
                return Ok(enabled.to_string());
            }
            WindowCommand::Focus => {
                if self.non_interactive {
                    return Ok("ok".to_owned());
                }
                self.window.focus_window();
                return Ok("ok".to_owned());
            }
            WindowCommand::RequestUserAttention => {
                let kind = match payload.trim() {
                    "critical" => Some(UserAttentionType::Critical),
                    "informational" => Some(UserAttentionType::Informational),
                    "" | "none" => None,
                    _ => return Err(format!("invalid user attention level: {payload}")),
                };
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                self.window.request_user_attention(kind);
                return Ok("ok".to_owned());
            }
            WindowCommand::GetBounds => {
                let scale = self.window.scale_factor();
                let (x, y) = self.window.outer_position().map_or_else(
                    |_| ("null".to_owned(), "null".to_owned()),
                    |position| {
                        let pos = position.to_logical::<i32>(scale);
                        (pos.x.to_string(), pos.y.to_string())
                    },
                );
                let size = self.window.inner_size().to_logical::<u32>(scale);
                let maximized = self.window.is_maximized();
                let fullscreen = self.window.fullscreen().is_some();
                return Ok(format!(
                    "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"maximized\":{},\"fullscreen\":{},\"scaleFactor\":{:.4}}}",
                    x, y, size.width, size.height, maximized, fullscreen, scale,
                ));
            }
            WindowCommand::SetSize => {
                let dimension = |key| {
                    win_platform::json_get_strict_u32(payload, key)
                        .filter(|&value| value > 0)
                        .ok_or_else(|| format!("{key} must be a positive integer"))
                };
                let size = window_state::clamp_size(
                    window_state::LogicalSize::new(dimension("width")?, dimension("height")?),
                    self.min_size,
                    self.max_size,
                );
                if !self.non_interactive
                    && let Some(applied) = self
                        .window
                        .request_inner_size(LogicalSize::new(size.width, size.height))
                {
                    webview.resize(applied);
                    let actual = applied.to_logical::<u32>(self.window.scale_factor());
                    return Ok(format!("{}x{}", actual.width, actual.height));
                }
                return Ok(format!("{}x{}", size.width, size.height));
            }
            WindowCommand::GoBack => {
                if webview.can_go_back() {
                    let _ = webview.go_back(1);
                    return Ok("true".to_owned());
                }
                return Ok("false".to_owned());
            }
            WindowCommand::GoForward => {
                if webview.can_go_forward() {
                    let _ = webview.go_forward(1);
                    return Ok("true".to_owned());
                }
                return Ok("false".to_owned());
            }
            WindowCommand::CanGoBack => {
                return Ok(if webview.can_go_back() {
                    "true"
                } else {
                    "false"
                }
                .to_owned());
            }
            WindowCommand::CanGoForward => {
                return Ok(if webview.can_go_forward() {
                    "true"
                } else {
                    "false"
                }
                .to_owned());
            }
            WindowCommand::ShowChips => self.apply_presentation(webview, Presentation::Chips)?,
            WindowCommand::ShowFlyout => {
                self.apply_presentation(webview, Presentation::Flyout)?;
            }
            WindowCommand::Hide => {
                if !self.tray_active {
                    return Err(
                        "トレイが無効なためウィンドウを非表示にできません（--tray を指定してください）"
                            .to_owned(),
                    );
                }
                self.set_window_visible(false);
            }
            WindowCommand::Show => self.set_window_visible(true),
            WindowCommand::Close => {
                self.exit_requested.set(true);
                self.waker.wake();
            }
            WindowCommand::Reload => {
                self.reload_requested.set(true);
                self.waker.wake();
            }
            WindowCommand::HardReload => {
                self.hard_reload_requested.set(true);
                self.waker.wake();
            }
            WindowCommand::ZoomIn => {
                let next = window_state::step_zoom(webview.page_zoom(), 1);
                webview.set_page_zoom(next);
                return Ok(format!("{next:.1}"));
            }
            WindowCommand::ZoomOut => {
                let next = window_state::step_zoom(webview.page_zoom(), -1);
                webview.set_page_zoom(next);
                return Ok(format!("{next:.1}"));
            }
            WindowCommand::ZoomReset => {
                webview.set_page_zoom(1.0);
                return Ok("1.0".to_owned());
            }
            WindowCommand::OpenExternal => {
                if !bridge::allows_external_url(payload) {
                    return Err(format!("許可されていない外部 URL です: {payload}"));
                }
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                embedder_ui::open_external_url(payload)?;
            }
            WindowCommand::SetOpacity => {
                let value: f64 = payload
                    .trim()
                    .parse()
                    .map_err(|_| format!("不透明度の数値解析に失敗しました: {payload}"))?;
                if self.non_interactive {
                    let alpha = win_platform::opacity_to_alpha(value)
                        .ok_or_else(|| "不正な不透明度です".to_owned())?;
                    return Ok(format!("{:.2}", f64::from(alpha) / 255.0));
                }
                win_platform::set_window_opacity(hwnd, value)?;
            }
            WindowCommand::SetClickThrough => {
                let enabled = match payload.trim() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(format!("不正な真偽値です: {payload}")),
                };
                if self.non_interactive {
                    return Ok(enabled.to_string());
                }
                win_platform::set_window_click_through(hwnd, enabled)?;
            }
            WindowCommand::CapturePng => {
                return self.queue_capture(CaptureFormat::Png, payload, call_id);
            }
            WindowCommand::ExportPdf => {
                return self.queue_capture(CaptureFormat::Pdf, payload, call_id);
            }
            WindowCommand::ExportHtml => {
                let raw_name = win_platform::json_get_string(payload, "name")
                    .unwrap_or_else(|| "export.html".to_owned());
                let mut suggested = win_platform::sanitize_suggested_filename(&raw_name);
                let lower = suggested.to_ascii_lowercase();
                if !lower.ends_with(".html") && !lower.ends_with(".htm") {
                    suggested.push_str(".html");
                }
                let text = win_platform::json_get_string(payload, "text")
                    .ok_or_else(|| "エクスポートする HTML (text) が不足しています".to_owned())?;
                if text.len() > MAX_CAPABILITY_FILE_BYTES {
                    return Err("HTML データサイズが上限を超えています".to_owned());
                }
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                let Some(dest) = embedder_ui::show_save_file_dialog(
                    hwnd,
                    &suggested,
                    &["html".to_owned()],
                    self.non_interactive,
                ) else {
                    return Err("cancelled".to_owned());
                };
                std::fs::write(&dest, text.as_bytes())
                    .map_err(|err| format!("HTML 保存に失敗しました: {err}"))?;
                let saved_name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(suggested);
                return Ok(saved_name);
            }
            WindowCommand::SaveDownload | WindowCommand::DialogSaveFile => {
                let raw_name = win_platform::json_get_string(payload, "name")
                    .unwrap_or_else(|| "download.bin".to_owned());
                let suggested = win_platform::sanitize_suggested_filename(&raw_name);
                let mut extensions =
                    win_platform::json_get_string_array(payload, "extensions").unwrap_or_default();
                if extensions.is_empty()
                    && let Some((_, ext)) = suggested.rsplit_once('.')
                    && !ext.is_empty()
                {
                    extensions.push(ext.to_owned());
                }
                let bytes = if let Some(b64) = win_platform::json_get_string(payload, "dataBase64")
                {
                    win_platform::base64_decode(&b64, MAX_CAPABILITY_FILE_BYTES)?
                } else if let Some(text) = win_platform::json_get_string(payload, "text") {
                    if text.len() > MAX_CAPABILITY_FILE_BYTES {
                        return Err("保存データサイズが上限を超えています".to_owned());
                    }
                    text.into_bytes()
                } else {
                    return Err("保存データ (text または dataBase64) が不足しています".to_owned());
                };
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                let Some(dest) = embedder_ui::show_save_file_dialog(
                    hwnd,
                    &suggested,
                    &extensions,
                    self.non_interactive,
                ) else {
                    return Err("cancelled".to_owned());
                };
                std::fs::write(&dest, &bytes)
                    .map_err(|err| format!("ファイル保存に失敗しました: {err}"))?;
                let saved_name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(suggested);
                return Ok(saved_name);
            }
            WindowCommand::DialogOpenFile => {
                let extensions =
                    win_platform::json_get_string_array(payload, "extensions").unwrap_or_default();
                let multiple = win_platform::json_get_bool(payload, "multiple").unwrap_or(false);
                let Some(paths) = embedder_ui::show_open_file_dialog(
                    hwnd,
                    &extensions,
                    multiple,
                    self.non_interactive,
                ) else {
                    return Err("cancelled".to_owned());
                };
                return Ok(self.files.borrow_mut().register_batch_json(&paths));
            }
            WindowCommand::DialogSelectFolder => {
                let title = if payload.trim().is_empty() {
                    "ダウンロード先フォルダを選択"
                } else {
                    payload.trim()
                };
                let Some(folder) =
                    embedder_ui::show_select_folder_dialog(hwnd, title, self.non_interactive)
                else {
                    return Err("cancelled".to_owned());
                };
                return Ok(folder.to_string_lossy().into_owned());
            }
            WindowCommand::FileReadText => {
                return self.files.borrow().read_text(payload);
            }
            WindowCommand::FileReadBase64 => {
                return self.files.borrow().read_base64(payload);
            }
            WindowCommand::SafeStorageEncrypt => {
                if payload.len() > MAX_CAPABILITY_FILE_BYTES {
                    return Err("暗号化データが大きすぎます".to_owned());
                }
                let encrypted = win_platform::dpapi_encrypt(payload.as_bytes())?;
                return Ok(win_platform::base64_encode(&encrypted));
            }
            WindowCommand::SafeStorageDecrypt => {
                let ciphertext = win_platform::base64_decode(payload, MAX_CAPABILITY_FILE_BYTES)?;
                let plain = win_platform::dpapi_decrypt(&ciphertext)?;
                return String::from_utf8(plain)
                    .map_err(|_| "復号データが UTF-8 ではありません".to_owned());
            }
            WindowCommand::Notify => {
                let title = win_platform::json_get_string(payload, "title")
                    .unwrap_or_else(|| self.default_title.clone());
                let body = win_platform::json_get_string(payload, "body").unwrap_or_default();
                self.deliver_notification(&title, &body);
            }
            WindowCommand::GetTheme => {
                return Ok(if self.dark_theme.get() {
                    "dark".to_owned()
                } else {
                    "light".to_owned()
                });
            }
            WindowCommand::GetPlatform => {
                return Ok(crate::unix_platform::platform_capabilities().to_json());
            }
            WindowCommand::ClipboardReadText => {
                if self.non_interactive {
                    return Ok(String::new());
                }
                return win_platform::clipboard_read_text(hwnd);
            }
            WindowCommand::ClipboardWriteText => {
                if payload.len() > win_platform::MAX_CLIPBOARD_BYTES {
                    return Err("クリップボードテキストが大きすぎます".to_owned());
                }
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                win_platform::clipboard_write_text(hwnd, payload)?;
            }
            WindowCommand::ClipboardReadHtml => {
                if self.non_interactive {
                    return Ok(String::new());
                }
                return win_platform::clipboard_read_html(hwnd);
            }
            WindowCommand::ClipboardWriteHtml => {
                if payload.len() > win_platform::MAX_CLIPBOARD_BYTES {
                    return Err("クリップボード HTML が大きすぎます".to_owned());
                }
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                win_platform::clipboard_write_html(hwnd, payload)?;
            }
            WindowCommand::ClipboardReadImage => {
                if self.non_interactive {
                    return Ok(
                        "{\"width\":0,\"height\":0,\"pngBase64\":\"\",\"dataUrl\":\"\"}".to_owned(),
                    );
                }
                return win_platform::clipboard_read_image_json(hwnd);
            }
            WindowCommand::ClipboardWriteImage => {
                if let Some(bmp_b64) = win_platform::json_get_string(payload, "bmpBase64")
                    && !bmp_b64.is_empty()
                {
                    let bmp =
                        win_platform::base64_decode(&bmp_b64, win_platform::MAX_CLIPBOARD_BYTES)?;
                    let dib = win_platform::decode_bmp_to_dib(&bmp)?;
                    if self.non_interactive {
                        return Ok("dry-run".to_owned());
                    }
                    win_platform::clipboard_write_image_dib(hwnd, &dib)?;
                } else if let Some(rgba_b64) = win_platform::json_get_string(payload, "rgbaBase64")
                    && !rgba_b64.is_empty()
                {
                    let width = win_platform::json_get_strict_u32(payload, "width")
                        .ok_or_else(|| "画像幅 (width) が指定されていません".to_owned())?;
                    let height = win_platform::json_get_strict_u32(payload, "height")
                        .ok_or_else(|| "画像高さ (height) が指定されていません".to_owned())?;
                    let rgba =
                        win_platform::base64_decode(&rgba_b64, win_platform::MAX_CLIPBOARD_BYTES)?;
                    if self.non_interactive {
                        win_platform::rgba_to_dib(width, height, &rgba)?;
                        return Ok("dry-run".to_owned());
                    }
                    win_platform::clipboard_write_image_rgba(hwnd, width, height, &rgba)?;
                } else {
                    return Err(
                        "画像データ (rgbaBase64 または bmpBase64) が不足しています".to_owned()
                    );
                }
            }
            WindowCommand::ClipboardClear => {
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                win_platform::clipboard_clear(hwnd)?;
            }
            WindowCommand::AutostartGet => {
                if self.non_interactive {
                    return Ok("false".to_owned());
                }
                let enabled = win_platform::autostart_is_enabled(self.autostart_app_name)?;
                return Ok(if enabled {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                });
            }
            WindowCommand::AutostartSet => {
                let enabled = match payload.trim() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(format!("不正な真偽値です: {payload}")),
                };
                if self.non_interactive {
                    return Ok(enabled.to_string());
                }
                let exe = std::env::current_exe()
                    .map_err(|err| format!("実行ファイルパスの取得に失敗しました: {err}"))?;
                win_platform::autostart_set_enabled_for_exe(
                    self.autostart_app_name,
                    enabled,
                    &exe,
                    &self.autostart_args,
                )?;
                return Ok(enabled.to_string());
            }
            WindowCommand::ProtocolClientSet => {
                let scheme = win_platform::validate_custom_uri_scheme(payload)?;
                if self.non_interactive {
                    return Ok(scheme);
                }
                let exe = std::env::current_exe()
                    .map_err(|err| format!("実行ファイルパスの取得に失敗しました: {err}"))?;
                win_platform::register_uri_scheme_for_exe(
                    &scheme,
                    self.autostart_app_name,
                    &exe,
                    &self.autostart_args,
                )?;
                return Ok(scheme);
            }
            WindowCommand::ProtocolClientRemove => {
                let scheme = win_platform::validate_custom_uri_scheme(payload)?;
                if self.non_interactive {
                    return Ok(scheme);
                }
                win_platform::unregister_uri_scheme(&scheme)?;
                return Ok(scheme);
            }
            WindowCommand::SessionListSiteData => {
                self.pending_session_ops
                    .borrow_mut()
                    .push_back(PendingSessionOp::ListSiteData { call_id });
                self.waker.wake();
                return Ok(self.cached_site_data_json.borrow().clone());
            }
            WindowCommand::SessionClearSiteData => {
                self.pending_session_ops
                    .borrow_mut()
                    .push_back(PendingSessionOp::ClearSiteData { call_id });
                self.waker.wake();
                return Ok("queued".to_owned());
            }
            WindowCommand::SessionClearCookies => {
                let session_only = payload.trim() == "session";
                self.pending_session_ops
                    .borrow_mut()
                    .push_back(PendingSessionOp::ClearCookies {
                        session_only,
                        call_id,
                    });
                self.waker.wake();
                return Ok("queued".to_owned());
            }
            WindowCommand::SessionClearCache => {
                self.pending_session_ops
                    .borrow_mut()
                    .push_back(PendingSessionOp::ClearCache { call_id });
                self.waker.wake();
                return Ok("queued".to_owned());
            }
            WindowCommand::OpenChildWindow => {
                let url_str = win_platform::json_get_string(payload, "url")
                    .unwrap_or_else(|| "about:blank".to_owned());
                if !bridge::allows_child_window_url(
                    &url_str,
                    self.initial_url.as_str(),
                    self.initial_file_dir.as_deref(),
                    self.dev_origin.as_ref(),
                ) {
                    return Err(format!(
                        "子ウィンドウで許可されていない URL です: {url_str}"
                    ));
                }
                let url = Url::parse(&url_str)
                    .map_err(|err| format!("子ウィンドウ URL の解析に失敗しました: {err}"))?;
                let title = win_platform::json_get_string(payload, "title")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| self.default_title.clone());
                let raw_w = win_platform::json_get_u32(payload, "width").unwrap_or(800) as i32;
                let raw_h = win_platform::json_get_u32(payload, "height").unwrap_or(600) as i32;
                let (width, height) = window_state::clamp_window_request_size(raw_w, raw_h);
                if self.non_interactive {
                    return Ok("dry-run".to_owned());
                }
                self.pending_child_windows
                    .borrow_mut()
                    .push_back(PendingChildWindow::FromBridge {
                        origin_window: self.window.id(),
                        url,
                        title,
                        width,
                        height,
                        call_id,
                    });
                self.waker.wake();
                return Ok("queued".to_owned());
            }
        }
        Ok(String::new())
    }

    fn handle_window_command_async(
        &self,
        call_id: u64,
        command: WindowCommand,
        payload: &str,
        webview: &WebView,
    ) -> Result<String, String> {
        self.capabilities
            .check_window_command_name(command.as_str())?;
        match command {
            WindowCommand::CapturePng
            | WindowCommand::ExportPdf
            | WindowCommand::SessionListSiteData
            | WindowCommand::SessionClearSiteData
            | WindowCommand::SessionClearCookies
            | WindowCommand::SessionClearCache
            | WindowCommand::OpenChildWindow => {
                let _ = self.handle_window_command(command, payload, Some(call_id), webview)?;
                Ok("queued".to_owned())
            }
            WindowCommand::DialogSelectFolder => {
                let title = if payload.trim().is_empty() {
                    "ダウンロード先フォルダを選択".to_owned()
                } else {
                    payload.trim().to_owned()
                };
                let hwnd = hwnd_of(&self.window);
                let target_window = self.window.id();
                let non_interactive = self.non_interactive;
                let proxy = self.waker.0.clone();
                std::thread::spawn(move || {
                    let (ok, out) =
                        match embedder_ui::show_select_folder_dialog(hwnd, &title, non_interactive)
                        {
                            Some(folder) => (true, folder.to_string_lossy().into_owned()),
                            None => (false, "cancelled".to_owned()),
                        };
                    let _ = proxy.send_event(HostEvent::AsyncIpcReply {
                        target_window,
                        call_id,
                        ok,
                        payload: out,
                        is_json: false,
                    });
                });
                Ok("queued".to_owned())
            }
            WindowCommand::SafeStorageEncrypt => {
                if payload.len() > MAX_CAPABILITY_FILE_BYTES {
                    return Err("暗号化データが大きすぎます".to_owned());
                }
                let text = payload.to_owned();
                let target_window = self.window.id();
                let proxy = self.waker.0.clone();
                std::thread::spawn(move || {
                    let res = win_platform::dpapi_encrypt(text.as_bytes())
                        .map(|enc| win_platform::base64_encode(&enc));
                    let (ok, out) = match res {
                        Ok(s) => (true, s),
                        Err(e) => (false, e),
                    };
                    let _ = proxy.send_event(HostEvent::AsyncIpcReply {
                        target_window,
                        call_id,
                        ok,
                        payload: out,
                        is_json: false,
                    });
                });
                Ok("queued".to_owned())
            }
            WindowCommand::SafeStorageDecrypt => {
                let b64 = payload.to_owned();
                let target_window = self.window.id();
                let proxy = self.waker.0.clone();
                std::thread::spawn(move || {
                    let res = win_platform::base64_decode(&b64, MAX_CAPABILITY_FILE_BYTES)
                        .and_then(|ct| win_platform::dpapi_decrypt(&ct))
                        .and_then(|plain| {
                            String::from_utf8(plain)
                                .map_err(|_| "復号データが UTF-8 ではありません".to_owned())
                        });
                    let (ok, out) = match res {
                        Ok(s) => (true, s),
                        Err(e) => (false, e),
                    };
                    let _ = proxy.send_event(HostEvent::AsyncIpcReply {
                        target_window,
                        call_id,
                        ok,
                        payload: out,
                        is_json: false,
                    });
                });
                Ok("queued".to_owned())
            }
            other => {
                let parse_json = matches!(
                    other,
                    WindowCommand::DialogOpenFile | WindowCommand::ClipboardReadImage
                );
                self.pending_async_tasks
                    .borrow_mut()
                    .push_back(PendingAsyncTask::Window {
                        call_id,
                        command: other,
                        payload: payload.to_owned(),
                        parse_json,
                    });
                self.waker.wake();
                Ok("queued".to_owned())
            }
        }
    }

    /// Resize the window to a presentation, restore its saved position, and
    /// let the page follow along.
    fn apply_presentation(
        &self,
        webview: &WebView,
        presentation: Presentation,
    ) -> Result<(), String> {
        if !self.presentations {
            return Err("このアプリでは表示モード切替は無効です".to_owned());
        }
        // The pending position still belongs to the old presentation.
        self.flush_pending_move(true);
        let size = presentation.size();
        if let Some(applied) = self.window.request_inner_size(LogicalSize::new(
            f64::from(size.width),
            f64::from(size.height),
        )) {
            // The size was applied synchronously; keep Servo in sync now
            // instead of waiting for a Resized event that may not come.
            webview.resize(applied);
        }
        let restore = self
            .handler
            .borrow_mut()
            .on_presentation_changed(presentation);
        if let Some(position) = restore
            && logical_rect_visible(&self.window, position, size)
        {
            self.ignore_move_until
                .set(Some(Instant::now() + PROGRAMMATIC_MOVE_GRACE));
            self.window
                .set_outer_position(LogicalPosition::new(position.x, position.y));
        }
        let data = format!("\"{}\"", presentation.config_value());
        webview.evaluate_javascript(bridge::dispatch_script("presentation", &data), |_| {});
        Ok(())
    }
}

impl WebViewDelegate for Delegate {
    fn screen_geometry(&self, _: WebView) -> Option<ScreenGeometry> {
        let monitor_size = self
            .window
            .current_monitor()
            .or_else(|| self.window.primary_monitor())
            .map(|monitor| {
                let size = monitor.size();
                (size.width, size.height)
            });
        let outer_pos = self.window.outer_position().unwrap_or_default();
        let outer_size = self.window.outer_size();
        let rects = window_state::compute_screen_geometry(
            monitor_size,
            (outer_pos.x, outer_pos.y),
            (outer_size.width, outer_size.height),
        );
        Some(ScreenGeometry {
            size: DeviceIntSize::new(rects.screen_width, rects.screen_height),
            available_size: DeviceIntSize::new(rects.available_width, rects.available_height),
            window_rect: DeviceIntRect::from_origin_and_size(
                DeviceIntPoint::new(rects.window_x, rects.window_y),
                DeviceIntSize::new(rects.window_width, rects.window_height),
            ),
        })
    }

    fn notify_new_frame_ready(&self, _: WebView) {
        self.window.request_redraw();
    }

    fn notify_animating_changed(&self, _: WebView, animating: bool) {
        self.animating.set(animating);
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        if status == LoadStatus::Complete {
            self.load_complete.set(true);
            self.waker.wake();
        }
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        if self.explicit_title.borrow().is_none() {
            let title = title
                .as_deref()
                .map(|title| bridge::sanitize_window_title(title, &self.default_title));
            self.window
                .set_title(title.as_deref().unwrap_or(&self.default_title));
        }
    }

    fn notify_cursor_changed(&self, _: WebView, cursor: Cursor) {
        input::set_cursor(&self.window, cursor);
    }

    fn notify_crashed(&self, webview: WebView, reason: String, backtrace: Option<String>) {
        eprintln!("[webview crashed] {reason}");
        if let Some(ref bt) = backtrace {
            eprintln!("{bt}");
        }
        let url_str = webview.url().map(|u| u.to_string()).unwrap_or_default();
        let report = win_platform::CrashReport::new(
            &self.default_title,
            &url_str,
            "webview-crashed",
            &reason,
            backtrace,
        );
        if let Ok(path) = win_platform::write_crash_report(&self.crash_dir, &report) {
            eprintln!("[crash-report] wrote {}", path.display());
        }
    }

    fn notify_closed(&self, _: WebView) {
        self.close_in_progress.set(false);
        self.close_started_at.set(None);
        self.should_exit.set(true);
        self.waker.wake();
    }

    fn request_create_new(&self, _parent_webview: WebView, request: CreateNewWebViewRequest) {
        if !self.capabilities.allow_child_windows {
            eprintln!("[security] child window creation denied by app capability policy");
            return;
        }
        if self.non_interactive {
            return;
        }
        self.pending_child_windows
            .borrow_mut()
            .push_back(PendingChildWindow::FromServoRequest(request));
        self.waker.wake();
    }

    /// Navigation guard (Milestone B + Stage 1 `--dev-url` loopback origin):
    /// Draconium apps are confined strictly to their initial origin (and
    /// canonicalized initial directory when launched from `file://`).
    fn request_navigation(&self, _: WebView, request: NavigationRequest) {
        if bridge::allows_navigation_for_origin_with_fs(
            request.url.as_str(),
            self.initial_url.as_str(),
            self.initial_file_dir.as_deref(),
            self.dev_origin.as_ref(),
        ) {
            request.allow();
        } else {
            request.deny();
        }
    }

    /// Default-deny permission policy (Milestone C): local apps do not get
    /// camera/microphone/geolocation/notification permissions silently.
    fn request_permission(&self, _: WebView, request: PermissionRequest) {
        eprintln!(
            "[permission denied] {:?} (local Draconium default policy)",
            request.feature()
        );
        request.deny();
    }

    fn request_move_to(&self, _: WebView, point: DeviceIntPoint) {
        if !self.capabilities.allow_window_sizing {
            return;
        }
        let size = self.window.outer_size();
        if physical_rect_visible(&self.window, point.x, point.y, size.width, size.height) {
            self.ignore_move_until
                .set(Some(Instant::now() + PROGRAMMATIC_MOVE_GRACE));
            self.window
                .set_outer_position(PhysicalPosition::new(point.x, point.y));
        }
    }

    fn request_resize_to(&self, webview: WebView, requested_outer_size: DeviceIntSize) {
        if !self.capabilities.allow_window_sizing {
            return;
        }
        let (width, height) = window_state::clamp_window_request_size(
            requested_outer_size.width,
            requested_outer_size.height,
        );
        if let Some(applied) = self
            .window
            .request_inner_size(PhysicalSize::new(width, height))
        {
            webview.resize(applied);
        }
    }

    /// Forward desktop notification requests from the WebView to the tray
    /// balloon (`NIF_INFO`) or host stderr fallback.
    fn show_notification(&self, _: WebView, notification: Notification) {
        self.deliver_notification(&notification.title, &notification.body);
    }

    /// Forward page console output and uncaught JS errors to the host log
    /// (Milestone C). Servo reports uncaught exceptions at Error level.
    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        eprintln!("[console {level:?}] {message}");
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        let hwnd = hwnd_of(&self.window);
        match control {
            EmbedderControl::InputMethod(input_method) => {
                self.input_method.set(Some(input_method.id()));
                let rect = input_method.position();
                self.window.set_ime_allowed(true);
                self.window.set_ime_cursor_area(
                    PhysicalPosition::new(rect.min.x, rect.min.y),
                    PhysicalSize::new(rect.width(), rect.height()),
                );
            }
            EmbedderControl::SimpleDialog(SimpleDialog::Prompt(mut prompt)) => {
                // Privileged `dn-window:` and async commands require the per-launch
                // session token closed inside the frozen preload bridge, and are
                // checked against the trusted initial origin (plus populated
                // WebView history URL when available) and the `Capabilities` ACL.
                let trusted_launch_origin = bridge::has_supported_origin(self.initial_url.as_str())
                    && (!bridge::is_file_url(self.initial_url.as_str())
                        || self.initial_file_dir.is_some());
                let same_origin = trusted_launch_origin
                    && webview.url().is_none_or(|current| {
                        bridge::allows_navigation_for_origin_with_fs(
                            current.as_str(),
                            self.initial_url.as_str(),
                            self.initial_file_dir.as_deref(),
                            self.dev_origin.as_ref(),
                        )
                    });
                let reply = if !same_origin {
                    Err("このオリジンではネイティブブリッジを利用できません".to_owned())
                } else {
                    match bridge::classify_command(prompt.message(), &self.session_token) {
                        BridgeCommand::Window(command) => self.handle_window_command(
                            command,
                            prompt.current_value(),
                            None,
                            &webview,
                        ),
                        BridgeCommand::WindowAsync { call_id, command } => self
                            .handle_window_command_async(
                                call_id,
                                command,
                                prompt.current_value(),
                                &webview,
                            ),
                        BridgeCommand::AppAsync { call_id, command } => {
                            match self.capabilities.check_app_command(command) {
                                Ok(()) => {
                                    self.pending_async_tasks.borrow_mut().push_back(
                                        PendingAsyncTask::App {
                                            call_id,
                                            command: command.to_owned(),
                                            payload: prompt.current_value().to_owned(),
                                        },
                                    );
                                    self.waker.wake();
                                    Ok("queued".to_owned())
                                }
                                Err(err) => Err(err),
                            }
                        }
                        BridgeCommand::Rejected(err) => Err(err),
                        BridgeCommand::App(cmd) => match self.capabilities.check_app_command(cmd) {
                            Ok(()) => self
                                .handler
                                .borrow_mut()
                                .handle_request(cmd, prompt.current_value()),
                            Err(err) => Err(err),
                        },
                    }
                };
                let encoded = match reply {
                    Ok(data) => format!("ok:{data}"),
                    Err(err) => format!("err:{err}"),
                };
                prompt.set_current_value(&encoded);
                prompt.confirm();
            }
            EmbedderControl::SimpleDialog(SimpleDialog::Alert(alert)) => {
                embedder_ui::show_alert(
                    hwnd,
                    &self.default_title,
                    alert.message(),
                    self.non_interactive,
                );
                alert.confirm();
            }
            EmbedderControl::SimpleDialog(SimpleDialog::Confirm(confirm)) => {
                if embedder_ui::show_confirm(
                    hwnd,
                    &self.default_title,
                    confirm.message(),
                    self.non_interactive,
                ) {
                    confirm.confirm();
                } else {
                    confirm.dismiss();
                }
            }
            EmbedderControl::SelectElement(mut select) => {
                let mut selected = select.selected_options();
                let mut entries = Vec::<PopupMenuEntry>::new();
                let mut option_ids = Vec::<usize>::new();
                for item in select.options() {
                    match item {
                        SelectElementOptionOrOptgroup::Option(option) => {
                            option_ids.push(option.id);
                            let menu_id = option_ids.len() as u32;
                            entries.push(PopupMenuEntry::item(
                                menu_id,
                                option.label.clone(),
                                !option.is_disabled,
                                selected.contains(&option.id),
                            ));
                        }
                        SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                            if !entries.is_empty() {
                                entries.push(PopupMenuEntry::separator());
                            }
                            entries.push(PopupMenuEntry::item(0, label.clone(), false, false));
                            for option in options {
                                option_ids.push(option.id);
                                let menu_id = option_ids.len() as u32;
                                entries.push(PopupMenuEntry::item(
                                    menu_id,
                                    format!("  {}", option.label),
                                    !option.is_disabled,
                                    selected.contains(&option.id),
                                ));
                            }
                        }
                    }
                }
                let pos = select.position();
                if let Some(menu_id) = embedder_ui::show_popup_menu(
                    hwnd,
                    pos.min.x,
                    pos.max.y,
                    &entries,
                    self.non_interactive,
                ) && let Some(&option_id) = option_ids.get(menu_id.saturating_sub(1) as usize)
                {
                    if select.allow_select_multiple() {
                        if let Some(idx) = selected.iter().position(|&id| id == option_id) {
                            selected.remove(idx);
                        } else {
                            selected.push(option_id);
                        }
                    } else {
                        selected = vec![option_id];
                    }
                    select.select(selected);
                }
                select.submit();
            }
            EmbedderControl::ContextMenu(menu) => {
                let mut entries = Vec::<PopupMenuEntry>::new();
                let mut actions = Vec::<ContextMenuAction>::new();
                for item in menu.items() {
                    match item {
                        ContextMenuItem::Separator => entries.push(PopupMenuEntry::separator()),
                        ContextMenuItem::Item {
                            label,
                            action,
                            enabled,
                        } => {
                            actions.push(*action);
                            let menu_id = actions.len() as u32;
                            entries.push(PopupMenuEntry::item(
                                menu_id,
                                label.clone(),
                                *enabled,
                                false,
                            ));
                        }
                    }
                }
                let pos = menu.position();
                if let Some(menu_id) = embedder_ui::show_popup_menu(
                    hwnd,
                    pos.min.x,
                    pos.min.y,
                    &entries,
                    self.non_interactive,
                ) && let Some(&action) = actions.get(menu_id.saturating_sub(1) as usize)
                {
                    menu.select(action);
                } else {
                    menu.dismiss();
                }
            }
            EmbedderControl::FilePicker(mut picker) => {
                if !self.capabilities.allow_file_dialogs {
                    picker.dismiss();
                    return;
                }
                let extensions: Vec<String> = picker
                    .filter_patterns()
                    .iter()
                    .map(|pattern| pattern.0.clone())
                    .collect();
                if let Some(paths) = embedder_ui::show_open_file_dialog(
                    hwnd,
                    &extensions,
                    picker.allow_select_multiple(),
                    self.non_interactive,
                ) {
                    picker.select(&paths);
                    picker.submit();
                } else {
                    picker.dismiss();
                }
            }
            EmbedderControl::ColorPicker(mut picker) => {
                let initial = picker
                    .current_color()
                    .map(|c| (c.red, c.green, c.blue))
                    .unwrap_or((0, 0, 0));
                if let Some((red, green, blue)) =
                    embedder_ui::show_color_picker(hwnd, initial, self.non_interactive)
                {
                    picker.select(Some(RgbColor { red, green, blue }));
                }
                picker.submit();
            }
        }
    }

    fn hide_embedder_control(&self, _: WebView, id: EmbedderControlId) {
        if self.input_method.get() == Some(id) {
            self.input_method.set(None);
            self.window.set_ime_allowed(false);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReloadShortcut {
    None,
    Reload,
    HardReload,
}

fn classify_reload_shortcut(
    allow_reload_shortcuts: bool,
    composing: bool,
    modifiers: ModifiersState,
    logical_key: &WinitKey,
) -> ReloadShortcut {
    if !allow_reload_shortcuts || composing || modifiers.alt_key() || modifiers.super_key() {
        return ReloadShortcut::None;
    }
    let is_f5 = matches!(logical_key, WinitKey::Named(WinitNamedKey::F5));
    let is_r = matches!(
        logical_key,
        WinitKey::Character(text) if text.eq_ignore_ascii_case("r")
    );
    if (is_f5 && modifiers.control_key())
        || (modifiers.control_key() && modifiers.shift_key() && is_r)
    {
        ReloadShortcut::HardReload
    } else if (is_f5 && !modifiers.control_key()) || (modifiers.control_key() && is_r) {
        ReloadShortcut::Reload
    } else {
        ReloadShortcut::None
    }
}

fn close_timed_out(started: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started) > CLOSE_TIMEOUT
}

fn close_poll_deadline(now: Instant) -> Instant {
    now + CLOSE_POLL_INTERVAL
}

fn request_close(webview: &WebView, delegate: &Rc<Delegate>) {
    if delegate.close_in_progress.get() || delegate.should_exit.get() {
        return;
    }
    delegate.flush_pending_move(true);
    delegate.close_in_progress.set(true);
    delegate.close_started_at.set(Some(Instant::now()));
    let delegate_cb = delegate.clone();
    webview.evaluate_javascript(
        "typeof window.__draconiumBeforeClose === 'function' ? Boolean(window.__draconiumBeforeClose()) : true",
        move |result| {
            match result {
                Ok(JSValue::Boolean(false)) => {
                    delegate_cb.close_in_progress.set(false);
                    delegate_cb.close_started_at.set(None);
                }
                _ => {
                    delegate_cb.close_in_progress.set(false);
                    delegate_cb.close_started_at.set(None);
                    delegate_cb.should_exit.set(true);
                }
            }
            delegate_cb.waker.wake();
        },
    );
}

/// Reload the page through the same before-close hook as closing, so an
/// app with unsaved state (the memo's autosave window) can flush first and
/// veto a reload that would discard it.
fn request_reload(webview: &WebView, delegate: &Rc<Delegate>) {
    if delegate.reload_in_progress.get()
        || delegate.close_in_progress.get()
        || delegate.should_exit.get()
    {
        return;
    }
    delegate.flush_pending_move(true);
    delegate.reload_in_progress.set(true);
    let delegate_cb = delegate.clone();
    let webview_cb = webview.clone();
    webview.evaluate_javascript(
        "typeof window.__draconiumBeforeClose === 'function' ? Boolean(window.__draconiumBeforeClose()) : true",
        move |result| {
            delegate_cb.reload_in_progress.set(false);
            if !matches!(result, Ok(JSValue::Boolean(false)))
                && !delegate_cb.should_exit.get()
                && !delegate_cb.close_in_progress.get()
            {
                webview_cb.reload();
            }
            delegate_cb.waker.wake();
        },
    );
}

fn target_webview(state: &State, target: WindowId) -> Option<&WebView> {
    if state.delegate.window.id() == target {
        Some(&state.webview)
    } else {
        state
            .child_windows
            .iter()
            .find(|child| child.window_id == target)
            .map(|child| &child.webview)
    }
}

fn send_async_reply(
    waker: &Waker,
    target_window: WindowId,
    call_id: u64,
    ok: bool,
    payload: String,
    is_json: bool,
) {
    let _ = waker.0.send_event(HostEvent::AsyncIpcReply {
        target_window,
        call_id,
        ok,
        payload,
        is_json,
    });
}

fn spawn_child_window(
    event_loop: &ActiveEventLoop,
    state: &mut State,
    pending: PendingChildWindow,
) {
    if state.child_windows.len() >= MAX_CHILD_WINDOWS {
        if let PendingChildWindow::FromBridge {
            origin_window,
            call_id: Some(cid),
            ..
        } = pending
        {
            send_async_reply(
                &state.delegate.waker,
                origin_window,
                cid,
                false,
                "子ウィンドウの上限数に達しています".to_owned(),
                false,
            );
        }
        return;
    }

    let (origin_and_call_id, title_owned, width, height, child_initial_url) = match &pending {
        PendingChildWindow::FromBridge {
            origin_window,
            url,
            title,
            width,
            height,
            call_id,
        } => (
            call_id.map(|cid| (*origin_window, cid)),
            title.clone(),
            *width,
            *height,
            url.clone(),
        ),
        PendingChildWindow::FromServoRequest(_) => (
            None,
            state.delegate.default_title.clone(),
            800,
            600,
            state.delegate.initial_url.clone(),
        ),
    };
    let send_open_err = |reason: &str| {
        if let Some((origin_window, cid)) = origin_and_call_id {
            send_async_reply(
                &state.delegate.waker,
                origin_window,
                cid,
                false,
                reason.to_owned(),
                false,
            );
        }
    };

    let attributes = Window::default_attributes()
        .with_title(&title_owned)
        .with_inner_size(LogicalSize::new(f64::from(width), f64::from(height)))
        .with_decorations(true)
        .with_visible(true);
    let Ok(window) = event_loop.create_window(attributes) else {
        send_open_err("子ウィンドウの作成に失敗しました");
        return;
    };
    let window = Rc::new(window);
    let window_id = window.id();
    let dark = state.delegate.dark_theme.get();
    win_platform::apply_dwm_dark_mode(hwnd_of(&window), dark);

    let Ok(display_handle) = event_loop.display_handle() else {
        send_open_err("ディスプレイハンドルの取得に失敗しました");
        return;
    };
    let Ok(win_handle) = window.window_handle() else {
        send_open_err("ウィンドウハンドルの取得に失敗しました");
        return;
    };
    let Ok(rc) = WindowRenderingContext::new(display_handle, win_handle, window.inner_size())
    else {
        send_open_err("レンダリングコンテキストの作成に失敗しました");
        return;
    };
    let child_rc = Rc::new(rc);
    let _ = child_rc.make_current();

    let child_file_dir = bridge::resolve_initial_file_dir(child_initial_url.as_str());
    let child_ucm = Rc::new(UserContentManager::new(&state.servo));
    child_ucm.add_stylesheet(Rc::new(UserStyleSheet::new(
        bridge::POPOVER_POLYFILL_CSS.to_owned(),
        Url::parse("dn://app/dn-popover-polyfill.css").expect("valid static stylesheet URL"),
    )));
    child_ucm.add_script(Rc::new(UserScript::new(
        bridge::render_preload_script_for_origin(
            &state.delegate.session_token,
            child_initial_url.as_str(),
        ),
        None,
    )));

    let child_delegate = Rc::new(Delegate {
        default_title: title_owned,
        explicit_title: RefCell::new(None),
        last_observed_fullscreen: Cell::new(window.fullscreen().is_some()),
        consumed_f11: Cell::new(None),
        min_size: MIN_SET_WINDOW_SIZE,
        max_size: MAX_SET_WINDOW_SIZE,
        window: window.clone(),
        waker: state.delegate.waker.clone(),
        handler: state.delegate.handler.clone(),
        session_token: state.delegate.session_token.clone(),
        capabilities: state.delegate.capabilities.attenuate_for_child(),
        dev_origin: state.delegate.dev_origin.clone(),
        initial_url: child_initial_url,
        initial_file_dir: child_file_dir,
        autostart_app_name: state.delegate.autostart_app_name,
        autostart_args: state.delegate.autostart_args.clone(),
        crash_dir: state.delegate.crash_dir.clone(),
        files: RefCell::new(FileAccessRegistry::new()),
        tray: state.delegate.tray.clone(),
        presentations: false,
        tray_active: false,
        close_to_tray: false,
        non_interactive: state.delegate.non_interactive,
        allow_reload_shortcuts: state.delegate.allow_reload_shortcuts,
        dark_theme: Cell::new(dark),
        window_visible: Cell::new(true),
        input_method: Cell::new(None),
        animating: Cell::new(false),
        load_complete: Cell::new(false),
        has_painted: Cell::new(false),
        close_in_progress: Cell::new(false),
        close_started_at: Cell::new(None),
        should_exit: Cell::new(false),
        exit_requested: Cell::new(false),
        reload_in_progress: Cell::new(false),
        reload_requested: Cell::new(false),
        hard_reload_requested: Cell::new(false),
        primary_down: Cell::new(false),
        pending_move: Cell::new(None),
        ignore_move_until: Cell::new(None),
        pending_captures: RefCell::new(VecDeque::new()),
        pending_session_ops: RefCell::new(VecDeque::new()),
        pending_child_windows: RefCell::new(VecDeque::new()),
        pending_async_tasks: RefCell::new(VecDeque::new()),
        pending_deep_links: RefCell::new(VecDeque::new()),
        cached_site_data_json: RefCell::new("[]".to_owned()),
    });

    let scale = Scale::new(window.scale_factor() as f32);
    let child_webview = match pending {
        PendingChildWindow::FromBridge { url, .. } => {
            WebViewBuilder::new(&state.servo, child_rc.clone())
                .url(url)
                .hidpi_scale_factor(scale)
                .delegate(child_delegate.clone())
                .user_content_manager(child_ucm.clone())
                .build()
        }
        PendingChildWindow::FromServoRequest(request) => request
            .builder(child_rc.clone())
            .hidpi_scale_factor(scale)
            .delegate(child_delegate.clone())
            .user_content_manager(child_ucm.clone())
            .build(),
    };
    child_webview.notify_theme_change(if dark { Theme::Dark } else { Theme::Light });
    child_webview.set_focused(window.has_focus());

    state.child_windows.push(ChildWindowEntry {
        window_id,
        webview: child_webview,
        _user_content_manager: child_ucm,
        rendering_context: child_rc,
        delegate: child_delegate,
        mouse_position: DevicePoint::zero(),
        modifiers: ModifiersState::empty(),
        composing: false,
    });

    if let Some((origin_window, cid)) = origin_and_call_id {
        send_async_reply(
            &state.delegate.waker,
            origin_window,
            cid,
            true,
            "opened".to_owned(),
            false,
        );
    }
}

impl ApplicationHandler<HostEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let config = self.config.take().expect("HostConfig already consumed");
        let handler = self.handler.take().expect("HostHandler already consumed");
        let background_polling = handler.wants_background_polling();
        let non_interactive = self.self_test.is_some() || self.screenshot_job.is_some();

        let min_size = config.window.min_size.map_or(MIN_SET_WINDOW_SIZE, |size| {
            window_state::LogicalSize::new(
                size.width.max(MIN_SET_WINDOW_SIZE.width),
                size.height.max(MIN_SET_WINDOW_SIZE.height),
            )
        });
        let max_size = config.window.max_size.map_or(MAX_SET_WINDOW_SIZE, |size| {
            window_state::LogicalSize::new(
                size.width.min(MAX_SET_WINDOW_SIZE.width),
                size.height.min(MAX_SET_WINDOW_SIZE.height),
            )
        });
        let inconsistent_limits = config
            .window
            .min_size
            .zip(config.window.max_size)
            .is_some_and(|(min, max)| min.width > max.width || min.height > max.height);
        if inconsistent_limits
            || (config.capabilities.allow_window_sizing
                && (min_size.width > max_size.width || min_size.height > max_size.height))
        {
            eprintln!("[window] Inconsistent configured inner size limits");
            event_loop.exit();
            return;
        }

        let single_instance = if let Some(ref mutex_name) = config.single_instance_name {
            match SingleInstanceLock::acquire(mutex_name) {
                Ok(Some(lock)) => Some(lock),
                Ok(None) => {
                    let payload = win_platform::build_second_instance_json(
                        &config.raw_args,
                        config.initial_deep_link.as_deref(),
                    );
                    let forwarded =
                        win_platform::forward_to_running_instance(&config.instance_tag, &payload);
                    eprintln!(
                        "[single-instance] すでに起動しているため終了します ({mutex_name}, forwarded={forwarded})"
                    );
                    event_loop.exit();
                    return;
                }
                Err(err) => {
                    eprintln!("[single-instance] Mutex 取得に失敗しました: {err}");
                    None
                }
            }
        } else {
            None
        };

        #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
        let mut attributes = Window::default_attributes()
            .with_title(config.title)
            .with_inner_size(LogicalSize::new(
                f64::from(config.window.initial_size.width),
                f64::from(config.window.initial_size.height),
            ))
            .with_decorations(config.window.decorations)
            .with_transparent(config.window.transparent)
            .with_window_level(if config.window.always_on_top {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            })
            .with_visible(!config.window.start_hidden);
        if let Some(min_size) = config.window.min_size {
            attributes = attributes.with_min_inner_size(LogicalSize::new(
                f64::from(min_size.width),
                f64::from(min_size.height),
            ));
        }
        if let Some(max_size) = config.window.max_size {
            attributes = attributes.with_max_inner_size(LogicalSize::new(
                f64::from(max_size.width),
                f64::from(max_size.height),
            ));
        }
        #[cfg(target_os = "windows")]
        {
            attributes = attributes.with_skip_taskbar(config.window.skip_taskbar);
        }
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .expect("Failed to create window"),
        );
        if let Some(position) = config.window.initial_position
            && logical_rect_visible(&window, position, config.window.initial_size)
        {
            window.set_outer_position(LogicalPosition::new(position.x, position.y));
        }

        let initial_dark = win_platform::detect_system_dark_mode().unwrap_or(false);
        let hwnd = hwnd_of(&window);
        if !config.window.decorations {
            win_platform::apply_dwm_rounded_corners(hwnd);
        }
        win_platform::apply_dwm_dark_mode(hwnd, initial_dark);
        if let Some(opacity) = config.window.opacity
            && !non_interactive
            && let Err(err) = win_platform::set_window_opacity(hwnd, opacity)
        {
            eprintln!("[window] 初期不透明度の適用をスキップしました: {err}");
        }

        let wants_os_monitor = !non_interactive
            && (config.window.enable_tray
                || config.window.global_hotkey.is_some()
                || cfg!(target_os = "windows")
                || cfg!(unix));
        let tray = if wants_os_monitor {
            let proxy = self.waker.0.clone();
            let tray_cfg = TrayConfig {
                tooltip: config.title.to_owned(),
                presentations: config.window.presentations,
                show_tray_icon: config.window.enable_tray,
                hotkey: config.window.global_hotkey,
                instance_window_tag: config.instance_tag.clone(),
            };
            match TrayHandle::start(tray_cfg, move |action| {
                let _ = proxy.send_event(HostEvent::Tray(action));
            }) {
                Ok(handle) => Some(Rc::new(handle)),
                Err(err) => {
                    if config.window.enable_tray || config.window.global_hotkey.is_some() {
                        eprintln!(
                            "[tray] システムトレイ / OS 監視の初期化をスキップしました: {err}"
                        );
                    }
                    None
                }
            }
        } else {
            None
        };
        let tray_active = tray.as_ref().is_some_and(|handle| handle.has_tray_icon());

        let rendering_context = Rc::new(
            WindowRenderingContext::new(
                event_loop.display_handle().expect("No display handle"),
                window.window_handle().expect("No window handle"),
                window.inner_size(),
            )
            .expect("Failed to create rendering context"),
        );
        rendering_context
            .make_current()
            .expect("Failed to make rendering context current");

        let transparent_mode = config.window.transparent;
        let devtools_enabled = config.devtools_addr.is_some();
        let mut prefs = Preferences::default();
        prefs.layout_css_backdrop_filter_enabled = config.backdrop_filter;
        if transparent_mode {
            prefs.shell_background_color_rgba = [0.0, 0.0, 0.0, 0.0];
        }
        if let Some(addr) = config.devtools_addr.clone() {
            prefs.devtools_server_enabled = true;
            prefs.devtools_server_listen_address = addr;
        }
        let mut servo_builder = ServoBuilder::default()
            .preferences(prefs)
            .event_loop_waker(Box::new(self.waker.clone()));
        // Milestone B + Stage 1: always register `dn://` with disk asset root
        // (if present), compiled-in embedded fallback, and CSP headers.
        {
            let mut registry = servo::protocol_handler::ProtocolRegistry::default();
            let csp = Some(crate::protocol::build_csp_header(config.allow_wasm_csp));
            if registry
                .register(
                    crate::protocol::SCHEME,
                    crate::dn_protocol::DnAssetHandler::new(
                        config.asset_root.clone(),
                        config.embedded_assets,
                        csp,
                    ),
                )
                .is_err()
            {
                eprintln!("dn:// プロトコル登録に失敗しました");
            }
            servo_builder = servo_builder.protocol_registry(registry);
        }
        let servo = servo_builder.build();
        servo.set_delegate(Rc::new(HostServoDelegate { devtools_enabled }));
        servo.setup_logging();

        // Preload bridge + UserStyleSheet (Milestone B + Stage 1 CSP-clean popover shim):
        // every page gets a frozen `window.Draconium` bound to this launch's session token.
        let session_token = bridge::generate_session_token();
        let user_content_manager = Rc::new(UserContentManager::new(&servo));
        user_content_manager.add_stylesheet(Rc::new(UserStyleSheet::new(
            bridge::POPOVER_POLYFILL_CSS.to_owned(),
            Url::parse("dn://app/dn-popover-polyfill.css").expect("valid static stylesheet URL"),
        )));
        if transparent_mode {
            user_content_manager.add_stylesheet(Rc::new(UserStyleSheet::new(
                "html, body { background: transparent !important; background-image: none !important; }"
                    .to_owned(),
                Url::parse("dn://app/dn-transparent.css").expect("valid static stylesheet URL"),
            )));
        }
        user_content_manager.add_script(Rc::new(UserScript::new(
            bridge::render_preload_script_for_origin(&session_token, config.url.as_str()),
            None,
        )));

        let mut initial_deep_links = VecDeque::new();
        if let Some(uri) = config.initial_deep_link {
            initial_deep_links.push_back(uri);
        }

        let initial_url = config.url.clone();
        let initial_file_dir = bridge::resolve_initial_file_dir(initial_url.as_str());
        let delegate = Rc::new(Delegate {
            default_title: config.title.to_owned(),
            explicit_title: RefCell::new(None),
            last_observed_fullscreen: Cell::new(window.fullscreen().is_some()),
            consumed_f11: Cell::new(None),
            min_size,
            max_size,
            window,
            waker: self.waker.clone(),
            handler: Rc::new(RefCell::new(handler)),
            session_token,
            capabilities: config.capabilities,
            dev_origin: config.dev_origin,
            initial_url,
            initial_file_dir,
            autostart_app_name: config.autostart_app_name,
            autostart_args: config.autostart_args,
            crash_dir: config.crash_dir,
            files: RefCell::new(FileAccessRegistry::new()),
            tray: tray.clone(),
            presentations: config.window.presentations,
            tray_active,
            close_to_tray: config.window.close_to_tray && tray_active,
            non_interactive,
            allow_reload_shortcuts: config.allow_reload_shortcuts,
            dark_theme: Cell::new(initial_dark),
            window_visible: Cell::new(!config.window.start_hidden),
            input_method: Cell::new(None),
            animating: Cell::new(false),
            load_complete: Cell::new(false),
            has_painted: Cell::new(false),
            close_in_progress: Cell::new(false),
            close_started_at: Cell::new(None),
            should_exit: Cell::new(false),
            exit_requested: Cell::new(false),
            reload_in_progress: Cell::new(false),
            reload_requested: Cell::new(false),
            hard_reload_requested: Cell::new(false),
            primary_down: Cell::new(false),
            pending_move: Cell::new(None),
            ignore_move_until: Cell::new(Some(Instant::now() + PROGRAMMATIC_MOVE_GRACE)),
            pending_captures: RefCell::new(VecDeque::new()),
            pending_session_ops: RefCell::new(VecDeque::new()),
            pending_child_windows: RefCell::new(VecDeque::new()),
            pending_async_tasks: RefCell::new(VecDeque::new()),
            pending_deep_links: RefCell::new(initial_deep_links),
            cached_site_data_json: RefCell::new("[]".to_owned()),
        });
        let webview = WebViewBuilder::new(&servo, rendering_context.clone())
            .url(config.url)
            .hidpi_scale_factor(Scale::new(delegate.window.scale_factor() as f32))
            .delegate(delegate.clone())
            .user_content_manager(user_content_manager.clone())
            .build();
        webview.notify_theme_change(if initial_dark {
            Theme::Dark
        } else {
            Theme::Light
        });
        webview.set_focused(delegate.window.has_focus());
        self.state = Some(State {
            webview,
            child_windows: Vec::new(),
            _user_content_manager: user_content_manager,
            servo,
            rendering_context,
            delegate,
            _single_instance: single_instance,
            _tray: tray,
            background_polling,
            next_bg_poll: Instant::now() + BACKGROUND_POLL_INTERVAL,
            mouse_position: DevicePoint::zero(),
            modifiers: ModifiersState::empty(),
            composing: false,
            hovering_files: false,
            pending_dropped_files: Vec::new(),
        });
        self.state.as_ref().unwrap().servo.spin_event_loop();
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: HostEvent) {
        let Some(state) = &self.state else {
            return;
        };
        match event {
            HostEvent::Wake => {}
            HostEvent::AsyncIpcReply {
                target_window,
                call_id,
                ok,
                payload,
                is_json,
            } => {
                if let Some(webview) = target_webview(state, target_window) {
                    let script = bridge::resolve_async_script(call_id, ok, &payload, is_json);
                    webview.evaluate_javascript(script, |_| {});
                }
            }
            HostEvent::CaptureFinished {
                target_window,
                call_id,
                event_json,
                reply_result,
            } => {
                if let Some(webview) = target_webview(state, target_window) {
                    webview.evaluate_javascript(
                        bridge::dispatch_script("capture-saved", &event_json),
                        |_| {},
                    );
                    if let Some(cid) = call_id {
                        let (ok, msg) = match reply_result {
                            Ok(name) => (true, name),
                            Err(err) => (false, err),
                        };
                        let script = bridge::resolve_async_script(cid, ok, &msg, false);
                        webview.evaluate_javascript(script, |_| {});
                    }
                }
            }
            HostEvent::Tray(action) => match action {
                TrayAction::ToggleVisible => {
                    let next = !state.delegate.window_visible.get();
                    state.delegate.set_window_visible(next);
                }
                TrayAction::ShowChips => {
                    state.delegate.set_window_visible(true);
                    let _ = state
                        .delegate
                        .apply_presentation(&state.webview, Presentation::Chips);
                }
                TrayAction::ShowFlyout => {
                    state.delegate.set_window_visible(true);
                    let _ = state
                        .delegate
                        .apply_presentation(&state.webview, Presentation::Flyout);
                }
                TrayAction::Quit => {
                    request_close(&state.webview, &state.delegate);
                }
                TrayAction::GlobalHotkey => {
                    let next = !state.delegate.window_visible.get();
                    state.delegate.set_window_visible(next);
                    if state.delegate.load_complete.get() {
                        state.webview.evaluate_javascript(
                            bridge::dispatch_script("global-hotkey", "\"triggered\""),
                            |_| {},
                        );
                    }
                }
                TrayAction::ThemeChanged => {
                    if let Some(dark) = win_platform::detect_system_dark_mode()
                        && dark != state.delegate.dark_theme.get()
                    {
                        state.delegate.dark_theme.set(dark);
                        win_platform::apply_dwm_dark_mode(hwnd_of(&state.delegate.window), dark);
                        let theme = if dark { Theme::Dark } else { Theme::Light };
                        state.webview.notify_theme_change(theme);
                        for child in &state.child_windows {
                            child.delegate.dark_theme.set(dark);
                            win_platform::apply_dwm_dark_mode(
                                hwnd_of(&child.delegate.window),
                                dark,
                            );
                            child.webview.notify_theme_change(theme);
                        }
                        if state.delegate.load_complete.get() {
                            let theme_json = if dark { "\"dark\"" } else { "\"light\"" };
                            state.webview.evaluate_javascript(
                                bridge::dispatch_script("theme-changed", theme_json),
                                |_| {},
                            );
                        }
                    }
                }
                TrayAction::PowerEvent(kind) => {
                    if state.delegate.load_complete.get() {
                        let json = format!("\"{kind}\"");
                        state.webview.evaluate_javascript(
                            bridge::dispatch_script("power-event", &json),
                            |_| {},
                        );
                    }
                }
                TrayAction::NotificationClicked => {
                    state.delegate.set_window_visible(true);
                    if state.delegate.load_complete.get() {
                        state.webview.evaluate_javascript(
                            bridge::dispatch_script("notification-clicked", "{}"),
                            |_| {},
                        );
                    }
                }
                TrayAction::SecondInstance(payload_json) => {
                    state.delegate.set_window_visible(true);
                    if let Some(uri) = win_platform::json_get_string(&payload_json, "deepLink")
                        && !uri.is_empty()
                    {
                        if state.delegate.load_complete.get() {
                            let uri_json = win_platform::json_string(&uri);
                            state.webview.evaluate_javascript(
                                bridge::dispatch_script("deep-link", &uri_json),
                                |_| {},
                            );
                        } else {
                            state
                                .delegate
                                .pending_deep_links
                                .borrow_mut()
                                .push_back(uri);
                        }
                    }
                    if state.delegate.load_complete.get() {
                        state.webview.evaluate_javascript(
                            bridge::dispatch_script("second-instance", &payload_json),
                            |_| {},
                        );
                    }
                }
            },
        }
        state.servo.spin_event_loop();
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let Some(state) = &self.state
            && matches!(cause, StartCause::ResumeTimeReached { .. })
            && (state.delegate.animating.get()
                || state
                    .child_windows
                    .iter()
                    .any(|c| c.delegate.animating.get()))
        {
            state.servo.spin_event_loop();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let (Some(runner), Some(state)) = (&mut self.self_test, &mut self.state) {
            runner.advance(state);
        }
        if let (Some(job), Some(state)) = (&mut self.screenshot_job, &mut self.state) {
            job.advance(state);
        }
        // Deliver any queued initial or second-instance deep links once load completes.
        if let Some(state) = &self.state
            && state.delegate.load_complete.get()
        {
            let mut dispatched = false;
            while let Some(uri) = state.delegate.pending_deep_links.borrow_mut().pop_front() {
                let uri_json = win_platform::json_string(&uri);
                state
                    .webview
                    .evaluate_javascript(bridge::dispatch_script("deep-link", &uri_json), |_| {});
                dispatched = true;
            }
            if dispatched {
                state.servo.spin_event_loop();
            }
        }
        // Flush any winit file-drop batch as a single capability-scoped event.
        if let Some(state) = &mut self.state
            && !state.pending_dropped_files.is_empty()
            && state.delegate.load_complete.get()
        {
            let dropped = std::mem::take(&mut state.pending_dropped_files);
            if state
                .delegate
                .capabilities
                .check(crate::policy::Capability::FileDrop)
                .is_ok()
            {
                let files_json = state
                    .delegate
                    .files
                    .borrow_mut()
                    .register_batch_json(&dropped);
                let scale = state.delegate.window.scale_factor().max(0.1);
                let css_x = (f64::from(state.mouse_position.x) / scale).round();
                let css_y = (f64::from(state.mouse_position.y) / scale).round();
                let detail_json = format!("{{\"x\":{css_x},\"y\":{css_y},\"files\":{files_json}}}");
                state.webview.evaluate_javascript(
                    bridge::dispatch_script("file-drop", &detail_json),
                    |_| {},
                );
                state.servo.spin_event_loop();
            }
        }
        // Interactive PNG or PDF capture queued by `Draconium.window.{capturePng,exportPdf}` or `window.print()`.
        // Session & storage management operations (`SiteDataManager` / `NetworkManager`).
        if let Some(state) = &self.state {
            let mut targets: Vec<(Rc<Delegate>, WebView)> =
                Vec::with_capacity(1 + state.child_windows.len());
            targets.push((state.delegate.clone(), state.webview.clone()));
            for child in &state.child_windows {
                targets.push((child.delegate.clone(), child.webview.clone()));
            }
            let mut ran_capture = false;
            for (delegate, webview) in &targets {
                let target_window = delegate.window.id();
                while let Some(pending) = delegate.pending_captures.borrow_mut().pop_front() {
                    let hwnd = hwnd_of(&delegate.window);
                    let ext = match pending.format {
                        CaptureFormat::Png => "png",
                        CaptureFormat::Pdf => "pdf",
                    };
                    if let Some(dest) = embedder_ui::show_save_file_dialog(
                        hwnd,
                        &pending.suggested,
                        &[ext.to_owned()],
                        delegate.non_interactive,
                    ) {
                        let proxy = delegate.waker.0.clone();
                        let format = pending.format;
                        let call_id = pending.call_id;
                        let fallback_name = pending.suggested.clone();
                        webview.take_screenshot(None, move |capture| match capture {
                            Ok(image) => {
                                let proxy_worker = proxy.clone();
                                std::thread::spawn(move || {
                                    let save_res: Result<(), String> =
                                        match format {
                                            CaptureFormat::Png => image
                                                .save(&dest)
                                                .map_err(|e| format!("PNG 保存失敗: {e}")),
                                            CaptureFormat::Pdf => {
                                                win_platform::encode_rgba_as_raster_pdf(
                                                    image.width(),
                                                    image.height(),
                                                    image.as_raw(),
                                                )
                                                .and_then(|pdf| {
                                                    std::fs::write(&dest, &pdf)
                                                        .map_err(|e| format!("PDF 保存失敗: {e}"))
                                                })
                                            }
                                        };
                                    let (event_json, reply_result) = match save_res {
                                        Ok(()) => {
                                            let name = dest
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or(fallback_name);
                                            let ev = format!(
                                                "{{\"ok\":true,\"name\":{}}}",
                                                win_platform::json_string(&name)
                                            );
                                            (ev, Ok(name))
                                        }
                                        Err(err) => {
                                            let ev = format!(
                                                "{{\"ok\":false,\"error\":{}}}",
                                                win_platform::json_string(&err)
                                            );
                                            (ev, Err(err))
                                        }
                                    };
                                    let _ = proxy_worker.send_event(HostEvent::CaptureFinished {
                                        target_window,
                                        call_id,
                                        event_json,
                                        reply_result,
                                    });
                                });
                            }
                            Err(err) => {
                                let msg = format!("キャプチャ失敗: {err:?}");
                                let event_json = format!(
                                    "{{\"ok\":false,\"error\":{}}}",
                                    win_platform::json_string(&msg)
                                );
                                let _ = proxy.send_event(HostEvent::CaptureFinished {
                                    target_window,
                                    call_id,
                                    event_json,
                                    reply_result: Err(msg),
                                });
                            }
                        });
                        ran_capture = true;
                    } else {
                        let _ = delegate.waker.0.send_event(HostEvent::CaptureFinished {
                            target_window,
                            call_id: pending.call_id,
                            event_json: "{\"ok\":false,\"error\":\"cancelled\"}".to_owned(),
                            reply_result: Err("cancelled".to_owned()),
                        });
                    }
                }
            }
            if ran_capture {
                state.servo.spin_event_loop();
            }

            let mut ran_session_op = false;
            for (delegate, _) in &targets {
                let target_window = delegate.window.id();
                while let Some(op) = delegate.pending_session_ops.borrow_mut().pop_front() {
                    ran_session_op = true;
                    match op {
                        PendingSessionOp::ListSiteData { call_id } => {
                            let entries = state
                                .servo
                                .site_data_manager()
                                .site_data(StorageType::all());
                            let json = format_site_data_json(&entries);
                            *delegate.cached_site_data_json.borrow_mut() = json.clone();
                            if let Some(cid) = call_id {
                                send_async_reply(
                                    &delegate.waker,
                                    target_window,
                                    cid,
                                    true,
                                    json,
                                    true,
                                );
                            }
                        }
                        PendingSessionOp::ClearSiteData { call_id } => {
                            let entries = state
                                .servo
                                .site_data_manager()
                                .site_data(StorageType::all());
                            let names: Vec<String> = entries.iter().map(|e| e.name()).collect();
                            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                            state
                                .servo
                                .site_data_manager()
                                .clear_site_data(&name_refs, StorageType::all());
                            *delegate.cached_site_data_json.borrow_mut() = "[]".to_owned();
                            if let Some(cid) = call_id {
                                send_async_reply(
                                    &delegate.waker,
                                    target_window,
                                    cid,
                                    true,
                                    "cleared".to_owned(),
                                    false,
                                );
                            }
                        }
                        PendingSessionOp::ClearCookies {
                            session_only,
                            call_id,
                        } => {
                            if session_only {
                                state.servo.site_data_manager().clear_session_cookies(None);
                            } else {
                                state.servo.site_data_manager().clear_cookies(None);
                            }
                            if let Some(cid) = call_id {
                                send_async_reply(
                                    &delegate.waker,
                                    target_window,
                                    cid,
                                    true,
                                    "cleared".to_owned(),
                                    false,
                                );
                            }
                        }
                        PendingSessionOp::ClearCache { call_id } => {
                            state.servo.network_manager().clear_cache();
                            if let Some(cid) = call_id {
                                send_async_reply(
                                    &delegate.waker,
                                    target_window,
                                    cid,
                                    true,
                                    "cleared".to_owned(),
                                    false,
                                );
                            }
                        }
                    }
                }
            }
            if ran_session_op {
                state.servo.spin_event_loop();
            }
        }
        // Spawn any queued child windows (`Draconium.window.openChild` / `window.open`).
        if let Some(state) = &mut self.state {
            let mut spawned = false;
            loop {
                let next_req = state
                    .delegate
                    .pending_child_windows
                    .borrow_mut()
                    .pop_front();
                let Some(req) = next_req else {
                    break;
                };
                spawn_child_window(event_loop, state, req);
                spawned = true;
            }
            if spawned {
                let _ = state.rendering_context.make_current();
                state.servo.spin_event_loop();
            }
        }
        // Execute queued non-blocking async IPC tasks (`Draconium.invokeAsync` and `*Async`).
        if let Some(state) = &self.state {
            let mut targets: Vec<(Rc<Delegate>, WebView)> =
                Vec::with_capacity(1 + state.child_windows.len());
            targets.push((state.delegate.clone(), state.webview.clone()));
            for child in &state.child_windows {
                targets.push((child.delegate.clone(), child.webview.clone()));
            }
            let mut ran_async = false;
            for (delegate, webview) in &targets {
                while let Some(task) = delegate.pending_async_tasks.borrow_mut().pop_front() {
                    ran_async = true;
                    match task {
                        PendingAsyncTask::App {
                            call_id,
                            command,
                            payload,
                        } => {
                            let res = delegate
                                .handler
                                .borrow_mut()
                                .handle_request(&command, &payload);
                            let (ok, out) = match res {
                                Ok(s) => (true, s),
                                Err(e) => (false, e),
                            };
                            let script = bridge::resolve_async_script(call_id, ok, &out, false);
                            webview.evaluate_javascript(script, |_| {});
                        }
                        PendingAsyncTask::Window {
                            call_id,
                            command,
                            payload,
                            parse_json,
                        } => {
                            let res = delegate.handle_window_command(
                                command,
                                &payload,
                                Some(call_id),
                                webview,
                            );
                            let (ok, out) = match res {
                                Ok(s) => (true, s),
                                Err(e) => (false, e),
                            };
                            let script =
                                bridge::resolve_async_script(call_id, ok, &out, parse_json);
                            webview.evaluate_javascript(script, |_| {});
                        }
                    }
                }
            }
            if ran_async {
                state.servo.spin_event_loop();
            }
        }
        // Background push events (e.g. host file watchers or periodic feeds).
        if let Some(state) = &mut self.state
            && state.background_polling
            && state.delegate.load_complete.get()
            && !state.delegate.should_exit.get()
            && Instant::now() >= state.next_bg_poll
        {
            state.next_bg_poll = Instant::now() + BACKGROUND_POLL_INTERVAL;
            let pushed = state.delegate.handler.borrow_mut().poll_background_event();
            if let Some((event_name, json_payload)) = pushed {
                state.webview.evaluate_javascript(
                    bridge::dispatch_script(event_name, &json_payload),
                    |_| {},
                );
                state.servo.spin_event_loop();
            }
        }
        // A page-issued `dn-window:close` runs the same close flow as the
        // window's close button (including the before-close hook).
        if let Some(state) = &self.state
            && state.delegate.exit_requested.get()
            && !state.delegate.close_in_progress.get()
        {
            state.delegate.exit_requested.set(false);
            request_close(&state.webview, &state.delegate);
        }
        // A page-issued `dn-window:hard-reload` clears the network cache and runs the guarded reload flow.
        if let Some(state) = &self.state
            && state.delegate.hard_reload_requested.get()
        {
            state.delegate.hard_reload_requested.set(false);
            state.servo.network_manager().clear_cache();
            request_reload(&state.webview, &state.delegate);
        }
        // A page-issued `dn-window:reload` runs the guarded reload flow.
        if let Some(state) = &self.state
            && state.delegate.reload_requested.get()
        {
            state.delegate.reload_requested.set(false);
            request_reload(&state.webview, &state.delegate);
        }
        // Reap any child windows that requested close or received `notify_closed`.
        if let Some(state) = &mut self.state
            && !state.child_windows.is_empty()
        {
            state
                .child_windows
                .retain(|c| !c.delegate.should_exit.get() && !c.delegate.exit_requested.get());
        }
        // Save the window position once a move burst settles.
        if let Some(state) = &self.state {
            state.delegate.flush_pending_move(false);
        }
        // Drive Servo while waiting for the JS before-close hook and exit if it stalls.
        if let Some(state) = &self.state
            && state.delegate.close_in_progress.get()
        {
            state.servo.spin_event_loop();
            if !state.delegate.should_exit.get()
                && state
                    .delegate
                    .close_started_at
                    .get()
                    .is_some_and(|started| close_timed_out(started, Instant::now()))
            {
                state.delegate.should_exit.set(true);
                state.delegate.close_in_progress.set(false);
                state.delegate.close_started_at.set(None);
            }
        }
        if self
            .state
            .as_ref()
            .is_some_and(|state| state.delegate.should_exit.get())
        {
            if let (Some(runner), Some(state)) = (&mut self.self_test, &mut self.state)
                && runner.kind == SelfTestKind::Workspace
            {
                runner.advance(state);
            }
            event_loop.exit();
            return;
        }
        let animating = self.state.as_ref().is_some_and(|state| {
            state.delegate.animating.get()
                || state
                    .child_windows
                    .iter()
                    .any(|c| c.delegate.animating.get())
        });
        let mut deadline = if animating {
            Some(Instant::now() + Duration::from_millis(16))
        } else {
            None
        };
        if let Some(state) = &self.state
            && state.background_polling
            && state.delegate.load_complete.get()
        {
            deadline = Some(deadline.map_or(state.next_bg_poll, |d| d.min(state.next_bg_poll)));
        }
        // Wake up when a pending move position becomes due to save, even if
        // nothing else stirs the event loop after the drag ends.
        if let Some(state) = &self.state
            && let Some((_, moved_at)) = state.delegate.pending_move.get()
        {
            let due = moved_at + MOVE_SAVE_QUIET;
            deadline = Some(deadline.map_or(due, |d| d.min(due)));
        }
        if let Some(state) = &self.state
            && state.delegate.close_in_progress.get()
        {
            let close_wake = close_poll_deadline(Instant::now());
            deadline = Some(deadline.map_or(close_wake, |d| d.min(close_wake)));
        }
        if let Some(runner) = &self.self_test {
            let test_wake = runner
                .wait_until
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(25));
            deadline = Some(deadline.map_or(test_wake, |d| d.min(test_wake)));
        }
        if let Some(job) = &self.screenshot_job {
            let shot_wake = job
                .wait_until
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(25));
            deadline = Some(deadline.map_or(shot_wake, |d| d.min(shot_wake)));
        }
        event_loop.set_control_flow(match deadline {
            Some(instant) => ControlFlow::WaitUntil(instant),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, _: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else {
            return;
        };
        if window_id != state.delegate.window.id() {
            if let Some(idx) = state
                .child_windows
                .iter()
                .position(|c| c.window_id == window_id)
            {
                match event {
                    WindowEvent::CloseRequested => {
                        state.child_windows.remove(idx);
                    }
                    WindowEvent::RedrawRequested => {
                        let child = &state.child_windows[idx];
                        let _ = child.rendering_context.make_current();
                        child.webview.paint();
                        child.rendering_context.present();
                        child.delegate.has_painted.set(true);
                    }
                    WindowEvent::Resized(size) => {
                        let child = &state.child_windows[idx];
                        let _ = child.rendering_context.make_current();
                        child.webview.resize(size);
                        child.delegate.sync_fullscreen(&child.webview);
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let child = &state.child_windows[idx];
                        child
                            .webview
                            .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                        child.webview.resize(child.delegate.window.inner_size());
                    }
                    WindowEvent::Focused(focused) => {
                        let child = &mut state.child_windows[idx];
                        child.webview.set_focused(focused);
                        if !focused {
                            child.modifiers = ModifiersState::empty();
                            child.composing = false;
                            child.delegate.primary_down.set(false);
                        }
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        let child = &mut state.child_windows[idx];
                        child.mouse_position =
                            DevicePoint::new(position.x as f32, position.y as f32);
                        child.webview.notify_input_event(InputEvent::MouseMove(
                            MouseMoveEvent::new(child.mouse_position.into()),
                        ));
                    }
                    WindowEvent::CursorLeft { .. } => {
                        let child = &state.child_windows[idx];
                        child
                            .webview
                            .notify_input_event(InputEvent::MouseLeftViewport(
                                MouseLeftViewportEvent::default(),
                            ));
                    }
                    WindowEvent::MouseInput {
                        state: key, button, ..
                    } => {
                        let child = &state.child_windows[idx];
                        if button == winit::event::MouseButton::Left {
                            child
                                .delegate
                                .primary_down
                                .set(key == ElementState::Pressed);
                        }
                        let action = match key {
                            ElementState::Pressed => MouseButtonAction::Down,
                            ElementState::Released => MouseButtonAction::Up,
                        };
                        child.webview.notify_input_event(InputEvent::MouseButton(
                            MouseButtonEvent::new(
                                action,
                                input::mouse_button(button),
                                child.mouse_position.into(),
                            ),
                        ));
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let child = &state.child_windows[idx];
                        let (x, y, mode) = match delta {
                            MouseScrollDelta::LineDelta(x, y) => {
                                (f64::from(x), f64::from(y), WheelMode::DeltaLine)
                            }
                            MouseScrollDelta::PixelDelta(delta) => {
                                (delta.x, delta.y, WheelMode::DeltaPixel)
                            }
                        };
                        child
                            .webview
                            .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                                WheelDelta { x, y, z: 0.0, mode },
                                child.mouse_position.into(),
                            )));
                    }
                    WindowEvent::ModifiersChanged(modifiers) => {
                        state.child_windows[idx].modifiers = modifiers.state();
                    }
                    WindowEvent::KeyboardInput {
                        event,
                        is_synthetic,
                        ..
                    } => {
                        let child = &state.child_windows[idx];
                        if child.delegate.handle_fullscreen_shortcut(
                            &child.webview,
                            &event,
                            child.modifiers,
                            child.composing,
                            is_synthetic,
                        ) {
                            return;
                        }
                        child.webview.notify_input_event(InputEvent::Keyboard(
                            input::keyboard_event(&event, child.modifiers, child.composing),
                        ));
                    }
                    WindowEvent::Ime(ime) => {
                        let child = &mut state.child_windows[idx];
                        let composition = match ime {
                            Ime::Enabled => {
                                child.composing = true;
                                Some((CompositionState::Start, String::new()))
                            }
                            Ime::Preedit(text, _) => {
                                child.composing = true;
                                Some((CompositionState::Update, text))
                            }
                            Ime::Commit(text) => {
                                child.composing = false;
                                child.webview.notify_input_event(InputEvent::Ime(
                                    ImeEvent::Composition(CompositionEvent {
                                        state: CompositionState::Update,
                                        data: text.clone(),
                                    }),
                                ));
                                Some((CompositionState::End, text))
                            }
                            Ime::Disabled => {
                                child.composing = false;
                                None
                            }
                        };
                        if let Some((cstate, data)) = composition {
                            child.webview.notify_input_event(InputEvent::Ime(
                                ImeEvent::Composition(CompositionEvent {
                                    state: cstate,
                                    data,
                                }),
                            ));
                        }
                    }
                    _ => {}
                }
                state.servo.spin_event_loop();
            }
            return;
        }
        let webview = &state.webview;
        match event {
            WindowEvent::CloseRequested => {
                if state.delegate.close_to_tray {
                    state.delegate.set_window_visible(false);
                } else {
                    request_close(webview, &state.delegate);
                }
            }
            WindowEvent::RedrawRequested => {
                let _ = state.rendering_context.make_current();
                webview.paint();
                state.rendering_context.present();
                state.delegate.has_painted.set(true);
            }
            WindowEvent::Resized(size) => {
                let _ = state.rendering_context.make_current();
                webview.resize(size);
                state.delegate.sync_fullscreen(webview);
            }
            WindowEvent::Moved(position) => {
                // Position persistence (Milestone A): ignore OS minimize
                // sentinel coordinates (e.g. Windows `-32000, -32000`),
                // off-screen moves, and programmatic placement bursts.
                if state.delegate.window.is_minimized() == Some(true) {
                    return;
                }
                if state
                    .delegate
                    .ignore_move_until
                    .get()
                    .is_some_and(|until| Instant::now() < until)
                {
                    return;
                }
                let size = state.delegate.window.outer_size();
                if !physical_rect_visible(
                    &state.delegate.window,
                    position.x,
                    position.y,
                    size.width,
                    size.height,
                ) {
                    return;
                }
                let logical = window_state::physical_to_logical_position(
                    position.x,
                    position.y,
                    state.delegate.window.scale_factor(),
                );
                state
                    .delegate
                    .pending_move
                    .set(Some((logical, Instant::now())));
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                webview.set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                webview.resize(state.delegate.window.inner_size());
            }
            WindowEvent::Focused(focused) => {
                webview.set_focused(focused);
                if !focused {
                    state.modifiers = ModifiersState::empty();
                    state.composing = false;
                    state.delegate.primary_down.set(false);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.mouse_position = DevicePoint::new(position.x as f32, position.y as f32);
                webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                    state.mouse_position.into(),
                )));
            }
            WindowEvent::CursorLeft { .. } => {
                webview.notify_input_event(InputEvent::MouseLeftViewport(
                    MouseLeftViewportEvent::default(),
                ));
            }
            WindowEvent::MouseInput {
                state: key, button, ..
            } => {
                if button == winit::event::MouseButton::Left {
                    state
                        .delegate
                        .primary_down
                        .set(key == ElementState::Pressed);
                }
                let action = match key {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    input::mouse_button(button),
                    state.mouse_position.into(),
                )));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Keep discrete line input separate from a trackpad's pixel input.
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(x), f64::from(y), WheelMode::DeltaLine)
                    }
                    MouseScrollDelta::PixelDelta(delta) => {
                        (delta.x, delta.y, WheelMode::DeltaPixel)
                    }
                };
                webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                    WheelDelta { x, y, z: 0.0, mode },
                    state.mouse_position.into(),
                )));
            }
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if state.delegate.handle_fullscreen_shortcut(
                    webview,
                    &event,
                    state.modifiers,
                    state.composing,
                    is_synthetic,
                ) {
                    return;
                }
                // Host shortcuts: F5 / Ctrl+R reload through the guarded flow
                // (when enabled); Ctrl+Shift+R / Ctrl+F5 clear cache and reload;
                // Ctrl+= / Ctrl+- / Ctrl+0 adjust page zoom.
                // Ignored during IME composition or when Alt/Super are held.
                let reload_shortcut = classify_reload_shortcut(
                    state.delegate.allow_reload_shortcuts,
                    state.composing,
                    state.modifiers,
                    &event.logical_key,
                );
                let no_alt_super = !state.modifiers.alt_key() && !state.modifiers.super_key();
                let zoom_action = if !state.composing
                    && no_alt_super
                    && state.modifiers.control_key()
                {
                    match &event.logical_key {
                        WinitKey::Character(text) if matches!(text.as_str(), "+" | "=") => Some(1),
                        WinitKey::Character(text) if text.as_str() == "-" => Some(-1),
                        WinitKey::Character(text) if text.as_str() == "0" => Some(0),
                        _ => None,
                    }
                } else {
                    None
                };
                match reload_shortcut {
                    ReloadShortcut::HardReload => {
                        if event.state == ElementState::Pressed && !event.repeat {
                            state.servo.network_manager().clear_cache();
                            request_reload(webview, &state.delegate);
                        }
                    }
                    ReloadShortcut::Reload => {
                        if event.state == ElementState::Pressed && !event.repeat {
                            request_reload(webview, &state.delegate);
                        }
                    }
                    ReloadShortcut::None => {
                        if let Some(delta) = zoom_action {
                            if event.state == ElementState::Pressed {
                                let next = if delta == 0 {
                                    1.0
                                } else {
                                    window_state::step_zoom(webview.page_zoom(), delta)
                                };
                                webview.set_page_zoom(next);
                            }
                        } else {
                            webview.notify_input_event(InputEvent::Keyboard(
                                input::keyboard_event(&event, state.modifiers, state.composing),
                            ));
                        }
                    }
                }
            }
            WindowEvent::Ime(ime) => {
                let composition = match ime {
                    Ime::Enabled => {
                        state.composing = true;
                        Some((CompositionState::Start, String::new()))
                    }
                    Ime::Preedit(text, _) => {
                        state.composing = true;
                        Some((CompositionState::Update, text))
                    }
                    Ime::Commit(text) => {
                        state.composing = false;
                        webview.notify_input_event(InputEvent::Ime(ImeEvent::Composition(
                            CompositionEvent {
                                state: CompositionState::Update,
                                data: text.clone(),
                            },
                        )));
                        Some((CompositionState::End, text))
                    }
                    Ime::Disabled => {
                        state.composing = false;
                        // On Windows this also follows a normal composition commit.
                        // It must not dismiss the editor or clear its focus.
                        None
                    }
                };
                if let Some((state, data)) = composition {
                    webview.notify_input_event(InputEvent::Ime(ImeEvent::Composition(
                        CompositionEvent { state, data },
                    )));
                }
            }
            WindowEvent::HoveredFile(_) => {
                if state.delegate.capabilities.allow_file_drop && !state.hovering_files {
                    state.hovering_files = true;
                    if state.delegate.load_complete.get() {
                        webview.evaluate_javascript(
                            bridge::dispatch_script("file-drop-hover", "{}"),
                            |_| {},
                        );
                    }
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if state.hovering_files {
                    state.hovering_files = false;
                    if state.delegate.load_complete.get() {
                        webview.evaluate_javascript(
                            bridge::dispatch_script("file-drop-cancel", "{}"),
                            |_| {},
                        );
                    }
                }
            }
            WindowEvent::DroppedFile(path) => {
                state.hovering_files = false;
                if state.delegate.capabilities.allow_file_drop {
                    state.pending_dropped_files.push(path);
                }
            }
            _ => {}
        }
        state.servo.spin_event_loop();
    }
}

#[derive(Clone)]
struct Waker(EventLoopProxy<HostEvent>);

struct HostServoDelegate {
    devtools_enabled: bool,
}

impl ServoDelegate for HostServoDelegate {
    fn notify_devtools_server_started(&self, port: u16, _token: String) {
        eprintln!("[devtools] Firefox RDP server ready on 127.0.0.1:{port}");
    }

    fn request_devtools_connection(&self, request: AllowOrDenyRequest) {
        if self.devtools_enabled {
            request.allow();
        } else {
            request.deny();
        }
    }

    fn notify_error(&self, error: ServoError) {
        eprintln!("[servo error] {error:?}");
    }
}

#[derive(Debug)]
enum HostEvent {
    Wake,
    Tray(TrayAction),
    AsyncIpcReply {
        target_window: WindowId,
        call_id: u64,
        ok: bool,
        payload: String,
        is_json: bool,
    },
    CaptureFinished {
        target_window: WindowId,
        call_id: Option<u64>,
        event_json: String,
        reply_result: Result<String, String>,
    },
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.0.send_event(HostEvent::Wake);
    }
}

/// Automated screenshot capture job (`--screenshot <path>`, Milestone C).
struct ScreenshotJob {
    path: PathBuf,
    started_at: Instant,
    wait_until: Option<Instant>,
    in_flight: bool,
    result: Rc<RefCell<Option<Result<String, String>>>>,
}

impl ScreenshotJob {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            started_at: Instant::now(),
            wait_until: None,
            in_flight: false,
            result: Rc::new(RefCell::new(None)),
        }
    }

    fn advance(&mut self, state: &State) {
        let now = Instant::now();
        if now.duration_since(self.started_at) > SCREENSHOT_TIMEOUT {
            eprintln!("SCREENSHOT_FAILED: timed out waiting for frame");
            std::process::exit(1);
        }
        if self.in_flight {
            if let Some(outcome) = self.result.borrow_mut().take() {
                match outcome {
                    Ok(summary) => {
                        println!("{summary}");
                        state.delegate.should_exit.set(true);
                    }
                    Err(err) => {
                        eprintln!("SCREENSHOT_FAILED: {err}");
                        std::process::exit(1);
                    }
                }
            }
            return;
        }
        if !state.delegate.load_complete.get() || !state.delegate.has_painted.get() {
            return;
        }
        match self.wait_until {
            None => {
                // Allow one brief settle window after initial paint so JS
                // startup state (e.g. initial IPC hydration) renders.
                self.wait_until = Some(now + Duration::from_millis(250));
                return;
            }
            Some(deadline) if now < deadline => return,
            Some(_) => {
                self.wait_until = None;
            }
        }
        self.in_flight = true;
        let path = self.path.clone();
        let slot = self.result.clone();
        let waker = state.delegate.waker.clone();
        state.webview.take_screenshot(None, move |capture| {
            let outcome = match capture {
                Ok(image) => {
                    let report =
                        embedder_ui::analyze_rgba_image(image.width(), image.height(), image.as_raw());
                    if let Some(parent) = path.parent()
                        && !parent.as_os_str().is_empty()
                    {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match image.save(&path) {
                        Ok(()) => Ok(format!(
                            "SCREENSHOT_OK width={} height={} sampled_pixels={} distinct_colors={} non_blank={} path={}",
                            report.width,
                            report.height,
                            report.sampled_pixels,
                            report.distinct_colors,
                            report.non_blank,
                            path.display()
                        )),
                        Err(err) => Err(format!("PNG save failed ({}): {err}", path.display())),
                    }
                }
                Err(err) => Err(format!("capture failed: {err:?}")),
            };
            *slot.borrow_mut() = Some(outcome);
            waker.wake();
        });
        state.servo.spin_event_loop();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelfTestKind {
    Scroll,
    Workspace,
    WorkspaceReload,
    Starter,
}

struct SelfTestRunner {
    kind: SelfTestKind,
    phase: usize,
    started_at: Instant,
    wait_until: Option<Instant>,
    eval_pending: bool,
    eval_result: Rc<RefCell<Option<Result<JSValue, String>>>>,
    scroll_samples: Vec<f64>,
    bench_summary: String,
}

impl SelfTestRunner {
    fn new(name: &str) -> Self {
        let kind = match name {
            "scroll" => SelfTestKind::Scroll,
            "workspace" => SelfTestKind::Workspace,
            "workspace-reload" => SelfTestKind::WorkspaceReload,
            "starter" => SelfTestKind::Starter,
            other => {
                eprintln!("Unknown --self-test mode: {other}");
                std::process::exit(2);
            }
        };
        Self {
            kind,
            phase: 0,
            started_at: Instant::now(),
            wait_until: None,
            eval_pending: false,
            eval_result: Rc::new(RefCell::new(None)),
            scroll_samples: Vec::new(),
            bench_summary: String::new(),
        }
    }

    fn fail(&self, message: &str) -> ! {
        eprintln!("SELF_TEST_FAILED: {message}");
        std::process::exit(1);
    }

    fn start_eval(&mut self, state: &State, script: &str) {
        self.eval_pending = true;
        *self.eval_result.borrow_mut() = None;
        let slot = self.eval_result.clone();
        let waker = state.delegate.waker.clone();
        state.webview.evaluate_javascript(script, move |res| {
            *slot.borrow_mut() = Some(res.map_err(|err| format!("{err:?}")));
            waker.wake();
        });
        state.servo.spin_event_loop();
    }

    fn take_eval(&mut self) -> Option<JSValue> {
        if !self.eval_pending {
            return None;
        }
        let result = self.eval_result.borrow_mut().take()?;
        self.eval_pending = false;
        match result {
            Ok(value) => Some(value),
            Err(err) => self.fail(&format!("JS evaluation failed: {err}")),
        }
    }

    fn send_wheel(&self, state: &mut State, x: f64, y: f64) {
        let point = DevicePoint::new(300.0, 300.0);
        state.mouse_position = point;
        state
            .webview
            .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
        state
            .webview
            .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                WheelDelta {
                    x,
                    y,
                    z: 0.0,
                    mode: WheelMode::DeltaLine,
                },
                point.into(),
            )));
        state.servo.spin_event_loop();
    }

    fn advance(&mut self, state: &mut State) {
        let now = Instant::now();
        if now.duration_since(self.started_at) > SELF_TEST_TIMEOUT {
            self.fail(&format!("timed out at phase {}", self.phase));
        }
        if self.phase == 0 {
            if state.delegate.load_complete.get() && state.delegate.has_painted.get() {
                self.wait_until = Some(now + Duration::from_millis(200));
                self.phase = 1;
            }
            return;
        }
        if let Some(deadline) = self.wait_until {
            let wake_for_exit =
                self.kind == SelfTestKind::Workspace && state.delegate.should_exit.get();
            if now < deadline && !wake_for_exit {
                return;
            }
            self.wait_until = None;
        }
        match self.kind {
            SelfTestKind::Scroll => self.advance_scroll(state, now),
            SelfTestKind::Workspace => self.advance_workspace(state, now),
            SelfTestKind::WorkspaceReload => self.advance_workspace_reload(state),
            SelfTestKind::Starter => self.advance_starter(state, now),
        }
    }

    fn advance_scroll(&mut self, state: &mut State, now: Instant) {
        match self.phase {
            1 => {
                self.send_wheel(state, 0.0, -1.0);
                self.wait_until = Some(now + Duration::from_millis(600));
                self.phase = 2;
            }
            2 => {
                self.start_eval(state, "window.scrollY");
                self.phase = 3;
            }
            3 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::Number(y1) = value else {
                    self.fail(&format!("unexpected scrollY type: {value:?}"));
                };
                self.scroll_samples.push(y1);
                self.send_wheel(state, 0.0, -1.0);
                self.wait_until = Some(now + Duration::from_millis(20));
                self.phase = 4;
            }
            4 => {
                self.send_wheel(state, 0.0, -1.0);
                self.wait_until = Some(now + Duration::from_millis(600));
                self.phase = 5;
            }
            5 => {
                self.start_eval(state, "window.scrollY");
                self.phase = 6;
            }
            6 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::Number(y2) = value else {
                    self.fail(&format!("unexpected scrollY type: {value:?}"));
                };
                self.scroll_samples.push(y2);
                self.send_wheel(state, 0.0, 1.0);
                self.wait_until = Some(now + Duration::from_millis(600));
                self.phase = 7;
            }
            7 => {
                self.start_eval(state, "window.scrollY");
                self.phase = 8;
            }
            8 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::Number(y3) = value else {
                    self.fail(&format!("unexpected scrollY type: {value:?}"));
                };
                self.scroll_samples.push(y3);
                let [y1, y2, y3] = [
                    self.scroll_samples[0],
                    self.scroll_samples[1],
                    self.scroll_samples[2],
                ];
                if (y1 - 40.0).abs() < 0.01 && (y2 - 120.0).abs() < 0.01 && (y3 - 80.0).abs() < 0.01
                {
                    println!("SCROLL_SELF_TEST_OK y1={y1} y2={y2} y3={y3}");
                    state.delegate.should_exit.set(true);
                } else {
                    self.fail(&format!("scroll mismatch y1={y1} y2={y2} y3={y3}"));
                }
            }
            _ => {}
        }
    }

    fn advance_workspace(&mut self, state: &mut State, now: Instant) {
        match self.phase {
            1 => {
                self.start_eval(
                    state,
                    "(() => { \
                        if (!window.Draconium || !Object.isFrozen(window.Draconium) || !Object.isFrozen(window.Draconium.clipboard) || !Object.isFrozen(window.Draconium.session) || !Object.isFrozen(window.Draconium.autostart)) { \
                            return 'BAD:preload bridge or namespaces not frozen'; \
                        } \
                        const deniedCmd = window.Draconium.invoke('echo', 'forbidden'); \
                        if (deniedCmd.ok) { return 'BAD:unauthorized app command allowed'; } \
                        const deniedProto = window.Draconium.protocolClient.register('dn-folio'); \
                        if (deniedProto.ok) { return 'BAD:protocolClient allowed when disabled'; } \
                        const forged = window.prompt('dn-window:close', ''); \
                        if (!String(forged).startsWith('err:')) { return 'BAD:forged prompt allowed'; } \
                        const forgedFile = window.Draconium.files.readText('dnfile-forged'); \
                        if (forgedFile.ok) { return 'BAD:forged file token allowed'; } \
                        const badExt = window.Draconium.window.openExternal('file:///C:/Windows/cmd.exe'); \
                        if (badExt.ok) { return 'BAD:file external URL allowed'; } \
                        const okExt = window.Draconium.window.openExternal('https://example.com'); \
                        if (!okExt.ok) { return 'BAD:https external URL denied: ' + okExt.error; } \
                        const hideRes = window.Draconium.window.hide(); \
                        if (hideRes.ok) { return 'BAD:hide succeeded without tray'; } \
                        const themeRes = window.Draconium.window.getTheme(); \
                        if (!themeRes.ok || (themeRes.data !== 'dark' && themeRes.data !== 'light')) { return 'BAD:getTheme failed'; } \
                        const platRes = window.Draconium.window.getPlatform(); \
                        if (!platRes.ok || !platRes.data || typeof platRes.data.os !== 'string' || typeof platRes.data.nativeDialogs !== 'boolean' || typeof platRes.data.mediaGstreamer !== 'boolean') { return 'BAD:getPlatform failed'; } \
                        const maxRes = window.Draconium.window.isMaximized(); \
                        if (!maxRes.ok) { return 'BAD:isMaximized failed'; } \
                        const titleRes = window.Draconium.window.setTitle('Draconium Folio Self-Test'); \
                        if (!titleRes.ok) { return 'BAD:setTitle failed'; } \
                        const boundsRes = window.Draconium.window.getBounds(); \
                        if (!boundsRes.ok || typeof boundsRes.data.width !== 'number' || boundsRes.data.width <= 0 || typeof boundsRes.data.fullscreen !== 'boolean') { return 'BAD:getBounds failed'; } \
                        const fullscreenRes = window.Draconium.window.isFullscreen(); \
                        if (!fullscreenRes.ok || (fullscreenRes.data !== 'true' && fullscreenRes.data !== 'false')) { return 'BAD:isFullscreen failed'; } \
                        const canBackRes = window.Draconium.window.canGoBack(); \
                        const canForwardRes = window.Draconium.window.canGoForward(); \
                        if (!canBackRes.ok || !canForwardRes.ok) { return 'BAD:history state failed'; } \
                        const clipWrite = window.Draconium.clipboard.writeText('folio-self-test'); \
                        if (!clipWrite.ok) { return 'BAD:clipboard.writeText failed: ' + clipWrite.error; } \
                        const autoGet = window.Draconium.autostart.isEnabled(); \
                        if (!autoGet.ok || autoGet.data !== false) { return 'BAD:autostart.isEnabled failed'; } \
                        const pdfDry = window.Draconium.window.exportPdf('folio.pdf'); \
                        if (!pdfDry.ok) { return 'BAD:exportPdf dry-run failed: ' + pdfDry.error; } \
                        const zIn = window.Draconium.window.zoomIn(); \
                        const zReset = window.Draconium.window.zoomReset(); \
                        if (!zIn.ok || !zReset.ok) { return 'BAD:zoom failed'; } \
                        const pop = document.getElementById('export-popover'); \
                        if (!pop) { return 'BAD:export-popover missing'; } \
                        pop.showPopover(); \
                        const popOpened = pop.classList.contains('dn-popover-open') || pop.matches(':popover-open'); \
                        pop.hidePopover(); \
                        if (!popOpened) { return 'BAD:popover shim failed'; } \
                        const noteItems = document.querySelectorAll('#note-list .note-item').length; \
                        if (noteItems < 3) { return 'BAD:expected 3 seeded notes, got ' + noteItems; } \
                        const area = document.getElementById('editor-textarea'); \
                        const title = document.getElementById('input-title'); \
                        title.value = 'Folio Verification'; \
                        title.dispatchEvent(new Event('input', { bubbles: true })); \
                        area.value = '# 日本語テスト\\n\\n2行目😀'; \
                        area.dispatchEvent(new Event('input', { bubbles: true })); \
                        const previewH1 = document.querySelector('#preview-prose h1'); \
                        if (!previewH1 || previewH1.textContent !== '日本語テスト') { \
                            return 'BAD:markdown preview did not update'; \
                        } \
                        window.__draconiumAsyncList = null; \
                        window.Draconium.invokeAsync('list_notes', '{}').then(r => { window.__draconiumAsyncList = r; }); \
                        window.__folioBenchResult = null; \
                        if (typeof window.__folioRunBenchmark !== 'function') { return 'BAD:benchmark harness missing'; } \
                        window.__folioRunBenchmark(12).then(r => { window.__folioBenchResult = r; }); \
                        return 'STEP1:notes=' + noteItems; \
                    })()",
                );
                self.phase = 2;
            }
            2 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::String(summary) = value else {
                    self.fail(&format!("unexpected workspace phase 1: {value:?}"));
                };
                if !summary.starts_with("STEP1:") {
                    self.fail(&format!("workspace phase 1 failed: {summary}"));
                }
                self.wait_until = Some(now + Duration::from_millis(320));
                self.phase = 3;
            }
            3 => {
                self.start_eval(
                    state,
                    "(() => { \
                        if (!window.__folioBenchResult || !window.__draconiumAsyncList) { return 'PENDING'; } \
                        if (!window.__draconiumAsyncList.ok) { return 'BAD:invokeAsync failed'; } \
                        const r = window.__folioBenchResult; \
                        if (r.frames !== 12 || r.domNodes < 40) { return 'BAD_BENCH:' + JSON.stringify(r); } \
                        return 'OK:fps=' + r.fps + '|avg_ms=' + r.avgFrameMs + '|p95_ms=' + r.p95FrameMs + '|dom_nodes=' + r.domNodes; \
                    })()",
                );
                self.phase = 4;
            }
            4 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::String(summary) = value else {
                    self.fail(&format!("unexpected workspace phase 4: {value:?}"));
                };
                if summary == "PENDING" {
                    self.wait_until = Some(now + Duration::from_millis(120));
                    self.phase = 3;
                    return;
                }
                let Some(rest) = summary.strip_prefix("OK:") else {
                    self.fail(&format!("workspace benchmark failed: {summary}"));
                };
                self.bench_summary = rest.to_owned();
                request_close(&state.webview, &state.delegate);
                state.servo.spin_event_loop();
                self.phase = 5;
            }
            5 => {
                if state.delegate.should_exit.get() {
                    println!("WORKSPACE_SELF_TEST_OK {}", self.bench_summary);
                    self.phase = 99;
                } else if !state.delegate.close_in_progress.get() {
                    self.fail("close was prevented during workspace flush");
                } else {
                    self.wait_until = Some(now + Duration::from_millis(20));
                }
            }
            _ => {}
        }
    }

    fn advance_workspace_reload(&mut self, state: &mut State) {
        match self.phase {
            1 => {
                self.start_eval(
                    state,
                    "(() => { \
                        const t = document.getElementById('input-title').value; \
                        const v = document.getElementById('editor-textarea').value; \
                        return (t === 'Folio Verification' && v.includes('日本語テスト') && v.includes('2行目😀')) \
                            ? 'OK:' + t \
                            : 'BAD:' + JSON.stringify({ t, v }); \
                    })()",
                );
                self.phase = 2;
            }
            2 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::String(res) = value else {
                    self.fail(&format!("unexpected workspace-reload value: {value:?}"));
                };
                if let Some(rest) = res.strip_prefix("OK:") {
                    println!("WORKSPACE_RELOAD_SELF_TEST_OK {rest}");
                    self.phase = 99;
                    state.delegate.should_exit.set(true);
                } else {
                    self.fail(&format!("workspace-reload mismatch: {res}"));
                }
            }
            _ => {}
        }
    }

    fn advance_starter(&mut self, state: &mut State, now: Instant) {
        match self.phase {
            1 => {
                self.start_eval(
                    state,
                    "(() => { \
                        const input = document.getElementById('starter-input'); \
                        input.value = 'Hello 日本😀'; \
                        document.getElementById('btn-echo-sync').click(); \
                        const syncReply = document.getElementById('starter-reply').textContent; \
                        const denied = window.Draconium.invoke('save_note', '{}'); \
                        if (denied.ok) { return 'BAD:unauthorized command allowed'; } \
                        window.__starterAsyncDone = false; \
                        document.getElementById('btn-echo-async').click(); \
                        setTimeout(() => { window.__starterAsyncDone = true; }, 160); \
                        return syncReply.includes('Response #1 (9 chars)') ? 'STEP1:' + syncReply : 'BAD:' + syncReply; \
                    })()",
                );
                self.phase = 2;
            }
            2 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::String(summary) = value else {
                    self.fail(&format!("unexpected starter phase 1: {value:?}"));
                };
                if !summary.starts_with("STEP1:") {
                    self.fail(&format!("starter sync failed: {summary}"));
                }
                self.wait_until = Some(now + Duration::from_millis(200));
                self.phase = 3;
            }
            3 => {
                self.start_eval(
                    state,
                    "document.getElementById('starter-reply').textContent",
                );
                self.phase = 4;
            }
            4 => {
                let Some(value) = self.take_eval() else {
                    return;
                };
                let JSValue::String(reply) = value else {
                    self.fail(&format!("unexpected starter phase 4: {value:?}"));
                };
                if reply.contains("Response #2 (9 chars)") {
                    println!("STARTER_SELF_TEST_OK {reply}");
                    self.phase = 99;
                    state.delegate.should_exit.set(true);
                } else {
                    self.fail(&format!("starter async mismatch: {reply}"));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_config_reload_shortcuts_defaults_true_and_can_disable() {
        let url = Url::parse("dn://app/index.html").expect("valid URL");
        let default_cfg = HostConfig::new("App", url.clone());
        assert!(default_cfg.allow_reload_shortcuts);

        let disabled_cfg = HostConfig::new("App", url).with_reload_shortcuts(false);
        assert!(!disabled_cfg.allow_reload_shortcuts);
        assert!(
            disabled_cfg
                .with_reload_shortcuts(true)
                .allow_reload_shortcuts
        );
    }

    #[test]
    fn reload_shortcut_classification_respects_flag_modifiers_and_ime() {
        let f5 = WinitKey::Named(WinitNamedKey::F5);
        let r = WinitKey::Character("r".into());
        let upper_r = WinitKey::Character("R".into());
        let f2 = WinitKey::Named(WinitNamedKey::F2);

        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::empty(), &f5),
            ReloadShortcut::Reload
        );
        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::CONTROL, &r),
            ReloadShortcut::Reload
        );
        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::CONTROL, &f5),
            ReloadShortcut::HardReload
        );
        assert_eq!(
            classify_reload_shortcut(
                true,
                false,
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                &upper_r,
            ),
            ReloadShortcut::HardReload
        );
        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::empty(), &f2),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(true, true, ModifiersState::empty(), &f5),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::ALT, &f5),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(true, false, ModifiersState::SUPER, &r),
            ReloadShortcut::None
        );

        assert_eq!(
            classify_reload_shortcut(false, false, ModifiersState::empty(), &f5),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(false, false, ModifiersState::CONTROL, &r),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(false, false, ModifiersState::CONTROL, &f5),
            ReloadShortcut::None
        );
        assert_eq!(
            classify_reload_shortcut(
                false,
                false,
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                &r,
            ),
            ReloadShortcut::None
        );
    }

    #[test]
    fn close_timeout_and_poll_deadline_behave_as_configured() {
        let start = Instant::now();
        assert!(!close_timed_out(start, start));
        assert!(!close_timed_out(start, start + Duration::from_millis(500)));
        assert!(close_timed_out(start, start + Duration::from_millis(501)));
        assert_eq!(
            close_poll_deadline(start).duration_since(start),
            Duration::from_millis(25)
        );
    }
}

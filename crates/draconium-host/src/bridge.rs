//! The JS ↔ Rust bridge contract for the Draconium host.
//!
//! The host's synchronous channel from page JS to Rust is Servo's
//! `SimpleDialog::Prompt` embedder control: page code calls
//! `window.Draconium.*`, which delegates to a closure-captured native
//! `window.prompt` prefixed with a per-launch session token for privileged
//! host commands (`dn-tok:<nonce>:dn-window:<cmd>`) and correlated non-blocking
//! async commands (`dn-tok:<nonce>:dn-app-async:<id>:<cmd>` and
//! `dn-tok:<nonce>:dn-window-async:<id>:<cmd>`).
//!
//! Beyond the IPC bridge, [`PRELOAD_SCRIPT`] also injects lightweight shims
//! for web/Electron capabilities missing in Servo 0.8.0:
//! - HTML Popover API (`[popover]`, `showPopover`/`hidePopover`/`togglePopover`,
//!   `popovertarget`, light-dismiss), paired with [`POPOVER_POLYFILL_CSS`]
//!   injected at `Origin::User` via `UserStyleSheet` so strict `style-src 'self'`
//!   Content-Security-Policy is preserved
//! - Declarative `-webkit-app-region: drag` / `[data-dn-drag]` window drag
//! - `<a download>` interception for `data:` / `blob:` URLs → native Save dialog
//! - `window.print()` fallback → raster PDF export dialog
//! - Synthetic `dn:file-drop` DOM events paired with capability-scoped file tokens
//!
//! All items in this module are dependency-free pure Rust so they can be
//! unit-tested on any OS with `rustc --edition 2024 --test`.

/// Prefix marking a bridge command as a host window command rather than an
/// application command.
pub const WINDOW_COMMAND_PREFIX: &str = "dn-window:";
/// Prefix marking a correlated async host window command.
pub const WINDOW_ASYNC_PREFIX: &str = "dn-window-async:";
/// Prefix marking a correlated async application command.
pub const APP_ASYNC_PREFIX: &str = "dn-app-async:";
/// Prefix wrapping privileged host commands with the per-launch session token.
pub const TOKEN_COMMAND_PREFIX: &str = "dn-tok:";
/// Placeholder inside [`PRELOAD_SCRIPT`] replaced by [`render_preload_script`].
const TOKEN_PLACEHOLDER: &str = "__DN_SESSION_TOKEN__";

/// User-origin stylesheet registered via `UserContentManager::add_stylesheet`
/// to support the HTML Popover API polyfill without requiring `'unsafe-inline'`
/// in author `style-src` Content-Security-Policy.
pub const POPOVER_POLYFILL_CSS: &str = "[popover]:not(.dn-popover-open){display:none !important;}[popover].dn-popover-open{position:fixed;z-index:2147483647;}";

/// Validated loopback development server origin (`--dev-url http://127.0.0.1:5173`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevOrigin {
    pub host: String,
    pub port: u16,
    pub initial_url: String,
}

impl DevOrigin {
    /// Parse and validate a loopback HTTP development server URL.
    ///
    /// Only `http://127.0.0.1:<port>[/...]`, `http://localhost:<port>[/...]`,
    /// and `http://[::1]:<port>[/...]` with an explicit non-zero port are
    /// accepted. Remote hosts, `0.0.0.0`, userinfo (`@`), backslashes, and
    /// non-HTTP schemes are rejected.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed != raw
            || trimmed
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\' || ch == '@')
        {
            return Err("不正な --dev-url 指定です".to_owned());
        }
        let Some((scheme, rest)) = trimmed.split_once("://") else {
            return Err("--dev-url は http://<host>:<port> 形式で指定してください".to_owned());
        };
        if !scheme.eq_ignore_ascii_case("http") {
            return Err("--dev-url は http:// スキームのみ対応しています".to_owned());
        }
        let (authority, path_part) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains(['?', '#']) {
            return Err("--dev-url のホストまたはポートが不正です".to_owned());
        }
        let (host_raw, port_raw) = if let Some(after_bracket) = authority.strip_prefix("[::1]") {
            let Some(port_str) = after_bracket.strip_prefix(':') else {
                return Err("--dev-url には明示的なポート番号が必要です".to_owned());
            };
            ("[::1]", port_str)
        } else {
            let Some((h, p)) = authority.rsplit_once(':') else {
                return Err("--dev-url には明示的なポート番号が必要です".to_owned());
            };
            (h, p)
        };
        let host = if host_raw == "127.0.0.1" {
            "127.0.0.1".to_owned()
        } else if host_raw.eq_ignore_ascii_case("localhost") {
            "localhost".to_owned()
        } else if host_raw == "[::1]" {
            "[::1]".to_owned()
        } else {
            return Err(format!(
                "--dev-url はループバック (127.0.0.1 / localhost / [::1]) のみ許可されています: {host_raw}"
            ));
        };
        let port: u16 = port_raw
            .parse()
            .map_err(|_| format!("--dev-url のポート番号が不正です: {port_raw}"))?;
        if port == 0 {
            return Err("--dev-url のポート番号に 0 は指定できません".to_owned());
        }
        let initial_url = format!("http://{host}:{port}{path_part}");
        Ok(Self {
            host,
            port,
            initial_url,
        })
    }

    /// Return `true` if `url` is an `http://` URL targeting this exact
    /// loopback `(host, port)` origin.
    pub fn matches_url(&self, url: &str) -> bool {
        let trimmed = url.trim();
        if trimmed.is_empty()
            || trimmed != url
            || trimmed
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return false;
        }
        let Some((scheme, rest)) = trimmed.split_once("://") else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("http") {
            return false;
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
            return false;
        }
        let (host_raw, port_raw) = if let Some(after_bracket) = authority.strip_prefix("[::1]") {
            let Some(port_str) = after_bracket.strip_prefix(':') else {
                return false;
            };
            ("[::1]", port_str)
        } else {
            let Some((h, p)) = authority.rsplit_once(':') else {
                return false;
            };
            (h, p)
        };
        let Ok(port) = port_raw.parse::<u16>() else {
            return false;
        };
        port == self.port && host_raw.eq_ignore_ascii_case(&self.host)
    }
}

/// Window / platform operations the host performs on behalf of any page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    /// Start an OS window drag (`Window::drag_window`).
    Drag,
    /// Start an OS edge/corner window resize (`Window::drag_resize_window`).
    DragResize,
    /// Minimize the window (`Window::set_minimized(true)`).
    Minimize,
    /// Toggle maximized state (`Window::set_maximized(!is_maximized)`).
    ToggleMaximize,
    /// Query whether the window is currently maximized (`"true"` / `"false"`).
    IsMaximized,
    /// Set the native window title.
    SetTitle,
    /// Set or clear borderless fullscreen mode.
    SetFullscreen,
    /// Query whether the window is in fullscreen mode.
    IsFullscreen,
    /// Set whether the window stays above other windows.
    SetAlwaysOnTop,
    /// Focus the native window.
    Focus,
    /// Request or clear native user attention.
    RequestUserAttention,
    /// Read logical window bounds and state as JSON.
    GetBounds,
    /// Request a logical inner size.
    SetSize,
    /// Navigate backward in the WebView history.
    GoBack,
    /// Navigate forward in the WebView history.
    GoForward,
    /// Query whether backward history navigation is available.
    CanGoBack,
    /// Query whether forward history navigation is available.
    CanGoForward,
    /// Resize the window to the compact chips presentation.
    ShowChips,
    /// Resize the window to the expanded flyout presentation.
    ShowFlyout,
    /// Hide the window (`set_visible(false)`). Only allowed when a recovery
    /// mechanism such as the system tray is active.
    Hide,
    /// Show the window again (`set_visible(true)`).
    Show,
    /// Ask the host to run its normal close flow (including the page's
    /// `__draconiumBeforeClose` hook).
    Close,
    /// Reload the page (`WebView::reload`).
    Reload,
    /// Clear the network cache and reload the page (`NetworkManager::clear_cache` + reload).
    HardReload,
    /// Zoom the page in by one step (`WebView::set_page_zoom`).
    ZoomIn,
    /// Zoom the page out by one step (`WebView::set_page_zoom`).
    ZoomOut,
    /// Reset page zoom to 100% (`WebView::set_page_zoom(1.0)`).
    ZoomReset,
    /// Open an allowlisted external URL (`https:` / `mailto:`) in the OS
    /// default handler.
    OpenExternal,
    /// Set layered window opacity (`0.1..=1.0`, Windows `SetLayeredWindowAttributes`).
    SetOpacity,
    /// Toggle mouse click-through (`WS_EX_TRANSPARENT`, Windows).
    SetClickThrough,
    /// Prompt for a PNG path (`GetSaveFileNameW`) and save `WebView::take_screenshot`.
    CapturePng,
    /// Prompt for an HTML path (`GetSaveFileNameW`) and save a serialized HTML snapshot.
    ExportHtml,
    /// Prompt for a PDF path (`GetSaveFileNameW`) and save a single-page raster PDF snapshot.
    ExportPdf,
    /// Save a `<a download>` base64 payload via native `GetSaveFileNameW`.
    SaveDownload,
    /// Open native file picker and return capability-scoped file tokens.
    DialogOpenFile,
    /// Open native folder picker and return the selected directory path.
    DialogSelectFolder,
    /// Open native save dialog and write text or base64 content.
    DialogSaveFile,
    /// Read UTF-8 text from a capability-scoped file token (dropped or picked).
    FileReadText,
    /// Read base64 bytes from a capability-scoped file token (dropped or picked).
    FileReadBase64,
    /// Encrypt UTF-8 text with Windows DPAPI (`safeStorage.encrypt`).
    SafeStorageEncrypt,
    /// Decrypt base64 ciphertext with Windows DPAPI (`safeStorage.decrypt`).
    SafeStorageDecrypt,
    /// Display a desktop notification via the tray icon (`NIF_INFO`) or ephemeral balloon.
    Notify,
    /// Query current OS theme (`"dark"` or `"light"`).
    GetTheme,
    /// Query runtime OS identifier and native host capability matrix (JSON object).
    GetPlatform,
    /// Read plain UTF-8 text from the OS clipboard.
    ClipboardReadText,
    /// Write plain UTF-8 text to the OS clipboard.
    ClipboardWriteText,
    /// Read HTML fragment (`CF_HTML`) from the OS clipboard.
    ClipboardReadHtml,
    /// Write HTML fragment (`CF_HTML`) to the OS clipboard.
    ClipboardWriteHtml,
    /// Read image (`CF_DIB`) from the OS clipboard as a base64 PNG data URL / object.
    ClipboardReadImage,
    /// Write RGBA / BMP image data to the OS clipboard as `CF_DIB`.
    ClipboardWriteImage,
    /// Clear the OS clipboard.
    ClipboardClear,
    /// Query whether the app is registered in `HKCU\...\Run` autostart.
    AutostartGet,
    /// Enable or disable `HKCU\...\Run` autostart for the app.
    AutostartSet,
    /// Register a custom URI scheme (`HKCU\Software\Classes\<scheme>`) for deep links.
    ProtocolClientSet,
    /// Unregister a custom URI scheme from `HKCU\Software\Classes\<scheme>`.
    ProtocolClientRemove,
    /// List site data entries (`SiteDataManager::site_data`).
    SessionListSiteData,
    /// Clear site data (`SiteDataManager::clear_site_data`).
    SessionClearSiteData,
    /// Clear all or session-only cookies (`SiteDataManager::clear_cookies`).
    SessionClearCookies,
    /// Clear Servo's network cache (`NetworkManager::clear_cache`).
    SessionClearCache,
    /// Open a secondary child window (`WebViewBuilder::new`) with attenuated capabilities.
    OpenChildWindow,
}

impl WindowCommand {
    /// Canonical wire suffix name (without `dn-window:`), used for capability checks.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drag => "drag",
            Self::DragResize => "drag-resize",
            Self::Minimize => "minimize",
            Self::ToggleMaximize => "toggle-maximize",
            Self::IsMaximized => "is-maximized",
            Self::SetTitle => "set-title",
            Self::SetFullscreen => "set-fullscreen",
            Self::IsFullscreen => "is-fullscreen",
            Self::SetAlwaysOnTop => "set-always-on-top",
            Self::Focus => "focus",
            Self::RequestUserAttention => "request-user-attention",
            Self::GetBounds => "get-bounds",
            Self::SetSize => "set-size",
            Self::GoBack => "go-back",
            Self::GoForward => "go-forward",
            Self::CanGoBack => "can-go-back",
            Self::CanGoForward => "can-go-forward",
            Self::ShowChips => "chips",
            Self::ShowFlyout => "flyout",
            Self::Hide => "hide",
            Self::Show => "show",
            Self::Close => "close",
            Self::Reload => "reload",
            Self::HardReload => "hard-reload",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::ZoomReset => "zoom-reset",
            Self::OpenExternal => "open-external",
            Self::SetOpacity => "set-opacity",
            Self::SetClickThrough => "set-click-through",
            Self::CapturePng => "capture-png",
            Self::ExportHtml => "export-html",
            Self::ExportPdf => "export-pdf",
            Self::SaveDownload => "save-download",
            Self::DialogOpenFile => "dialog-open-file",
            Self::DialogSelectFolder => "dialog-select-folder",
            Self::DialogSaveFile => "dialog-save-file",
            Self::FileReadText => "file-read-text",
            Self::FileReadBase64 => "file-read-base64",
            Self::SafeStorageEncrypt => "safe-storage-encrypt",
            Self::SafeStorageDecrypt => "safe-storage-decrypt",
            Self::Notify => "notify",
            Self::GetTheme => "get-theme",
            Self::GetPlatform => "get-platform",
            Self::ClipboardReadText => "clipboard-read-text",
            Self::ClipboardWriteText => "clipboard-write-text",
            Self::ClipboardReadHtml => "clipboard-read-html",
            Self::ClipboardWriteHtml => "clipboard-write-html",
            Self::ClipboardReadImage => "clipboard-read-image",
            Self::ClipboardWriteImage => "clipboard-write-image",
            Self::ClipboardClear => "clipboard-clear",
            Self::AutostartGet => "autostart-get",
            Self::AutostartSet => "autostart-set",
            Self::ProtocolClientSet => "protocol-client-set",
            Self::ProtocolClientRemove => "protocol-client-remove",
            Self::SessionListSiteData => "session-list-site-data",
            Self::SessionClearSiteData => "session-clear-site-data",
            Self::SessionClearCookies => "session-clear-cookies",
            Self::SessionClearCache => "session-clear-cache",
            Self::OpenChildWindow => "open-child-window",
        }
    }
}

/// Remove control characters and surrounding whitespace from a native window
/// title, keeping it to at most 256 Unicode scalar values. An empty result uses
/// the supplied fallback title.
pub fn sanitize_window_title(raw: &str, fallback: &str) -> String {
    fn clean(value: &str) -> String {
        let filtered: String = value.chars().filter(|ch| !ch.is_control()).collect();
        filtered
            .trim()
            .chars()
            .take(256)
            .collect::<String>()
            .trim()
            .to_owned()
    }

    let title = clean(raw);
    if title.is_empty() {
        clean(fallback)
    } else {
        title
    }
}

/// Classification of an incoming `SimpleDialog::Prompt` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeCommand<'a> {
    /// Authenticated synchronous host window/platform command.
    Window(WindowCommand),
    /// Authenticated non-blocking async host window/platform command.
    WindowAsync {
        call_id: u64,
        command: WindowCommand,
    },
    /// Authenticated non-blocking async application command.
    AppAsync { call_id: u64, command: &'a str },
    /// Unauthenticated or unknown privileged command — must be rejected and
    /// never forwarded to the application handler.
    Rejected(String),
    /// Normal synchronous application command (e.g. `"load"`, `"save"`, `"load_state"`).
    App(&'a str),
}

/// Fill `bytes` from the operating system's cryptographic entropy source
/// (`/dev/urandom` on Unix, `BCryptGenRandom` on Windows).
///
/// Returns `false` on platforms without an implemented source, or when the
/// OS source fails; callers treat that as fatal rather than falling back to
/// a weaker generator (see [`generate_session_token`]).
#[cfg(unix)]
fn fill_os_random(bytes: &mut [u8]) -> bool {
    use std::io::Read;
    let Ok(mut source) = std::fs::File::open("/dev/urandom") else {
        return false;
    };
    source.read_exact(bytes).is_ok()
}

/// Fill `bytes` from the operating system's cryptographic entropy source
/// (`BCryptGenRandom` with the system-preferred RNG).
#[cfg(windows)]
fn fill_os_random(bytes: &mut [u8]) -> bool {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(
            h_algorithm: *mut std::ffi::c_void,
            pb_buffer: *mut u8,
            cb_buffer: u32,
            dw_flags: u32,
        ) -> i32;
    }
    /// `BCRYPT_USE_SYSTEM_PREFERRED_RNG`: use the system-preferred RNG
    /// without opening an algorithm handle first.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    let Ok(len) = u32::try_from(bytes.len()) else {
        return false;
    };
    // SAFETY: `bytes` is a valid writable buffer of `len` bytes and the
    // system-preferred RNG needs no algorithm handle.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            len,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    status == 0
}

/// No OS entropy source is implemented on this platform.
#[cfg(not(any(unix, windows)))]
fn fill_os_random(bytes: &mut [u8]) -> bool {
    let _ = bytes;
    false
}

/// Generate a per-launch 128-bit hex session token for same-realm preload
/// hardening (defense-in-depth against raw `window.prompt("dn-window:...")`
/// spoofing; not a substitute for an isolated JS compartment).
///
/// The 16 bytes come from the OS cryptographic entropy source
/// ([`fill_os_random`]) so the token actually carries 128 bits of entropy,
/// not just 128 bits of length.
///
/// # Panics
/// Panics when the OS entropy source is unavailable. Issuing a token from a
/// weaker, guessable generator (clock/PID/hash mixing) would silently
/// downgrade the only authentication on privileged bridge commands, so
/// token generation is deliberately fail-closed: the host must not start
/// its privileged IPC with a token an attacker could predict.
pub fn generate_session_token() -> String {
    let mut bytes = [0u8; 16];
    assert!(
        fill_os_random(&mut bytes),
        "OS entropy source unavailable; refusing to issue a guessable session token"
    );
    session_token_from_bytes(&bytes)
}

/// Hex-encode 16 entropy bytes as the 32-character session token.
fn session_token_from_bytes(bytes: &[u8; 16]) -> String {
    let mut token = String::with_capacity(32);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// Constant-time equality check for ASCII session tokens.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in ab.iter().zip(bb.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_window_command_name(name: &str) -> Option<WindowCommand> {
    match name {
        "drag" => Some(WindowCommand::Drag),
        "drag-resize" => Some(WindowCommand::DragResize),
        "minimize" => Some(WindowCommand::Minimize),
        "toggle-maximize" => Some(WindowCommand::ToggleMaximize),
        "is-maximized" => Some(WindowCommand::IsMaximized),
        "set-title" => Some(WindowCommand::SetTitle),
        "set-fullscreen" => Some(WindowCommand::SetFullscreen),
        "is-fullscreen" => Some(WindowCommand::IsFullscreen),
        "set-always-on-top" => Some(WindowCommand::SetAlwaysOnTop),
        "focus" => Some(WindowCommand::Focus),
        "request-user-attention" => Some(WindowCommand::RequestUserAttention),
        "get-bounds" => Some(WindowCommand::GetBounds),
        "set-size" => Some(WindowCommand::SetSize),
        "go-back" => Some(WindowCommand::GoBack),
        "go-forward" => Some(WindowCommand::GoForward),
        "can-go-back" => Some(WindowCommand::CanGoBack),
        "can-go-forward" => Some(WindowCommand::CanGoForward),
        "chips" => Some(WindowCommand::ShowChips),
        "flyout" => Some(WindowCommand::ShowFlyout),
        "hide" => Some(WindowCommand::Hide),
        "show" => Some(WindowCommand::Show),
        "close" => Some(WindowCommand::Close),
        "reload" => Some(WindowCommand::Reload),
        "hard-reload" => Some(WindowCommand::HardReload),
        "zoom-in" => Some(WindowCommand::ZoomIn),
        "zoom-out" => Some(WindowCommand::ZoomOut),
        "zoom-reset" => Some(WindowCommand::ZoomReset),
        "open-external" => Some(WindowCommand::OpenExternal),
        "set-opacity" => Some(WindowCommand::SetOpacity),
        "set-click-through" => Some(WindowCommand::SetClickThrough),
        "capture-png" => Some(WindowCommand::CapturePng),
        "export-html" => Some(WindowCommand::ExportHtml),
        "export-pdf" => Some(WindowCommand::ExportPdf),
        "save-download" => Some(WindowCommand::SaveDownload),
        "dialog-open-file" => Some(WindowCommand::DialogOpenFile),
        "dialog-select-folder" => Some(WindowCommand::DialogSelectFolder),
        "dialog-save-file" => Some(WindowCommand::DialogSaveFile),
        "file-read-text" => Some(WindowCommand::FileReadText),
        "file-read-base64" => Some(WindowCommand::FileReadBase64),
        "safe-storage-encrypt" => Some(WindowCommand::SafeStorageEncrypt),
        "safe-storage-decrypt" => Some(WindowCommand::SafeStorageDecrypt),
        "notify" => Some(WindowCommand::Notify),
        "get-theme" => Some(WindowCommand::GetTheme),
        "get-platform" => Some(WindowCommand::GetPlatform),
        "clipboard-read-text" => Some(WindowCommand::ClipboardReadText),
        "clipboard-write-text" => Some(WindowCommand::ClipboardWriteText),
        "clipboard-read-html" => Some(WindowCommand::ClipboardReadHtml),
        "clipboard-write-html" => Some(WindowCommand::ClipboardWriteHtml),
        "clipboard-read-image" => Some(WindowCommand::ClipboardReadImage),
        "clipboard-write-image" => Some(WindowCommand::ClipboardWriteImage),
        "clipboard-clear" => Some(WindowCommand::ClipboardClear),
        "autostart-get" => Some(WindowCommand::AutostartGet),
        "autostart-set" => Some(WindowCommand::AutostartSet),
        "protocol-client-set" => Some(WindowCommand::ProtocolClientSet),
        "protocol-client-remove" => Some(WindowCommand::ProtocolClientRemove),
        "session-list-site-data" => Some(WindowCommand::SessionListSiteData),
        "session-clear-site-data" => Some(WindowCommand::SessionClearSiteData),
        "session-clear-cookies" => Some(WindowCommand::SessionClearCookies),
        "session-clear-cache" => Some(WindowCommand::SessionClearCache),
        "open-child-window" => Some(WindowCommand::OpenChildWindow),
        _ => None,
    }
}

/// Parse a bridge command string into a window command, if it is one.
pub fn parse_window_command(command: &str) -> Option<WindowCommand> {
    let name = command.strip_prefix(WINDOW_COMMAND_PREFIX)?;
    parse_window_command_name(name)
}

/// Classify an incoming prompt message using the per-launch session token.
///
/// Privileged commands (`dn-window:*`) and async correlated commands
/// (`dn-window-async:<id>:<cmd>`, `dn-app-async:<id>:<cmd>`) MUST arrive
/// wrapped with `dn-tok:<session_token>:` via the frozen preload bridge;
/// raw unauthenticated calls or wrong tokens are rejected.
pub fn classify_command<'a>(raw_message: &'a str, session_token: &str) -> BridgeCommand<'a> {
    if let Some(rest) = raw_message.strip_prefix(TOKEN_COMMAND_PREFIX) {
        let Some((token, inner)) = rest.split_once(':') else {
            return BridgeCommand::Rejected("不正なブリッジ認証形式です".to_owned());
        };
        if !constant_time_eq(token, session_token) {
            return BridgeCommand::Rejected("ブリッジ認証トークンが一致しません".to_owned());
        }
        if let Some(win_async) = inner.strip_prefix(WINDOW_ASYNC_PREFIX) {
            let Some((id_str, cmd_name)) = win_async.split_once(':') else {
                return BridgeCommand::Rejected(
                    "不正な非同期ウィンドウコマンド形式です".to_owned(),
                );
            };
            let Ok(call_id) = id_str.parse::<u64>() else {
                return BridgeCommand::Rejected("非同期 call_id が不正です".to_owned());
            };
            if call_id == 0 {
                return BridgeCommand::Rejected("非同期 call_id に 0 は指定できません".to_owned());
            }
            return match parse_window_command_name(cmd_name) {
                Some(command) => BridgeCommand::WindowAsync { call_id, command },
                None => {
                    BridgeCommand::Rejected(format!("未対応の非同期ウィンドウコマンド: {cmd_name}"))
                }
            };
        }
        if let Some(app_async) = inner.strip_prefix(APP_ASYNC_PREFIX) {
            let Some((id_str, app_cmd)) = app_async.split_once(':') else {
                return BridgeCommand::Rejected("不正な非同期アプリコマンド形式です".to_owned());
            };
            let Ok(call_id) = id_str.parse::<u64>() else {
                return BridgeCommand::Rejected("非同期 call_id が不正です".to_owned());
            };
            if call_id == 0 || app_cmd.is_empty() || app_cmd.starts_with("dn-") {
                return BridgeCommand::Rejected("不正な非同期アプリコマンド指定です".to_owned());
            }
            return BridgeCommand::AppAsync {
                call_id,
                command: app_cmd,
            };
        }
        return match parse_window_command(inner) {
            Some(cmd) => BridgeCommand::Window(cmd),
            None => BridgeCommand::Rejected(format!("未対応のウィンドウコマンド: {inner}")),
        };
    }
    if raw_message.starts_with(WINDOW_COMMAND_PREFIX)
        || raw_message.starts_with(WINDOW_ASYNC_PREFIX)
        || raw_message.starts_with(APP_ASYNC_PREFIX)
    {
        return BridgeCommand::Rejected(
            "未認証の特権コマンドは拒否されました（window.Draconium API を使用してください）"
                .to_owned(),
        );
    }
    BridgeCommand::App(raw_message)
}

/// Strict allowlist for `dn-window:open-external` (see `docs/electron-mapping.md`
/// §1 `shell`). Only `https://` URLs with a non-empty host and `mailto:`
/// addresses without control/whitespace characters are permitted; `file:`,
/// `http:`, `javascript:`, executables, and UNC paths are always rejected.
pub fn allows_external_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed != url
        || trimmed
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return false;
    }
    let Some((scheme, rest)) = trimmed.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "https" => {
            let Some(after_slashes) = rest.strip_prefix("//") else {
                return false;
            };
            if after_slashes.contains('\\') || after_slashes.contains('"') {
                return false;
            }
            let authority = after_slashes.split(['/', '?', '#']).next().unwrap_or("");
            !authority.is_empty() && !authority.starts_with('@')
        }
        "mailto" => !rest.is_empty() && !rest.starts_with("//") && rest.contains('@'),
        _ => false,
    }
}

/// Navigation policy for local Draconium apps (Milestone B: external URL
/// blocking).
pub fn allows_navigation(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        || !scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return false;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "file" => {
            let Some(after_slashes) = rest.strip_prefix("//") else {
                return false;
            };
            let authority = after_slashes.split('/').next().unwrap_or("");
            authority.is_empty() || authority.eq_ignore_ascii_case("localhost")
        }
        "dn" => {
            let Some(after_slashes) = rest.strip_prefix("//") else {
                return false;
            };
            let authority = after_slashes.split(['/', '?', '#']).next().unwrap_or("");
            authority.eq_ignore_ascii_case("app")
        }
        "about" => rest.eq_ignore_ascii_case("blank"),
        _ => false,
    }
}

/// Navigation policy that additionally permits the active loopback `--dev-url`
/// origin when explicitly configured for frontend HMR development.
pub fn allows_navigation_with_dev(url: &str, dev_origin: Option<&DevOrigin>) -> bool {
    if allows_navigation(url) {
        return true;
    }
    dev_origin.is_some_and(|dev| dev.matches_url(url))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InitialOrigin {
    DnApp,
    Http {
        scheme: &'static str,
        host_lower: String,
        port: u16,
    },
    File {
        segments: Vec<String>,
        base_dir_len: usize,
        windows_drive: bool,
    },
}

fn decode_percent_segment_strict(raw: &str) -> Option<String> {
    if raw.contains('\\') || raw.contains('\0') {
        return None;
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied()?;
            let lo = bytes.get(i + 2).copied()?;
            let hex = [hi, lo];
            let hex_str = std::str::from_utf8(&hex).ok()?;
            let decoded = u8::from_str_radix(hex_str, 16).ok()?;
            if matches!(decoded, 0 | b'/' | b'\\') {
                return None;
            }
            out.push(decoded);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_file_url_parts(url: &str) -> Option<(Vec<String>, bool, bool)> {
    if url.is_empty() || url.chars().any(|ch| ch.is_control() || ch == '\\') {
        return None;
    }
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let after_slashes = rest.strip_prefix("//")?;
    let before_qf = after_slashes.split(['?', '#']).next().unwrap_or("");
    let slash_idx = before_qf.find('/')?;
    let authority = &before_qf[..slash_idx];
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let path_part = &before_qf[slash_idx..];
    let trailing_slash = path_part.ends_with('/');
    let mut segments = Vec::new();
    for raw_seg in path_part.split('/') {
        if raw_seg.is_empty() {
            continue;
        }
        let decoded = decode_percent_segment_strict(raw_seg)?;
        if decoded.is_empty()
            || decoded == "."
            || decoded == ".."
            || decoded.contains('/')
            || decoded.contains('\\')
            || decoded.contains('\0')
        {
            return None;
        }
        segments.push(decoded);
    }
    if segments.is_empty() {
        return None;
    }
    let windows_drive = segments.first().is_some_and(|first| {
        first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.as_bytes()[1] == b':'
    });
    Some((segments, trailing_slash, windows_drive))
}

fn parse_http_origin(url: &str) -> Option<(&'static str, String, u16)> {
    if url.is_empty() || url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return None;
    }
    let (scheme_raw, rest) = url.split_once("://")?;
    let (scheme, default_port) = if scheme_raw.eq_ignore_ascii_case("http") {
        ("http", 80u16)
    } else if scheme_raw.eq_ignore_ascii_case("https") {
        ("https", 443u16)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return None;
    }
    let (host_raw, port) = if let Some(bracket_end) = authority.find(']') {
        if !authority.starts_with('[') {
            return None;
        }
        let host_slice = &authority[..=bracket_end];
        let tail = &authority[bracket_end + 1..];
        let port = if tail.is_empty() {
            default_port
        } else if let Some(port_str) = tail.strip_prefix(':') {
            let parsed = port_str.parse::<u16>().ok()?;
            if parsed == 0 {
                return None;
            }
            parsed
        } else {
            return None;
        };
        (host_slice, port)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        let parsed = p.parse::<u16>().ok()?;
        if parsed == 0 || h.is_empty() {
            return None;
        }
        (h, parsed)
    } else {
        (authority, default_port)
    };
    Some((scheme, host_raw.to_ascii_lowercase(), port))
}

fn parse_initial_origin(url: &str) -> Option<InitialOrigin> {
    let (scheme, rest) = url.split_once(':')?;
    if scheme.eq_ignore_ascii_case("dn") {
        let after_slashes = rest.strip_prefix("//")?;
        let authority = after_slashes.split(['/', '?', '#']).next().unwrap_or("");
        return authority
            .eq_ignore_ascii_case("app")
            .then_some(InitialOrigin::DnApp);
    }
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        let (scheme_norm, host_lower, port) = parse_http_origin(url)?;
        return Some(InitialOrigin::Http {
            scheme: scheme_norm,
            host_lower,
            port,
        });
    }
    if scheme.eq_ignore_ascii_case("file") {
        let (segments, trailing_slash, windows_drive) = parse_file_url_parts(url)?;
        let base_dir_len = if trailing_slash {
            segments.len()
        } else {
            segments.len().saturating_sub(1)
        };
        return Some(InitialOrigin::File {
            segments,
            base_dir_len,
            windows_drive,
        });
    }
    None
}

fn file_segments_eq(a: &[String], b: &[String], case_insensitive: bool) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(left, right)| {
            if case_insensitive {
                left.to_lowercase() == right.to_lowercase()
            } else {
                left == right
            }
        })
}

fn file_segments_to_path(segments: &[String], windows_drive: bool) -> Option<std::path::PathBuf> {
    if segments.is_empty() {
        return None;
    }
    if windows_drive {
        let mut path = std::path::PathBuf::from(format!("{}\\", segments[0]));
        for seg in &segments[1..] {
            path.push(seg);
        }
        Some(path)
    } else {
        if cfg!(windows) {
            return None;
        }
        let mut path = std::path::PathBuf::from("/");
        for seg in segments {
            path.push(seg);
        }
        Some(path)
    }
}

/// Return `true` when `url` represents one of the origin-bound schemes capable
/// of receiving host bridge privileges (`dn://app/`, `http(s)://...`, or local
/// `file://...`). `about:blank` and other non-origin URLs return `false`.
pub fn has_supported_origin(url: &str) -> bool {
    parse_initial_origin(url).is_some()
}

/// Return `true` when `url` uses the `file://` scheme (case-insensitively).
pub fn is_file_url(url: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("file"))
}

/// Canonicalize the allowed base directory for a `file://` launch URL,
/// failing closed if `initial_url` escapes its own parent directory via symlink
/// or junction or cannot be resolved on disk.
pub fn resolve_initial_file_dir(initial_url: &str) -> Option<std::path::PathBuf> {
    let (segments, trailing_slash, windows_drive) = parse_file_url_parts(initial_url)?;
    let path = file_segments_to_path(&segments, windows_drive)?;
    let canon = std::fs::canonicalize(&path).ok()?;
    if trailing_slash {
        canon.is_dir().then_some(canon)
    } else {
        let parent = path.parent()?;
        let parent_canon = std::fs::canonicalize(parent).ok()?;
        if !canon.starts_with(&parent_canon) {
            return None;
        }
        parent_canon.is_dir().then_some(parent_canon)
    }
}

/// Verify on disk that a `file://` target URL resolves via `fs::canonicalize`
/// to an existing path strictly contained inside `allowed_dir_canon` (preventing
/// symlinks or Windows junctions under the app folder from reading/running
/// documents outside it).
pub fn file_url_is_contained(target_url: &str, allowed_dir_canon: &std::path::Path) -> bool {
    let Some((segments, _trailing, windows_drive)) = parse_file_url_parts(target_url) else {
        return false;
    };
    let Some(path) = file_segments_to_path(&segments, windows_drive) else {
        return false;
    };
    let Ok(canon) = std::fs::canonicalize(&path) else {
        return false;
    };
    canon != allowed_dir_canon && canon.starts_with(allowed_dir_canon)
}

/// Return `true` when `current_url` belongs to the same trust origin as
/// `initial_url`:
/// - `dn://app/...` matches only `dn://app/...`
/// - `http(s)://host:port/...` matches exact `(scheme, host, port)`
/// - `file://...` matches only local `file://` URLs confined under
///   `initial_url`'s parent directory without traversal segments.
pub fn url_matches_initial_origin(current_url: &str, initial_url: &str) -> bool {
    let Some(expected) = parse_initial_origin(initial_url) else {
        return false;
    };
    match expected {
        InitialOrigin::DnApp => matches!(
            parse_initial_origin(current_url),
            Some(InitialOrigin::DnApp)
        ),
        InitialOrigin::Http {
            scheme,
            host_lower,
            port,
        } => {
            matches!(
                parse_http_origin(current_url),
                Some((cur_scheme, cur_host, cur_port))
                    if cur_scheme == scheme && cur_host == host_lower && cur_port == port
            )
        }
        InitialOrigin::File {
            segments: init_segs,
            base_dir_len,
            windows_drive,
        } => {
            let Some((cur_segs, _trailing, cur_win_drive)) = parse_file_url_parts(current_url)
            else {
                return false;
            };
            if windows_drive != cur_win_drive {
                return false;
            }
            if file_segments_eq(&cur_segs, &init_segs, windows_drive) {
                return true;
            }
            if base_dir_len == 0 || cur_segs.len() <= base_dir_len {
                return false;
            }
            file_segments_eq(
                &cur_segs[..base_dir_len],
                &init_segs[..base_dir_len],
                windows_drive,
            )
        }
    }
}

/// Strict same-origin navigation policy with optional filesystem canonical
/// containment for `file://` URLs:
/// - `dn://app/...` apps may navigate only within `dn://app/...`
/// - `--dev-url` apps may navigate only within their matching loopback dev origin
/// - `file://` apps may navigate only to existing `file://` targets inside
///   `initial_file_dir` (and matching `initial_url`'s origin).
pub fn allows_navigation_for_origin_with_fs(
    target_url: &str,
    initial_url: &str,
    initial_file_dir: Option<&std::path::Path>,
    dev_origin: Option<&DevOrigin>,
) -> bool {
    if !allows_navigation_for_origin(target_url, initial_url, dev_origin) {
        return false;
    }
    if is_file_url(target_url) {
        return initial_file_dir.is_some_and(|dir| file_url_is_contained(target_url, dir));
    }
    true
}

/// Origin-bound navigation policy (syntax-level counterpart of
/// [`allows_navigation_for_origin_with_fs`] when no on-disk root is resolved):
/// rejects cross-origin transitions such as `dn://app/` -> `file://` or
/// foreign origins while keeping local intra-origin transitions working.
pub fn allows_navigation_for_origin(
    target_url: &str,
    initial_url: &str,
    dev_origin: Option<&DevOrigin>,
) -> bool {
    allows_navigation_with_dev(target_url, dev_origin)
        && url_matches_initial_origin(target_url, initial_url)
}

/// Child-window creation allowlist for `Draconium.window.openChild`:
/// allows `dn://app/`, `about:blank`, the configured `--dev-url` origin, and
/// (when launched from `file://`) canonically contained same-directory
/// `file://` URLs.
pub fn allows_child_window_url(
    target_url: &str,
    initial_url: &str,
    initial_file_dir: Option<&std::path::Path>,
    dev_origin: Option<&DevOrigin>,
) -> bool {
    if !allows_navigation_with_dev(target_url, dev_origin) {
        return false;
    }
    if is_file_url(target_url) {
        return is_file_url(initial_url)
            && url_matches_initial_origin(target_url, initial_url)
            && initial_file_dir.is_some_and(|dir| file_url_is_contained(target_url, dir));
    }
    true
}

/// Render [`PRELOAD_SCRIPT`] wrapped in an origin guard matching `initial_url`,
/// preventing navigated foreign-origin documents from receiving
/// `window.Draconium` or the launch session token.
pub fn render_preload_script_for_origin(session_token: &str, initial_url: &str) -> String {
    let Some(expected) = parse_initial_origin(initial_url) else {
        return "(() => {})();".to_owned();
    };
    let guard_expr = match expected {
        InitialOrigin::DnApp => concat!(
            "(() => {",
            " if (typeof window === \"undefined\" || window.top !== window) return false;",
            " const href = String((window.location && window.location.href) || \"\");",
            " return /^dn:\\/\\/app(?:\\/|$|\\?|#)/i.test(href);",
            " })()"
        )
        .to_owned(),
        InitialOrigin::Http {
            scheme,
            host_lower,
            port,
        } => {
            let proto = format!("{scheme}:");
            let unbracketed = host_lower
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&host_lower)
                .to_owned();
            let default_port = if scheme == "https" { "443" } else { "80" };
            let port_str = port.to_string();
            format!(
                concat!(
                    "(() => {{",
                    " if (typeof window === \"undefined\" || window.top !== window) return false;",
                    " const loc = window.location;",
                    " if (!loc || String(loc.protocol || \"\").toLowerCase() !== {proto}) return false;",
                    " const host = String(loc.hostname || \"\").toLowerCase();",
                    " if (host !== {host_lit} && host !== {unbracketed_lit}) return false;",
                    " const port = String(loc.port || {default_port});",
                    " return port === {port_lit};",
                    " }})()"
                ),
                proto = js_string_literal(&proto),
                host_lit = js_string_literal(&host_lower),
                unbracketed_lit = js_string_literal(&unbracketed),
                default_port = js_string_literal(default_port),
                port_lit = js_string_literal(&port_str),
            )
        }
        InitialOrigin::File {
            segments,
            base_dir_len,
            windows_drive,
        } => {
            let format_path = |count: usize, trailing_slash: bool, lower: bool| -> String {
                let mut s = String::from("/");
                for (idx, seg) in segments.iter().take(count).enumerate() {
                    if idx > 0 {
                        s.push('/');
                    }
                    if lower {
                        s.push_str(&seg.to_lowercase());
                    } else {
                        s.push_str(seg);
                    }
                }
                if trailing_slash && count > 0 {
                    s.push('/');
                }
                s
            };
            let exact_raw = format_path(segments.len(), false, false);
            let exact_file = format_path(segments.len(), false, windows_drive);
            let dir_prefix = if base_dir_len > 0 {
                format_path(base_dir_len, true, windows_drive)
            } else {
                String::new()
            };
            format!(
                concat!(
                    "(() => {{",
                    " if (typeof window === \"undefined\" || window.top !== window) return false;",
                    " const loc = window.location;",
                    " if (!loc || String(loc.protocol || \"\").toLowerCase() !== \"file:\") return false;",
                    " const host = String(loc.hostname || \"\").toLowerCase();",
                    " if (host !== \"\" && host !== \"localhost\") return false;",
                    " try {{",
                    " const rawPath = String(loc.pathname || \"\");",
                    " if (/%2f|%5c|%00|\\\\/i.test(rawPath)) return false;",
                    " const decoded = decodeURIComponent(rawPath);",
                    " if (decoded.indexOf(\"\\\\\") !== -1 || decoded.indexOf(\"\\0\") !== -1) return false;",
                    " const segs = decoded.split(\"/\").filter((s) => s.length > 0);",
                    " if (segs.some((s) => s === \".\" || s === \"..\")) return false;",
                    " const normRaw = \"/\" + segs.join(\"/\");",
                    " if (normRaw === {exact_raw}) return true;",
                    " const norm = {win_drive} ? normRaw.toLowerCase() : normRaw;",
                    " if (norm === {exact_file}) return true;",
                    " const dirPrefix = {dir_prefix};",
                    " return dirPrefix.length > 0 && norm.startsWith(dirPrefix) && norm.length > dirPrefix.length;",
                    " }} catch (_) {{ return false; }}",
                    " }})()"
                ),
                win_drive = if windows_drive { "true" } else { "false" },
                exact_raw = js_string_literal(&exact_raw),
                exact_file = js_string_literal(&exact_file),
                dir_prefix = js_string_literal(&dir_prefix),
            )
        }
    };
    let inner = render_preload_script(session_token);
    format!("(() => {{\n  if (!({guard_expr})) return;\n{inner}\n}})();")
}

/// Render [`PRELOAD_SCRIPT`] with the per-launch session token injected into
/// its private closure.
pub fn render_preload_script(session_token: &str) -> String {
    PRELOAD_SCRIPT.replace(TOKEN_PLACEHOLDER, &js_string_literal(session_token))
}

/// Preload script template injected into every page via `UserContentManager`.
///
/// Use [`render_preload_script`] to substitute the per-launch session token
/// before passing the script to `UserScript::new`.
pub const PRELOAD_SCRIPT: &str = r#"
(() => {
  if (window.Draconium && window.Draconium.__installed) return;
  const SESSION_TOKEN = __DN_SESSION_TOKEN__;
  const nativePrompt = window.prompt.bind(window);
  const listeners = new Map();
  const pendingAsync = new Map();
  let nextAsyncId = 1;

  function parseReply(raw) {
    if (typeof raw !== "string") return { ok: false, error: "cancelled" };
    if (raw.startsWith("ok:")) return { ok: true, data: raw.slice(3) };
    if (raw.startsWith("err:")) return { ok: false, error: raw.slice(4) };
    return { ok: false, error: "ホスト応答なし" };
  }

  function parseJsonReply(reply) {
    if (!reply.ok) return reply;
    try {
      return { ok: true, data: JSON.parse(reply.data) };
    } catch (error) {
      return { ok: false, error: "JSON 応答の解析に失敗しました" };
    }
  }

  function callApp(command, payload) {
    const cmd = String(command);
    if (cmd.startsWith("dn-")) {
      return { ok: false, error: "特権コマンドは window.Draconium API 経由で呼んでください" };
    }
    return parseReply(nativePrompt(cmd, payload == null ? "" : String(payload)));
  }

  function callHostWindow(name, payload) {
    const tagged = "dn-tok:" + SESSION_TOKEN + ":dn-window:" + name;
    return parseReply(nativePrompt(tagged, payload == null ? "" : String(payload)));
  }

  function callAppAsync(command, payload) {
    const cmd = String(command);
    if (cmd.startsWith("dn-")) {
      return Promise.resolve({ ok: false, error: "特権コマンドは window.Draconium API 経由で呼んでください" });
    }
    const callId = nextAsyncId++;
    return new Promise((resolve) => {
      pendingAsync.set(callId, { resolve, parseJson: false });
      const tagged = "dn-tok:" + SESSION_TOKEN + ":dn-app-async:" + callId + ":" + cmd;
      const syncAck = parseReply(nativePrompt(tagged, payload == null ? "" : String(payload)));
      if (!syncAck.ok) {
        pendingAsync.delete(callId);
        resolve(syncAck);
      }
    });
  }

  function callHostWindowAsync(name, payload, parseJson) {
    const callId = nextAsyncId++;
    return new Promise((resolve) => {
      pendingAsync.set(callId, { resolve, parseJson: Boolean(parseJson) });
      const tagged = "dn-tok:" + SESSION_TOKEN + ":dn-window-async:" + callId + ":" + name;
      const syncAck = parseReply(nativePrompt(tagged, payload == null ? "" : String(payload)));
      if (!syncAck.ok) {
        pendingAsync.delete(callId);
        resolve(syncAck);
      }
    });
  }

  const resolveAsyncFn = (callId, ok, payload, isJson) => {
    const entry = pendingAsync.get(callId);
    if (!entry) return;
    pendingAsync.delete(callId);
    if (!ok) {
      entry.resolve({ ok: false, error: String(payload) });
      return;
    }
    if (isJson || entry.parseJson) {
      try {
        entry.resolve({ ok: true, data: JSON.parse(payload) });
      } catch (_) {
        entry.resolve({ ok: false, error: "JSON 応答の解析に失敗しました" });
      }
    } else {
      entry.resolve({ ok: true, data: payload });
    }
  };
  Object.defineProperty(window, "__draconiumResolveAsync", {
    value: resolveAsyncFn,
    writable: false,
    configurable: false,
    enumerable: false,
  });

  function bytesToBase64(bytes) {
    let binary = "";
    const chunkSize = 0x8000;
    for (let i = 0; i < bytes.length; i += chunkSize) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunkSize));
    }
    return btoa(binary);
  }

  function dispatchSyntheticDropEvent(event, data) {
    if (typeof document === "undefined" || typeof CustomEvent !== "function") return;
    if (event === "file-drop-hover") {
      if (document.documentElement) document.documentElement.classList.add("dn-file-drop-hover");
      document.dispatchEvent(new CustomEvent("dn:file-drop-hover", { detail: data }));
    } else if (event === "file-drop-cancel") {
      if (document.documentElement) document.documentElement.classList.remove("dn-file-drop-hover");
      document.dispatchEvent(new CustomEvent("dn:file-drop-cancel", { detail: data }));
    } else if (event === "file-drop") {
      if (document.documentElement) document.documentElement.classList.remove("dn-file-drop-hover");
      let target = document.body || document.documentElement || document;
      if (data && typeof data.x === "number" && typeof data.y === "number" && typeof document.elementFromPoint === "function") {
        target = document.elementFromPoint(data.x, data.y) || target;
      }
      target.dispatchEvent(new CustomEvent("dn:file-drop", { detail: data, bubbles: true, cancelable: true }));
    }
  }

  const dispatchFn = (event, data) => {
    dispatchSyntheticDropEvent(event, data);
    const set = listeners.get(event);
    if (!set) return;
    for (const fn of [...set]) {
      try { fn(data); } catch (error) { console.error(error); }
    }
  };
  Object.defineProperty(window, "__draconiumDispatch", {
    value: dispatchFn,
    writable: false,
    configurable: false,
    enumerable: false,
  });

  const windowApi = Object.freeze({
    drag() { return callHostWindow("drag", ""); },
    dragResize(direction) { return callHostWindow("drag-resize", direction == null ? "" : String(direction)); },
    minimize() { return callHostWindow("minimize", ""); },
    toggleMaximize() { return callHostWindow("toggle-maximize", ""); },
    isMaximized() { return callHostWindow("is-maximized", ""); },
    setTitle(title) { return callHostWindow("set-title", title == null ? "" : String(title)); },
    setFullscreen(enabled) { return callHostWindow("set-fullscreen", enabled ? "true" : "false"); },
    isFullscreen() { return callHostWindow("is-fullscreen", ""); },
    setAlwaysOnTop(enabled) { return callHostWindow("set-always-on-top", enabled ? "true" : "false"); },
    focus() { return callHostWindow("focus", ""); },
    requestUserAttention(level) {
      const v = level === true ? "critical" : (level === false || level == null ? "" : String(level));
      return callHostWindow("request-user-attention", v);
    },
    getBounds() { return parseJsonReply(callHostWindow("get-bounds", "")); },
    setSize(width, height) {
      if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) {
        return { ok: false, error: "Width and height must be positive integers" };
      }
      return callHostWindow("set-size", JSON.stringify({ width, height }));
    },
    goBack() { return callHostWindow("go-back", ""); },
    goForward() { return callHostWindow("go-forward", ""); },
    canGoBack() { return callHostWindow("can-go-back", ""); },
    canGoForward() { return callHostWindow("can-go-forward", ""); },
    showChips() { return callHostWindow("chips", ""); },
    showFlyout() { return callHostWindow("flyout", ""); },
    hide() { return callHostWindow("hide", ""); },
    show() { return callHostWindow("show", ""); },
    close() { return callHostWindow("close", ""); },
    reload() { return callHostWindow("reload", ""); },
    hardReload() { return callHostWindow("hard-reload", ""); },
    zoomIn() { return callHostWindow("zoom-in", ""); },
    zoomOut() { return callHostWindow("zoom-out", ""); },
    zoomReset() { return callHostWindow("zoom-reset", ""); },
    openExternal(url) { return callHostWindow("open-external", url == null ? "" : String(url)); },
    setOpacity(opacity) { return callHostWindow("set-opacity", String(Number(opacity))); },
    setClickThrough(enabled) { return callHostWindow("set-click-through", enabled ? "true" : "false"); },
    capturePng(name) {
      return callHostWindow("capture-png", JSON.stringify({ name: name ? String(name) : "capture.png" }));
    },
    capturePngAsync(name) {
      return callHostWindowAsync("capture-png", JSON.stringify({ name: name ? String(name) : "capture.png" }), false);
    },
    exportHtml(name) {
      const html = (typeof document !== "undefined" && document.documentElement)
        ? "<!DOCTYPE html>\n" + document.documentElement.outerHTML
        : "";
      return callHostWindow("export-html", JSON.stringify({
        name: name ? String(name) : "export.html",
        text: html,
      }));
    },
    exportHtmlAsync(name) {
      const html = (typeof document !== "undefined" && document.documentElement)
        ? "<!DOCTYPE html>\n" + document.documentElement.outerHTML
        : "";
      return callHostWindowAsync("export-html", JSON.stringify({
        name: name ? String(name) : "export.html",
        text: html,
      }), false);
    },
    exportPdf(name) {
      return callHostWindow("export-pdf", JSON.stringify({ name: name ? String(name) : "capture.pdf" }));
    },
    exportPdfAsync(name) {
      return callHostWindowAsync("export-pdf", JSON.stringify({ name: name ? String(name) : "capture.pdf" }), false);
    },
    openChild(options) {
      const opts = typeof options === "string" ? { url: options } : (options || {});
      return callHostWindow("open-child-window", JSON.stringify({
        url: opts.url ? String(opts.url) : "about:blank",
        title: opts.title ? String(opts.title) : "",
        width: typeof opts.width === "number" ? opts.width : 800,
        height: typeof opts.height === "number" ? opts.height : 600,
      }));
    },
    getTheme() { return callHostWindow("get-theme", ""); },
    getPlatform() { return parseJsonReply(callHostWindow("get-platform", "")); },
  });

  const dialogApi = Object.freeze({
    openFile(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        multiple: Boolean(opts.multiple),
      });
      return parseJsonReply(callHostWindow("dialog-open-file", payload));
    },
    openFileAsync(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        multiple: Boolean(opts.multiple),
      });
      return callHostWindowAsync("dialog-open-file", payload, true);
    },
    selectFolder(options) {
      const opts = options && typeof options === "object" ? options : {};
      const title = opts.title ? String(opts.title) : "";
      return callHostWindow("dialog-select-folder", title);
    },
    selectFolderAsync(options) {
      const opts = options && typeof options === "object" ? options : {};
      const title = opts.title ? String(opts.title) : "";
      return callHostWindowAsync("dialog-select-folder", title, false);
    },
    saveTextFile(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        name: opts.name ? String(opts.name) : "export.txt",
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        text: opts.text == null ? "" : String(opts.text),
      });
      return callHostWindow("dialog-save-file", payload);
    },
    saveTextFileAsync(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        name: opts.name ? String(opts.name) : "export.txt",
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        text: opts.text == null ? "" : String(opts.text),
      });
      return callHostWindowAsync("dialog-save-file", payload, false);
    },
    saveBase64File(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        name: opts.name ? String(opts.name) : "export.bin",
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        dataBase64: opts.dataBase64 == null ? "" : String(opts.dataBase64),
      });
      return callHostWindow("dialog-save-file", payload);
    },
    saveBase64FileAsync(options) {
      const opts = options && typeof options === "object" ? options : {};
      const payload = JSON.stringify({
        name: opts.name ? String(opts.name) : "export.bin",
        extensions: Array.isArray(opts.extensions) ? opts.extensions.map(String) : [],
        dataBase64: opts.dataBase64 == null ? "" : String(opts.dataBase64),
      });
      return callHostWindowAsync("dialog-save-file", payload, false);
    },
  });

  const filesApi = Object.freeze({
    readText(token) { return callHostWindow("file-read-text", token == null ? "" : String(token)); },
    readTextAsync(token) { return callHostWindowAsync("file-read-text", token == null ? "" : String(token), false); },
    readBase64(token) { return callHostWindow("file-read-base64", token == null ? "" : String(token)); },
    readBase64Async(token) { return callHostWindowAsync("file-read-base64", token == null ? "" : String(token), false); },
  });

  const safeStorageApi = Object.freeze({
    encrypt(plaintext) { return callHostWindow("safe-storage-encrypt", plaintext == null ? "" : String(plaintext)); },
    encryptAsync(plaintext) { return callHostWindowAsync("safe-storage-encrypt", plaintext == null ? "" : String(plaintext), false); },
    decrypt(ciphertextBase64) { return callHostWindow("safe-storage-decrypt", ciphertextBase64 == null ? "" : String(ciphertextBase64)); },
    decryptAsync(ciphertextBase64) { return callHostWindowAsync("safe-storage-decrypt", ciphertextBase64 == null ? "" : String(ciphertextBase64), false); },
  });

  const clipboardApi = Object.freeze({
    readText() { return callHostWindow("clipboard-read-text", ""); },
    readTextAsync() { return callHostWindowAsync("clipboard-read-text", "", false); },
    writeText(text) { return callHostWindow("clipboard-write-text", text == null ? "" : String(text)); },
    writeTextAsync(text) { return callHostWindowAsync("clipboard-write-text", text == null ? "" : String(text), false); },
    readHtml() { return callHostWindow("clipboard-read-html", ""); },
    readHtmlAsync() { return callHostWindowAsync("clipboard-read-html", "", false); },
    writeHtml(html) { return callHostWindow("clipboard-write-html", html == null ? "" : String(html)); },
    writeHtmlAsync(html) { return callHostWindowAsync("clipboard-write-html", html == null ? "" : String(html), false); },
    readImage() { return parseJsonReply(callHostWindow("clipboard-read-image", "")); },
    readImageAsync() { return callHostWindowAsync("clipboard-read-image", "", true); },
    writeImage(options) {
      const opts = options && typeof options === "object" ? options : {};
      return callHostWindow("clipboard-write-image", JSON.stringify({
        width: Number(opts.width) || 0,
        height: Number(opts.height) || 0,
        rgbaBase64: opts.rgbaBase64 ? String(opts.rgbaBase64) : "",
        bmpBase64: opts.bmpBase64 ? String(opts.bmpBase64) : "",
      }));
    },
    writeImageAsync(options) {
      const opts = options && typeof options === "object" ? options : {};
      return callHostWindowAsync("clipboard-write-image", JSON.stringify({
        width: Number(opts.width) || 0,
        height: Number(opts.height) || 0,
        rgbaBase64: opts.rgbaBase64 ? String(opts.rgbaBase64) : "",
        bmpBase64: opts.bmpBase64 ? String(opts.bmpBase64) : "",
      }), false);
    },
    clear() { return callHostWindow("clipboard-clear", ""); },
  });

  const autostartApi = Object.freeze({
    isEnabled() { return parseJsonReply(callHostWindow("autostart-get", "")); },
    setEnabled(enabled) { return callHostWindow("autostart-set", enabled ? "true" : "false"); },
  });

  const protocolClientApi = Object.freeze({
    register(scheme) { return callHostWindow("protocol-client-set", scheme == null ? "" : String(scheme)); },
    unregister(scheme) { return callHostWindow("protocol-client-remove", scheme == null ? "" : String(scheme)); },
  });

  const sessionApi = Object.freeze({
    listSiteData() { return parseJsonReply(callHostWindow("session-list-site-data", "")); },
    listSiteDataAsync() { return callHostWindowAsync("session-list-site-data", "", true); },
    clearSiteData() { return callHostWindow("session-clear-site-data", ""); },
    clearSiteDataAsync() { return callHostWindowAsync("session-clear-site-data", "", false); },
    clearCookies(options) {
      const sessionOnly = Boolean(options && options.sessionOnly);
      return callHostWindow("session-clear-cookies", sessionOnly ? "session" : "all");
    },
    clearCookiesAsync(options) {
      const sessionOnly = Boolean(options && options.sessionOnly);
      return callHostWindowAsync("session-clear-cookies", sessionOnly ? "session" : "all", false);
    },
    clearCache() { return callHostWindow("session-clear-cache", ""); },
    clearCacheAsync() { return callHostWindowAsync("session-clear-cache", "", false); },
  });

  const api = Object.freeze({
    __installed: true,
    invoke: callApp,
    invokeAsync: callAppAsync,
    on(event, fn) {
      if (typeof fn !== "function") return;
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event).add(fn);
    },
    off(event, fn) {
      const set = listeners.get(event);
      if (set) set.delete(fn);
    },
    notify(options) {
      const opts = typeof options === "string" ? { title: document.title || "Draconium", body: options } : (options || {});
      return callHostWindow("notify", JSON.stringify({
        title: opts.title == null ? (document.title || "Draconium") : String(opts.title),
        body: opts.body == null ? "" : String(opts.body),
      }));
    },
    window: windowApi,
    dialog: dialogApi,
    files: filesApi,
    safeStorage: safeStorageApi,
    clipboard: clipboardApi,
    autostart: autostartApi,
    protocolClient: protocolClientApi,
    session: sessionApi,
  });

  Object.defineProperty(window, "Draconium", {
    value: api,
    writable: false,
    configurable: false,
    enumerable: true,
  });

  // Fallback for window.print() (unsupported in Servo 0.8.0) -> raster PDF export dialog.
  try {
    window.print = () => {
      const title = (typeof document !== "undefined" && document.title) ? document.title : "page";
      return windowApi.exportPdf(title + ".pdf");
    };
  } catch (_) {}

  // --- Engine-gap shim 1: HTML Popover API polyfill (missing in Servo 0.8.0) ---
  // Note: Popover CSS rules are injected at Origin::User via UserStyleSheet
  // (POPOVER_POLYFILL_CSS) so strict author `style-src 'self'` CSP is preserved.
  if (typeof HTMLElement !== "undefined" && !("popover" in HTMLElement.prototype)) {
    const openAuto = [];
    const normalizeMode = (el) => {
      if (!el.hasAttribute("popover")) return null;
      const raw = (el.getAttribute("popover") || "").toLowerCase();
      return raw === "manual" ? "manual" : "auto";
    };
    const emitToggle = (el, type, oldState, newState, cancelable) => {
      const ev = new Event(type, { bubbles: false, cancelable });
      try {
        Object.defineProperty(ev, "oldState", { value: oldState });
        Object.defineProperty(ev, "newState", { value: newState });
      } catch (_) {}
      return el.dispatchEvent(ev);
    };
    const hideOne = (el) => {
      if (!el.classList.contains("dn-popover-open")) return;
      emitToggle(el, "beforetoggle", "open", "closed", false);
      el.classList.remove("dn-popover-open");
      const idx = openAuto.indexOf(el);
      if (idx >= 0) openAuto.splice(idx, 1);
      emitToggle(el, "toggle", "open", "closed", false);
    };
    const hideAutoExceptAncestors = (target) => {
      for (let i = openAuto.length - 1; i >= 0; i--) {
        const pop = openAuto[i];
        if (target && (pop === target || pop.contains(target))) continue;
        hideOne(pop);
      }
    };
    Object.defineProperty(HTMLElement.prototype, "popover", {
      configurable: true,
      get() { return normalizeMode(this); },
      set(val) {
        if (val == null) this.removeAttribute("popover");
        else this.setAttribute("popover", String(val));
      },
    });
    HTMLElement.prototype.showPopover = function showPopover() {
      const mode = normalizeMode(this);
      if (!mode) throw new DOMException("Element does not have a popover attribute", "NotSupportedError");
      if (this.classList.contains("dn-popover-open")) return;
      if (!emitToggle(this, "beforetoggle", "closed", "open", true)) return;
      if (mode === "auto") {
        hideAutoExceptAncestors(this);
        openAuto.push(this);
      }
      this.classList.add("dn-popover-open");
      emitToggle(this, "toggle", "closed", "open", false);
    };
    HTMLElement.prototype.hidePopover = function hidePopover() {
      const mode = normalizeMode(this);
      if (!mode) throw new DOMException("Element does not have a popover attribute", "NotSupportedError");
      hideOne(this);
    };
    HTMLElement.prototype.togglePopover = function togglePopover(force) {
      const isOpen = this.classList.contains("dn-popover-open");
      const wantOpen = force === undefined ? !isOpen : Boolean(force);
      if (wantOpen) {
        this.showPopover();
        return this.classList.contains("dn-popover-open");
      }
      this.hidePopover();
      return false;
    };
    if (typeof document !== "undefined") {
      document.addEventListener("click", (event) => {
        const el = event.target instanceof Element ? event.target : null;
        const trigger = el ? el.closest("[popovertarget]") : null;
        if (trigger) {
          const id = trigger.getAttribute("popovertarget");
          const target = id ? document.getElementById(id) : null;
          if (target && normalizeMode(target)) {
            const action = (trigger.getAttribute("popovertargetaction") || "toggle").toLowerCase();
            if (action === "show") target.showPopover();
            else if (action === "hide") target.hidePopover();
            else target.togglePopover();
            return;
          }
        }
        hideAutoExceptAncestors(el);
      });
      document.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && openAuto.length > 0) {
          hideOne(openAuto[openAuto.length - 1]);
        }
      });
    }
  }

  // --- Engine-gap shim 2: Declarative -webkit-app-region: drag / [data-dn-drag] ---
  if (typeof document !== "undefined") {
    const INTERACTIVE = "button, a, input, select, textarea, label, [contenteditable='true'], [role='button']";
    const readAppRegion = (el) => {
      if (el.hasAttribute("data-dn-no-drag")) return "no-drag";
      if (el.hasAttribute("data-dn-drag")) return "drag";
      const inline = el.style && el.style.getPropertyValue ? el.style.getPropertyValue("-webkit-app-region").trim().toLowerCase() : "";
      if (inline === "drag" || inline === "no-drag") return inline;
      if (typeof window.getComputedStyle === "function") {
        const custom = window.getComputedStyle(el).getPropertyValue("--dn-app-region").trim().toLowerCase();
        if (custom === "drag" || custom === "no-drag") return custom;
      }
      return "";
    };
    document.addEventListener("mousedown", (event) => {
      if (event.button !== 0 || event.defaultPrevented) return;
      let node = event.target instanceof Element ? event.target : null;
      while (node) {
        if (node.matches && node.matches(INTERACTIVE)) return;
        const region = readAppRegion(node);
        if (region === "no-drag") return;
        if (region === "drag") {
          windowApi.drag();
          return;
        }
        node = node.parentElement;
      }
    });
  }

  // --- Engine-gap shim 3: <a download> for data: and blob: URLs ---
  if (typeof document !== "undefined") {
    document.addEventListener("click", (event) => {
      if (event.defaultPrevented) return;
      const el = event.target instanceof Element ? event.target : null;
      const anchor = el ? el.closest("a[download][href]") : null;
      if (!anchor) return;
      const href = anchor.getAttribute("href") || "";
      const suggested = anchor.getAttribute("download") || "download.bin";
      if (href.startsWith("data:")) {
        event.preventDefault();
        const comma = href.indexOf(",");
        if (comma < 0) return;
        const meta = href.slice(5, comma);
        const body = href.slice(comma + 1);
        let dataBase64;
        if (/;base64/i.test(meta)) {
          dataBase64 = body;
        } else {
          const decoded = decodeURIComponent(body);
          dataBase64 = bytesToBase64(new TextEncoder().encode(decoded));
        }
        callHostWindow("save-download", JSON.stringify({ name: suggested, dataBase64 }));
      } else if (href.startsWith("blob:")) {
        event.preventDefault();
        fetch(href)
          .then((res) => res.arrayBuffer())
          .then((buf) => {
            const dataBase64 = bytesToBase64(new Uint8Array(buf));
            callHostWindow("save-download", JSON.stringify({ name: suggested, dataBase64 }));
          })
          .catch((error) => console.error("[dn download]", error));
      }
    }, true);
  }
})();
"#;

/// Escape a string as a double-quoted JavaScript string literal (including
/// the surrounding quotes), safe to embed in an evaluated script.
pub fn js_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Build the script that delivers a host → page event to the preload
/// listener registry. `data_json` must already be a JSON value (for example
/// `{"x":10,"y":20}` or `"chips"`); it is parsed back into a JS value by the
/// evaluated script so listeners receive objects, not strings.
pub fn dispatch_script(event: &str, data_json: &str) -> String {
    format!(
        "window.__draconiumDispatch && window.__draconiumDispatch({}, JSON.parse({}));",
        js_string_literal(event),
        js_string_literal(data_json),
    )
}

/// Build the script that resolves a pending `Draconium.invokeAsync` /
/// `*Async` Promise in the preload bridge.
pub fn resolve_async_script(call_id: u64, ok: bool, payload: &str, is_json: bool) -> String {
    format!(
        "window.__draconiumResolveAsync && window.__draconiumResolveAsync({call_id}, {ok}, {}, {is_json});",
        js_string_literal(payload),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_commands_parse() {
        assert_eq!(
            parse_window_command("dn-window:drag"),
            Some(WindowCommand::Drag)
        );
        for (name, command) in [
            ("set-title", WindowCommand::SetTitle),
            ("set-fullscreen", WindowCommand::SetFullscreen),
            ("is-fullscreen", WindowCommand::IsFullscreen),
            ("set-always-on-top", WindowCommand::SetAlwaysOnTop),
            ("focus", WindowCommand::Focus),
            (
                "request-user-attention",
                WindowCommand::RequestUserAttention,
            ),
            ("get-bounds", WindowCommand::GetBounds),
            ("set-size", WindowCommand::SetSize),
            ("go-back", WindowCommand::GoBack),
            ("go-forward", WindowCommand::GoForward),
            ("can-go-back", WindowCommand::CanGoBack),
            ("can-go-forward", WindowCommand::CanGoForward),
        ] {
            assert_eq!(
                parse_window_command(&format!("dn-window:{name}")),
                Some(command)
            );
            assert_eq!(command.as_str(), name);
        }
        assert_eq!(
            parse_window_command("dn-window:chips"),
            Some(WindowCommand::ShowChips)
        );
        assert_eq!(
            parse_window_command("dn-window:flyout"),
            Some(WindowCommand::ShowFlyout)
        );
        assert_eq!(
            parse_window_command("dn-window:hard-reload"),
            Some(WindowCommand::HardReload)
        );
        assert_eq!(
            parse_window_command("dn-window:zoom-in"),
            Some(WindowCommand::ZoomIn)
        );
        assert_eq!(
            parse_window_command("dn-window:zoom-out"),
            Some(WindowCommand::ZoomOut)
        );
        assert_eq!(
            parse_window_command("dn-window:zoom-reset"),
            Some(WindowCommand::ZoomReset)
        );
        assert_eq!(
            parse_window_command("dn-window:open-external"),
            Some(WindowCommand::OpenExternal)
        );
        assert_eq!(
            parse_window_command("dn-window:set-opacity"),
            Some(WindowCommand::SetOpacity)
        );
        assert_eq!(
            parse_window_command("dn-window:set-click-through"),
            Some(WindowCommand::SetClickThrough)
        );
        assert_eq!(
            parse_window_command("dn-window:capture-png"),
            Some(WindowCommand::CapturePng)
        );
        assert_eq!(
            parse_window_command("dn-window:export-html"),
            Some(WindowCommand::ExportHtml)
        );
        assert_eq!(
            parse_window_command("dn-window:export-pdf"),
            Some(WindowCommand::ExportPdf)
        );
        assert_eq!(
            parse_window_command("dn-window:save-download"),
            Some(WindowCommand::SaveDownload)
        );
        assert_eq!(
            parse_window_command("dn-window:dialog-open-file"),
            Some(WindowCommand::DialogOpenFile)
        );
        assert_eq!(
            parse_window_command("dn-window:dialog-save-file"),
            Some(WindowCommand::DialogSaveFile)
        );
        assert_eq!(
            parse_window_command("dn-window:file-read-text"),
            Some(WindowCommand::FileReadText)
        );
        assert_eq!(
            parse_window_command("dn-window:file-read-base64"),
            Some(WindowCommand::FileReadBase64)
        );
        assert_eq!(
            parse_window_command("dn-window:safe-storage-encrypt"),
            Some(WindowCommand::SafeStorageEncrypt)
        );
        assert_eq!(
            parse_window_command("dn-window:safe-storage-decrypt"),
            Some(WindowCommand::SafeStorageDecrypt)
        );
        assert_eq!(
            parse_window_command("dn-window:notify"),
            Some(WindowCommand::Notify)
        );
        assert_eq!(
            parse_window_command("dn-window:get-theme"),
            Some(WindowCommand::GetTheme)
        );
        assert_eq!(
            parse_window_command("dn-window:get-platform"),
            Some(WindowCommand::GetPlatform)
        );
        assert_eq!(
            parse_window_command("dn-window:clipboard-read-text"),
            Some(WindowCommand::ClipboardReadText)
        );
        assert_eq!(
            parse_window_command("dn-window:clipboard-write-html"),
            Some(WindowCommand::ClipboardWriteHtml)
        );
        assert_eq!(
            parse_window_command("dn-window:clipboard-read-image"),
            Some(WindowCommand::ClipboardReadImage)
        );
        assert_eq!(
            parse_window_command("dn-window:autostart-set"),
            Some(WindowCommand::AutostartSet)
        );
        assert_eq!(
            parse_window_command("dn-window:protocol-client-set"),
            Some(WindowCommand::ProtocolClientSet)
        );
        assert_eq!(
            parse_window_command("dn-window:session-clear-cookies"),
            Some(WindowCommand::SessionClearCookies)
        );
        assert_eq!(
            parse_window_command("dn-window:open-child-window"),
            Some(WindowCommand::OpenChildWindow)
        );
        assert_eq!(parse_window_command("dn-window:nope"), None);
        assert_eq!(parse_window_command("load"), None);
        assert_eq!(parse_window_command("save"), None);
    }

    #[test]
    fn window_titles_are_sanitized_and_bounded() {
        assert_eq!(
            sanitize_window_title(" \tHello\nWorld\u{7f} ", "Fallback"),
            "HelloWorld"
        );
        assert_eq!(sanitize_window_title(" \n\t", "  Fallback\n"), "Fallback");

        let long_title = "界".repeat(300);
        let sanitized = sanitize_window_title(&long_title, "Fallback");
        assert_eq!(sanitized.chars().count(), 256);
        assert_eq!(sanitized, "界".repeat(256));

        let unicode_boundary = format!("{}🌋", "é".repeat(255));
        assert_eq!(
            sanitize_window_title(&unicode_boundary, "Fallback"),
            unicode_boundary
        );
    }

    #[test]
    fn session_tokens_are_32_lower_hex_and_unique() {
        let first = generate_session_token();
        let second = generate_session_token();
        assert_eq!(first.len(), 32);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(first, second);
        let known: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        assert_eq!(
            session_token_from_bytes(&known),
            "000102030405060708090a0b0c0d0e0f"
        );
    }

    #[test]
    fn session_token_auth_and_async_command_classification() {
        let token = generate_session_token();
        assert_eq!(token.len(), 32);

        let valid = format!("dn-tok:{token}:dn-window:zoom-in");
        assert_eq!(
            classify_command(&valid, &token),
            BridgeCommand::Window(WindowCommand::ZoomIn)
        );

        let valid_win_async = format!("dn-tok:{token}:dn-window-async:42:capture-png");
        assert_eq!(
            classify_command(&valid_win_async, &token),
            BridgeCommand::WindowAsync {
                call_id: 42,
                command: WindowCommand::CapturePng,
            }
        );

        let valid_app_async = format!("dn-tok:{token}:dn-app-async:7:load_state");
        assert_eq!(
            classify_command(&valid_app_async, &token),
            BridgeCommand::AppAsync {
                call_id: 7,
                command: "load_state",
            }
        );

        // Zero call_id or unauthenticated async commands must be rejected.
        let zero_async = format!("dn-tok:{token}:dn-app-async:0:load");
        assert!(matches!(
            classify_command(&zero_async, &token),
            BridgeCommand::Rejected(_)
        ));
        assert!(matches!(
            classify_command("dn-app-async:1:load", &token),
            BridgeCommand::Rejected(_)
        ));
        assert!(matches!(
            classify_command("dn-window-async:1:close", &token),
            BridgeCommand::Rejected(_)
        ));
        assert!(matches!(
            classify_command("dn-window:close", &token),
            BridgeCommand::Rejected(_)
        ));
        assert!(matches!(
            classify_command("dn-tok:0000:dn-window:close", &token),
            BridgeCommand::Rejected(_)
        ));
        let unknown = format!("dn-tok:{token}:dn-window:nope");
        assert!(matches!(
            classify_command(&unknown, &token),
            BridgeCommand::Rejected(_)
        ));
        assert_eq!(classify_command("load", &token), BridgeCommand::App("load"));
    }

    #[test]
    fn dev_origin_validation_and_scoped_navigation() {
        let dev = DevOrigin::parse("http://127.0.0.1:5173").unwrap();
        assert_eq!(dev.host, "127.0.0.1");
        assert_eq!(dev.port, 5173);
        assert_eq!(dev.initial_url, "http://127.0.0.1:5173/");
        assert!(dev.matches_url("http://127.0.0.1:5173/workspace.html"));
        assert!(dev.matches_url("http://127.0.0.1:5173/assets/app.js?v=1"));
        assert!(!dev.matches_url("http://127.0.0.1:3000/workspace.html"));
        assert!(!dev.matches_url("http://localhost:5173/workspace.html"));
        assert!(!dev.matches_url("https://127.0.0.1:5173/workspace.html"));

        let ipv6 = DevOrigin::parse("http://[::1]:3000/app").unwrap();
        assert_eq!(ipv6.host, "[::1]");
        assert_eq!(ipv6.port, 3000);
        assert_eq!(ipv6.initial_url, "http://[::1]:3000/app");
        assert!(ipv6.matches_url("http://[::1]:3000/index.html"));

        assert!(DevOrigin::parse("http://0.0.0.0:5173").is_err());
        assert!(DevOrigin::parse("http://192.168.1.10:5173").is_err());
        assert!(DevOrigin::parse("https://127.0.0.1:5173").is_err());
        assert!(DevOrigin::parse("http://127.0.0.1").is_err());
        assert!(DevOrigin::parse("http://127.0.0.1:0").is_err());
        assert!(DevOrigin::parse("http://user@127.0.0.1:5173").is_err());

        assert!(!allows_navigation_with_dev(
            "http://127.0.0.1:5173/index.html",
            None
        ));
        assert!(allows_navigation_with_dev(
            "http://127.0.0.1:5173/index.html",
            Some(&dev)
        ));
        assert!(!allows_navigation_with_dev(
            "http://example.com/index.html",
            Some(&dev)
        ));
    }

    #[test]
    fn external_url_allowlist_is_strict() {
        assert!(allows_external_url("https://example.com/docs"));
        assert!(allows_external_url(
            "HTTPS://github.com/watanuki-azuma/Draconium"
        ));
        assert!(allows_external_url("mailto:user@example.com"));
        assert!(!allows_external_url("http://example.com/"));
        assert!(!allows_external_url("file:///C:/Windows/System32/cmd.exe"));
        assert!(!allows_external_url(r"\\server\share\run.exe"));
        assert!(!allows_external_url("javascript:alert(1)"));
        assert!(!allows_external_url("https://"));
        assert!(!allows_external_url("https://example.com/foo bar"));
        assert!(!allows_external_url("https://example.com/a\nb"));
        assert!(!allows_external_url("mailto:not-an-address"));
    }

    #[test]
    fn navigation_policy_allows_local_only() {
        assert!(allows_navigation("file:///C:/app/ui/workspace.html"));
        assert!(allows_navigation(
            "file://localhost/C:/app/ui/workspace.html"
        ));
        assert!(allows_navigation("dn://app/workspace.html"));
        assert!(allows_navigation("about:blank"));
        assert!(!allows_navigation("file://attacker/share/evil.html"));
        assert!(!allows_navigation("dn://other/workspace.html"));
        assert!(!allows_navigation("about:config"));
        assert!(!allows_navigation("data:text/html,hi"));
        assert!(!allows_navigation("blob:file:///1234"));
        assert!(!allows_navigation("https://example.com/"));
        assert!(!allows_navigation("http://example.com/"));
        assert!(!allows_navigation("javascript:alert(1)"));
        assert!(allows_navigation("FILE:///upper/case"));
        assert!(!allows_navigation("relative/path.html"));
        assert!(!allows_navigation(""));
    }

    #[test]
    fn preload_renders_session_token_and_includes_engine_gap_shims() {
        let rendered = render_preload_script("deadbeef1234");
        assert!(!rendered.contains(TOKEN_PLACEHOLDER));
        assert!(rendered.contains("\"deadbeef1234\""));
        assert!(rendered.contains("Object.defineProperty(window, \"Draconium\""));
        assert!(rendered.contains("invokeAsync"));
        assert!(rendered.contains("__draconiumResolveAsync"));
        assert!(rendered.contains("showPopover"));
        assert!(!rendered.contains("createElement(\"style\")"));
        assert!(rendered.contains("-webkit-app-region"));
        assert!(rendered.contains("a[download][href]"));
        assert!(rendered.contains("safeStorage"));
        assert!(rendered.contains("clipboard"));
        assert!(rendered.contains("dn:file-drop"));
        assert!(rendered.contains("windowApi.exportPdf(title + \".pdf\")"));
    }

    #[test]
    fn js_string_literal_escapes() {
        assert_eq!(js_string_literal("plain"), "\"plain\"");
        assert_eq!(js_string_literal("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(js_string_literal("x\ny"), "\"x\\ny\"");
    }

    #[test]
    fn dispatch_and_resolve_async_script_shape() {
        let script = dispatch_script("window-moved", "{\"x\":10,\"y\":20}");
        assert!(script.starts_with("window.__draconiumDispatch"));
        assert!(script.contains("\"window-moved\""));
        assert!(script.contains("JSON.parse"));

        let async_script = resolve_async_script(9, true, "{\"ok\":1}", true);
        assert!(async_script.starts_with("window.__draconiumResolveAsync"));
        assert!(async_script.contains("9, true,"));
    }

    #[test]
    fn origin_matching_navigation_and_preload_guard() {
        // 1. dn://app/
        assert!(url_matches_initial_origin(
            "dn://app/other.html?v=1#top",
            "dn://app/workspace.html"
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/ui/workspace.html",
            "dn://app/workspace.html"
        ));
        assert!(!url_matches_initial_origin(
            "about:blank",
            "dn://app/workspace.html"
        ));
        assert!(allows_navigation_for_origin(
            "dn://app/other.html",
            "dn://app/workspace.html",
            None
        ));
        assert!(!allows_navigation_for_origin(
            "file:///C:/Users/Public/evil.html",
            "dn://app/workspace.html",
            None
        ));

        // 2. http(s):// dev origin (exact port normalization and boundary check)
        let dev = DevOrigin::parse("http://127.0.0.1:5173").unwrap();
        assert!(url_matches_initial_origin(
            "http://127.0.0.1:5173/sub/page.html",
            "http://127.0.0.1:5173/index.html"
        ));
        assert!(!url_matches_initial_origin(
            "http://127.0.0.1:51730/index.html",
            "http://127.0.0.1:5173/index.html"
        ));
        assert!(!url_matches_initial_origin(
            "http://localhost:5173/index.html",
            "http://127.0.0.1:5173/index.html"
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/ui/index.html",
            "http://127.0.0.1:5173/index.html"
        ));
        assert!(allows_navigation_for_origin(
            "http://127.0.0.1:5173/app.html",
            "http://127.0.0.1:5173/index.html",
            Some(&dev)
        ));
        assert!(!allows_navigation_for_origin(
            "file:///C:/app/ui/index.html",
            "http://127.0.0.1:5173/index.html",
            Some(&dev)
        ));

        // 3. file:// directory confinement (sibling, traversal, UNC, root safety)
        let win_init = "file:///C:/app/ui/workspace.html";
        assert!(url_matches_initial_origin(
            "file:///c:/App/UI/other.html#frag",
            win_init
        ));
        assert!(url_matches_initial_origin(
            "file://localhost/C:/app/ui/sub/panel.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/ui-evil/evil.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/secret.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/ui/%2e%2e/secret.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "file:///C:/app/ui/a%2f..%2fsecret.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "file://attacker/share/workspace.html",
            win_init
        ));
        assert!(!url_matches_initial_origin(
            "dn://app/workspace.html",
            win_init
        ));
        assert!(allows_navigation_for_origin(
            "file:///C:/app/ui/other.html",
            win_init,
            None
        ));
        assert!(!allows_navigation_for_origin(
            "file:///C:/Users/Downloads/evil.html",
            win_init,
            None
        ));

        let unix_init = "file:///home/alice/app/ui/index.html";
        assert!(url_matches_initial_origin(
            "file:///home/alice/app/ui/panel.html",
            unix_init
        ));
        assert!(!url_matches_initial_origin(
            "file:///home/alice/app/UI/panel.html",
            unix_init
        ));

        // 4. Preload script guards
        let dn_preload = render_preload_script_for_origin("tok123", "dn://app/workspace.html");
        assert!(dn_preload.contains("window.top !== window"));
        assert!(dn_preload.contains("dn:\\/\\/app"));
        assert!(dn_preload.contains("\"tok123\""));

        let file_preload = render_preload_script_for_origin("tok456", win_init);
        assert!(file_preload.contains("\"/c:/app/ui/\""));
        assert!(file_preload.contains("\"tok456\""));

        let http_preload =
            render_preload_script_for_origin("tok789", "http://127.0.0.1:5173/index.html");
        assert!(http_preload.contains("\"5173\""));
        assert!(http_preload.contains("\"tok789\""));

        let blank_preload = render_preload_script_for_origin("secret_tok", "about:blank");
        assert!(!blank_preload.contains("secret_tok"));

        // 5. HTTP URL suffixes containing '@' (query/fragment) stay within origin while userinfo is rejected
        assert!(dev.matches_url("http://127.0.0.1:5173/notes?email=a@b.com#sec@1"));
        assert!(url_matches_initial_origin(
            "http://127.0.0.1:5173/notes?email=a@b.com#sec@1",
            "http://127.0.0.1:5173/index.html"
        ));
        assert!(!dev.matches_url("http://user@127.0.0.1:5173/index.html"));
        assert!(!url_matches_initial_origin(
            "http://user@127.0.0.1:5173/index.html",
            "http://127.0.0.1:5173/index.html"
        ));

        // 6. Non-ASCII Unicode case folding parity on Windows paths
        let unicode_win = "file:///C:/Users/ÄÖÜ/Workspace/index.html";
        assert!(url_matches_initial_origin(
            "file:///c:/users/äöü/workspace/panel.html",
            unicode_win
        ));
        let unicode_preload = render_preload_script_for_origin("toku", unicode_win);
        assert!(unicode_preload.contains("\"/c:/users/äöü/workspace/\""));

        // 7. Strict origin navigation blocks cross-origin jumps & canonical fs check verifies real paths
        assert!(!allows_navigation_for_origin(
            "about:blank",
            "dn://app/workspace.html",
            None
        ));
        assert!(!allows_navigation_for_origin(
            "dn://app/workspace.html",
            win_init,
            None
        ));
        assert!(allows_child_window_url(
            "dn://app/child.html",
            win_init,
            None,
            None
        ));
        assert!(allows_child_window_url(
            "about:blank",
            "dn://app/workspace.html",
            None,
            None
        ));

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fs_root = std::env::temp_dir().join(format!("dn-origin-fs-{stamp}"));
        let ui_dir = fs_root.join("ui");
        let outside_dir = fs_root.join("outside");
        std::fs::create_dir_all(&ui_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        let entry = ui_dir.join("app.html");
        let sibling = ui_dir.join("panel.html");
        let outside = outside_dir.join("secret.html");
        std::fs::write(&entry, b"entry").unwrap();
        std::fs::write(&sibling, b"sibling").unwrap();
        std::fs::write(&outside, b"secret").unwrap();

        let to_file_url = |p: &std::path::Path| -> String {
            let norm = p.to_string_lossy().replace('\\', "/");
            if norm.starts_with('/') {
                format!("file://{norm}")
            } else {
                format!("file:///{norm}")
            }
        };
        let entry_url = to_file_url(&entry);
        let sibling_url = to_file_url(&sibling);
        let outside_url = to_file_url(&outside);
        let resolved_dir = resolve_initial_file_dir(&entry_url).expect("resolve ui_dir");
        assert!(allows_navigation_for_origin_with_fs(
            &sibling_url,
            &entry_url,
            Some(&resolved_dir),
            None
        ));
        assert!(!allows_navigation_for_origin_with_fs(
            &outside_url,
            &entry_url,
            Some(&resolved_dir),
            None
        ));
        assert!(!file_url_is_contained(&to_file_url(&ui_dir), &resolved_dir));
        let _ = std::fs::remove_dir_all(&fs_root);
    }
}

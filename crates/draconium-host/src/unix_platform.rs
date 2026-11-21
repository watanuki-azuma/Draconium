/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Cross-platform OS integration layer (`macos` / `linux`) and runtime
//! [`PlatformCapabilities`] introspection for `draconium-host`.
//!
//! All formatting, escaping, parsing, and capability metadata functions are
//! compiled and unit-tested on every host OS (including Windows) via
//! `rustc --edition 2024 --test -D warnings crates/draconium-host/src/unix_platform.rs`.
//! Native OS command/IPC backends under `#[cfg(target_os = "macos")]`,
//! `#[cfg(target_os = "linux")]`, and `#[cfg(unix)]` require zero external
//! crates beyond Rust `std`.

use std::path::{Path, PathBuf};

/// Runtime capability matrix describing which native OS host features are
/// implemented on the current platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformCapabilities {
    pub os: &'static str,
    pub native_dialogs: bool,
    pub clipboard_text: bool,
    pub clipboard_html: bool,
    pub clipboard_image: bool,
    pub notifications: bool,
    pub autostart: bool,
    pub single_instance: bool,
    pub system_theme: bool,
    pub external_url: bool,
    pub system_tray: bool,
    pub global_hotkey: bool,
    pub safe_storage: bool,
    pub window_opacity: bool,
    pub click_through: bool,
    pub custom_uri_scheme: bool,
    pub media_gstreamer: bool,
}

impl PlatformCapabilities {
    /// Returns the native capability matrix for the compiled target OS.
    pub const fn for_current_os() -> Self {
        let mut capabilities = {
            #[cfg(target_os = "windows")]
            {
                Self::windows()
            }
            #[cfg(target_os = "macos")]
            {
                Self::macos()
            }
            #[cfg(target_os = "linux")]
            {
                Self::linux()
            }
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
            {
                Self::other_unix()
            }
            #[cfg(not(any(target_os = "windows", unix)))]
            {
                Self::other()
            }
        };
        capabilities.media_gstreamer = cfg!(feature = "media-gstreamer");
        capabilities
    }

    /// Capability matrix for Windows (`win32` FFI backend).
    pub const fn windows() -> Self {
        Self {
            os: "windows",
            native_dialogs: true,
            clipboard_text: true,
            clipboard_html: true,
            clipboard_image: true,
            notifications: true,
            autostart: true,
            single_instance: true,
            system_theme: true,
            external_url: true,
            system_tray: true,
            global_hotkey: true,
            safe_storage: true,
            window_opacity: true,
            click_through: true,
            custom_uri_scheme: true,
            media_gstreamer: false,
        }
    }

    /// Capability matrix for macOS (`osascript` / `pbcopy` / `LaunchAgents` / Unix socket backend).
    pub const fn macos() -> Self {
        Self {
            os: "macos",
            native_dialogs: true,
            clipboard_text: true,
            clipboard_html: true,
            clipboard_image: true,
            notifications: true,
            autostart: true,
            single_instance: true,
            system_theme: true,
            external_url: true,
            system_tray: false,
            global_hotkey: false,
            safe_storage: false,
            window_opacity: false,
            click_through: false,
            custom_uri_scheme: false,
            media_gstreamer: false,
        }
    }

    /// Capability matrix for Linux (`zenity`/`kdialog` / `wl-copy`/`xclip` / XDG autostart / Unix socket backend).
    pub const fn linux() -> Self {
        Self {
            os: "linux",
            native_dialogs: true,
            clipboard_text: true,
            clipboard_html: true,
            clipboard_image: true,
            notifications: true,
            autostart: true,
            single_instance: true,
            system_theme: true,
            external_url: true,
            system_tray: false,
            global_hotkey: false,
            safe_storage: false,
            window_opacity: false,
            click_through: false,
            custom_uri_scheme: true,
            media_gstreamer: false,
        }
    }

    /// Capability matrix for other Unix targets (Unix domain socket single-instance only).
    pub const fn other_unix() -> Self {
        Self {
            media_gstreamer: false,
            single_instance: true,
            ..Self::other()
        }
    }

    /// Conservative fallback for unsupported targets.
    pub const fn other() -> Self {
        Self {
            os: "other",
            native_dialogs: false,
            clipboard_text: false,
            clipboard_html: false,
            clipboard_image: false,
            notifications: false,
            autostart: false,
            single_instance: false,
            system_theme: false,
            external_url: false,
            system_tray: false,
            global_hotkey: false,
            safe_storage: false,
            window_opacity: false,
            click_through: false,
            custom_uri_scheme: false,
            media_gstreamer: false,
        }
    }

    /// Serializes this capability matrix to a JSON object string for `Draconium.window.getPlatform()`.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                r#"{{"os":"{}","nativeDialogs":{},"clipboardText":{},"clipboardHtml":{},"#,
                r#""clipboardImage":{},"notifications":{},"autostart":{},"singleInstance":{},"#,
                r#""systemTheme":{},"externalUrl":{},"systemTray":{},"globalHotkey":{},"#,
                r#""safeStorage":{},"windowOpacity":{},"clickThrough":{},"customUriScheme":{},"mediaGstreamer":{}}}"#
            ),
            self.os,
            self.native_dialogs,
            self.clipboard_text,
            self.clipboard_html,
            self.clipboard_image,
            self.notifications,
            self.autostart,
            self.single_instance,
            self.system_theme,
            self.external_url,
            self.system_tray,
            self.global_hotkey,
            self.safe_storage,
            self.window_opacity,
            self.click_through,
            self.custom_uri_scheme,
            self.media_gstreamer
        )
    }
}

/// Returns the compiled target OS's native host capability matrix.
pub const fn platform_capabilities() -> PlatformCapabilities {
    PlatformCapabilities::for_current_os()
}

// ---------------------------------------------------------------------------
// Pure cross-platform escaping, formatting, and parsing helpers
// (always compiled & unit-tested on all platforms including Windows)
// ---------------------------------------------------------------------------

/// Sanitizes an arbitrary app name or mutex/instance tag into a safe ASCII
/// identifier suitable for Unix socket names, `.desktop` filenames, and macOS
/// LaunchAgent labels.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn sanitize_unix_identifier(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_') {
            out.push(ch);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 72 {
            break;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.').to_owned();
    if trimmed.is_empty() {
        "draconium".to_owned()
    } else {
        trimmed
    }
}

/// Extracts and validates the dimensions from a PNG IHDR chunk.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub fn parse_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?) != 13
        || bytes.get(12..16)? != b"IHDR"
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 64_000_000 {
        return None;
    }
    Some((width, height))
}

/// Compact FNV-1a 32-bit hash for Unix instance and XDG handler filenames.
#[cfg_attr(not(unix), allow(dead_code))]
fn fnv1a_32(input: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in input.as_bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg_attr(not(unix), allow(dead_code))]
fn instance_path(dir: &Path, tag: &str, suffix: &str) -> PathBuf {
    let sanitized = sanitize_unix_identifier(tag);
    let prefix: String = sanitized.chars().take(12).collect();
    dir.join(format!("dn-{prefix}-{:08x}{suffix}", fnv1a_32(tag)))
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
const DIALOG_PATH_SEPARATOR: char = '\x1e';

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn strip_dialog_newline(output: &str) -> &str {
    match output.strip_suffix('\n') {
        Some(line) => line.strip_suffix('\r').unwrap_or(line),
        None => output,
    }
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn parse_dialog_path(output: &str) -> Option<PathBuf> {
    let path = strip_dialog_newline(output);
    (!path.is_empty() && !path.contains('\0')).then(|| PathBuf::from(path))
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn parse_open_dialog_paths(output: &str, multiple: bool) -> Option<Vec<PathBuf>> {
    if !multiple {
        return parse_dialog_path(output).map(|path| vec![path]);
    }
    output
        .strip_suffix('\n')
        .unwrap_or(output)
        .split(DIALOG_PATH_SEPARATOR)
        .map(|path| {
            (path.starts_with('/') && !path.chars().any(char::is_control))
                .then(|| PathBuf::from(path))
        })
        .collect()
}

/// Escapes a Rust string into a double-quoted AppleScript string literal,
/// stripping NUL bytes and escaping `\` / `"` / control newlines.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn escape_applescript_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '\0' => {}
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' => {}
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Builds a safe AppleScript for `display alert`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_alert_script(title: &str, message: &str) -> String {
    format!(
        "display alert {} message {} buttons {{\"OK\"}} default button \"OK\"",
        escape_applescript_string(title),
        escape_applescript_string(message)
    )
}

/// Builds a safe AppleScript for an OK/Cancel confirmation dialog.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_confirm_script(title: &str, message: &str) -> String {
    format!(
        "display dialog {} with title {} buttons {{\"Cancel\", \"OK\"}} default button \"OK\" cancel button \"Cancel\"",
        escape_applescript_string(message),
        escape_applescript_string(title)
    )
}

/// Builds a safe AppleScript for `choose file` returning POSIX paths separated by ASCII Record Separator.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_open_file_script(extensions: &[String], allow_multiple: bool) -> String {
    let safe_exts: Vec<String> = extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| escape_applescript_string(&e))
        .collect();

    let of_type = if safe_exts.is_empty() {
        String::new()
    } else {
        format!(" of type {{{}}}", safe_exts.join(", "))
    };

    if allow_multiple {
        format!(
            "set chosen to choose file{of_type} with multiple selections allowed\n\
             set pathSeparator to character id 30\n\
             set paths to {{}}\n\
             repeat with f in chosen\n\
             set chosenPath to POSIX path of f\n\
             if chosenPath contains pathSeparator then error \"Invalid file path\"\n\
             set end of paths to chosenPath\n\
             end repeat\n\
             set AppleScript's text item delimiters to pathSeparator\n\
             return paths as text"
        )
    } else {
        format!("return POSIX path of (choose file{of_type})")
    }
}

/// Builds a safe AppleScript for `choose file name` (Save dialog).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_save_file_script(default_name: &str) -> String {
    if default_name.trim().is_empty() {
        "return POSIX path of (choose file name)".to_owned()
    } else {
        format!(
            "return POSIX path of (choose file name default name {})",
            escape_applescript_string(default_name.trim())
        )
    }
}

/// Builds a safe AppleScript for `choose folder`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_select_folder_script(title: &str) -> String {
    if title.trim().is_empty() {
        "return POSIX path of (choose folder)".to_owned()
    } else {
        format!(
            "return POSIX path of (choose folder with prompt {})",
            escape_applescript_string(title.trim())
        )
    }
}

/// Builds a safe AppleScript for `choose color` using macOS 16-bit RGB components.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_color_picker_script(initial: (u8, u8, u8)) -> String {
    let r = u16::from(initial.0) * 257;
    let g = u16::from(initial.1) * 257;
    let b = u16::from(initial.2) * 257;
    format!(
        "set c to choose color default color {{{r}, {g}, {b}}}\n\
         return (item 1 of c as text) & \",\" & (item 2 of c as text) & \",\" & (item 3 of c as text)"
    )
}

/// Parses AppleScript `choose color` output (`"65535,32768,0"` or `"65535, 32768, 0"`)
/// into 8-bit `(r, g, b)`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_macos_color_rgb(output: &str) -> Option<(u8, u8, u8)> {
    let mut parts = output.trim().split(',');
    let r16: u16 = parts.next()?.trim().parse().ok()?;
    let g16: u16 = parts.next()?.trim().parse().ok()?;
    let b16: u16 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(((r16 / 257) as u8, (g16 / 257) as u8, (b16 / 257) as u8))
}

/// Builds a safe AppleScript for macOS `display notification`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_macos_notification_script(title: &str, body: &str) -> String {
    format!(
        "display notification {} with title {}",
        escape_applescript_string(body),
        escape_applescript_string(title)
    )
}

/// Parses `defaults read -g AppleInterfaceStyle` output (`"Dark"` -> `Some(true)`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_macos_dark_mode(output: &str) -> Option<bool> {
    Some(output.trim().eq_ignore_ascii_case("dark"))
}

/// Escapes text for XML element bodies (`<string>...</string>`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn escape_xml_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// Derives a reverse-DNS macOS LaunchAgent label from `app_name`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn macos_launch_agent_label(app_name: &str) -> String {
    format!("io.draconium.{}", sanitize_unix_identifier(app_name))
}

/// Builds a macOS `LaunchAgents` `.plist` XML file for user login autostart.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn build_launch_agent_plist(label: &str, program_args: &[String]) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    out.push_str("  <key>Label</key>\n  <string>");
    out.push_str(&escape_xml_text(label));
    out.push_str("</string>\n  <key>ProgramArguments</key>\n  <array>\n");
    for arg in program_args {
        out.push_str("    <string>");
        out.push_str(&escape_xml_text(arg));
        out.push_str("</string>\n");
    }
    out.push_str("  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n");
    out
}

/// Builds a safe `zenity --file-filter` specification from an extension list.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn build_zenity_file_filter(extensions: &[String]) -> Option<String> {
    let globs: Vec<String> = extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!("*.{e}"))
        .collect();
    if globs.is_empty() {
        None
    } else {
        Some(format!("Supported files | {}", globs.join(" ")))
    }
}

/// Parses `zenity --color-selection` / `kdialog --getcolor` output
/// (`rgb(r,g,b)`, `rgba(r,g,b,a)`, or `#rrggbb`) into `(r, g, b)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_zenity_color_rgb(output: &str) -> Option<(u8, u8, u8)> {
    let trimmed = output.trim();
    if let Some(hex) = trimmed.strip_prefix('#')
        && hex.len() == 6
    {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some((r, g, b));
    }
    let inner = trimmed
        .strip_prefix("rgb(")
        .or_else(|| trimmed.strip_prefix("rgba("))?
        .strip_suffix(')')?;
    let mut parts = inner.split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    Some((r, g, b))
}

/// Parses Linux desktop theme signals (`gsettings color-scheme` and `gtk-theme`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_linux_dark_mode(color_scheme: &str, gtk_theme: &str) -> Option<bool> {
    let scheme = color_scheme.trim().trim_matches('\'').to_ascii_lowercase();
    if scheme.contains("prefer-dark") {
        return Some(true);
    }
    if scheme.contains("prefer-light") {
        return Some(false);
    }
    let theme = gtk_theme.trim().trim_matches('\'').to_ascii_lowercase();
    if !theme.is_empty() {
        if theme.ends_with("-dark") || theme.ends_with(":dark") || theme.contains("dark") {
            return Some(true);
        }
        return Some(false);
    }
    if scheme == "default" {
        return Some(false);
    }
    None
}

/// Escapes a single argument for the Freedesktop Desktop Entry `Exec=` key
/// (quoting reserved characters and escaping `"`, `` ` ``, `$`, `\`, and `%`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn format_xdg_exec_arg(raw: &str) -> Result<String, String> {
    if raw.contains(['\0', '\n', '\r']) {
        return Err("Desktop Exec arguments cannot contain NUL or line breaks".to_owned());
    }
    let needs_quotes =
        raw.is_empty() || raw.chars().any(|c| " \t\"'\\><~|&;$*?#()\x60=".contains(c));
    let mut out = String::with_capacity(raw.len() + 4);
    if needs_quotes {
        out.push('"');
    }
    // Desktop Entry string unescaping runs before Exec argument unescaping.
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\\\"),
            '"' | '\x60' | '$' => {
                out.push_str(r"\\");
                out.push(ch);
            }
            '\t' => out.push_str(r"\t"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    if needs_quotes {
        out.push('"');
    }
    Ok(out)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn escape_xdg_name(raw: &str) -> Result<String, String> {
    if raw.contains(['\0', '\n', '\r']) {
        return Err("Desktop Name cannot contain NUL or line breaks".to_owned());
    }
    Ok(raw.trim().replace('\\', r"\\").replace('\t', r"\t"))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn format_xdg_exec(exe: &Path, args: &[&str]) -> Result<String, String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| "Desktop Exec path must be valid UTF-8".to_owned())?;
    let mut exec = format_xdg_exec_arg(exe)?;
    for arg in args {
        exec.push(' ');
        exec.push_str(&format_xdg_exec_arg(arg)?);
    }
    Ok(exec)
}

/// Returns the desktop-entry filename used to register a protocol handler.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn xdg_protocol_handler_filename(scheme: &str) -> String {
    format!(
        "dn-scheme-{}-{:08x}.desktop",
        sanitize_unix_identifier(scheme),
        fnv1a_32(scheme)
    )
}

/// Builds an XDG desktop entry that launches an application for a URI scheme.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn build_xdg_protocol_handler_desktop(
    app_name: &str,
    scheme: &str,
    exe: &Path,
    args: &[&str],
) -> Result<String, String> {
    let name = escape_xdg_name(app_name)?;
    let exec = format_xdg_exec(exe, args)?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={name}\n\
         Exec={exec} --deep-link %u\n\
         Terminal=false\n\
         NoDisplay=true\n\
         MimeType=x-scheme-handler/{scheme};\n"
    ))
}

/// Builds an XDG Autostart desktop entry for Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn build_xdg_autostart_desktop(
    app_name: &str,
    exe: &Path,
    args: &[&str],
) -> Result<String, String> {
    let name = escape_xdg_name(app_name)?;
    let exec = format_xdg_exec(exe, args)?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={name}\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn remove_xdg_mime_handler(contents: &str, scheme: &str, filename: &str) -> String {
    let key = format!("x-scheme-handler/{scheme}");
    let mut updated = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        let value_line = strip_dialog_newline(line);
        if let Some((entry, handlers)) = value_line.split_once('=')
            && entry.trim() == key
            && handlers
                .split(';')
                .any(|handler| handler.trim() == filename)
        {
            let remaining: Vec<_> = handlers
                .split(';')
                .filter(|handler| handler.trim() != filename)
                .collect();
            if remaining.iter().any(|handler| !handler.trim().is_empty()) {
                updated.push_str(entry);
                updated.push('=');
                updated.push_str(&remaining.join(";"));
                updated.push_str(&line[value_line.len()..]);
            }
        } else {
            updated.push_str(line);
        }
    }
    updated
}

/// Hex-encodes UTF-8 bytes for macOS AppleScript `«data HTML...»` clipboard writes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn encode_hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Decodes an even-length ASCII hexadecimal string into bytes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn decode_hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        if !pair[0].is_ascii_hexdigit() || !pair[1].is_ascii_hexdigit() {
            return None;
        }
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        decoded.push((high << 4) | low);
    }
    Some(decoded)
}

// ---------------------------------------------------------------------------
// Unix runtime backends (macOS & Linux) — compiled only on Unix targets
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod runtime {
    use super::*;
    use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
    use std::io::{self, ErrorKind, Read, Write};
    use std::os::unix::fs::{
        DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
    };
    use std::os::unix::net::{UnixListener, UnixStream};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const MAX_UNIX_IPC_PAYLOAD_BYTES: usize = 65_536;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn run_command_stdout(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

    #[cfg(target_os = "macos")]
    const MAX_CLIPBOARD_HEX_BYTES: usize = 2 * MAX_CLIPBOARD_BYTES + 64;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn read_bounded_stdout(
        command: &mut Command,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        let program = command.get_program().to_string_lossy().into_owned();
        let mut child = match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("Could not start {program}: {err}")),
        };
        let mut output = Vec::new();
        let read = child
            .stdout
            .take()
            .expect("piped stdout")
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut output);
        if read.is_err() || output.len() > max_bytes {
            let _ = child.kill();
            let _ = child.wait();
            read.map_err(|err| format!("Could not read {program} output: {err}"))?;
            return Err(format!(
                "{program} clipboard output exceeds {max_bytes} bytes"
            ));
        }
        let status = child
            .wait()
            .map_err(|err| format!("Could not wait for {program}: {err}"))?;
        Ok(status.success().then_some(output))
    }

    #[cfg(target_os = "linux")]
    fn read_linux_clipboard(
        readers: &[(&str, &[&str])],
        allow_empty: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        for &(program, args) in readers {
            if let Some(bytes) =
                read_bounded_stdout(Command::new(program).args(args), MAX_CLIPBOARD_BYTES)?
                && (allow_empty || !bytes.is_empty())
            {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    fn write_linux_clipboard(program: &str, args: &[&str], input: &[u8]) -> Result<(), String> {
        // Clipboard owners can fork. Their inherited handles must not keep pipes open.
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("Could not start {program}: {err}"))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let write = stdin.write_all(input);
        drop(stdin);
        if write.is_err() {
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|err| format!("Could not wait for {program}: {err}"))?;
        write.map_err(|err| format!("Could not write to {program}: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{program} failed ({status})"))
        }
    }

    #[cfg(target_os = "macos")]
    fn run_command_with_stdin(program: &str, args: &[&str], input: &[u8]) -> Result<(), String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("{program} 起動失敗: {err}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .map_err(|err| format!("{program} 書込失敗: {err}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("{program} 終了待機失敗: {err}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(format!("{program} 失敗: {err}"))
        }
    }

    #[cfg(target_os = "linux")]
    pub fn xdg_applications_dir() -> Result<PathBuf, String> {
        let data_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .ok_or_else(|| "XDG_DATA_HOME または HOME 環境変数が必要です".to_owned())?;
        Ok(data_dir.join("applications"))
    }

    #[cfg(target_os = "linux")]
    fn update_xdg_desktop_database(applications_dir: &Path) {
        let _ = Command::new("update-desktop-database")
            .arg(applications_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(target_os = "linux")]
    pub fn register_uri_scheme_args(
        scheme: &str,
        app_name: &str,
        exe: &Path,
        args: &[&str],
    ) -> Result<(), String> {
        let desktop = build_xdg_protocol_handler_desktop(app_name, scheme, exe, args)?;
        let applications_dir = xdg_applications_dir()?;
        std::fs::create_dir_all(&applications_dir)
            .map_err(|err| format!("XDG applications ディレクトリを作成できません: {err}"))?;
        let filename = xdg_protocol_handler_filename(scheme);
        let desktop_path = applications_dir.join(&filename);
        std::fs::write(&desktop_path, desktop.as_bytes())
            .map_err(|err| format!("XDG URI ハンドラーを書き込めません: {err}"))?;

        let mime_type = format!("x-scheme-handler/{scheme}");
        match Command::new("xdg-mime")
            .arg("default")
            .arg(&filename)
            .arg(mime_type)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => return Err(format!("xdg-mime default failed ({status})")),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Could not run xdg-mime: {err}")),
        }
        update_xdg_desktop_database(&applications_dir);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn unregister_uri_scheme(scheme: &str) -> Result<(), String> {
        let applications_dir = xdg_applications_dir()?;
        let filename = xdg_protocol_handler_filename(scheme);
        let desktop_path = applications_dir.join(&filename);
        match std::fs::remove_file(&desktop_path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(format!("XDG URI ハンドラーを削除できません: {err}")),
        }
        let mimeapps = xdg_config_dir()?.join("mimeapps.list");
        match fs::read_to_string(&mimeapps) {
            Ok(contents) => {
                let updated = remove_xdg_mime_handler(&contents, scheme, &filename);
                if updated != contents {
                    fs::write(&mimeapps, updated)
                        .map_err(|err| format!("Could not update {}: {err}", mimeapps.display()))?;
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Could not read {}: {err}", mimeapps.display())),
        }
        update_xdg_desktop_database(&applications_dir);
        Ok(())
    }

    fn current_uid() -> io::Result<u32> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        for attempt in 0..16 {
            let path = dir.join(format!("dn-uid-{pid}-{}-{attempt}", timestamp.as_nanos()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    let meta = file.metadata();
                    fs::remove_file(path)?;
                    return meta.map(|meta| meta.uid());
                }
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "Could not create UID probe",
        ))
    }

    fn verify_private_dir(path: &Path, uid: u32) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || meta.mode() & 0o777 != 0o700
            || meta.uid() != uid
        {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{} must be a 0700 directory owned by UID {uid}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    fn instance_dir(tag: &str, uid: u32, runtime_dir: Option<PathBuf>) -> io::Result<PathBuf> {
        if let Some(path) = runtime_dir.filter(|dir| {
            dir.is_absolute() && instance_path(dir, tag, ".sock").as_os_str().len() <= 96
        }) {
            verify_private_dir(&path, uid)?;
            return Ok(path);
        }

        // Use a short, user-owned location even when TMPDIR is too long for sun_path.
        let path = PathBuf::from(format!("/tmp/dn-{uid}"));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        verify_private_dir(&path, uid)?;
        Ok(path)
    }

    fn instance_dir_from_env(tag: &str) -> Result<PathBuf, String> {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        current_uid()
            .and_then(|uid| instance_dir(tag, uid, runtime_dir))
            .map_err(|err| format!("Could not initialize private Unix IPC directory: {err}"))
    }

    pub fn unix_instance_socket_path(tag: &str) -> Result<PathBuf, String> {
        Ok(instance_path(&instance_dir_from_env(tag)?, tag, ".sock"))
    }

    pub fn unix_instance_lock_path(tag: &str) -> Result<PathBuf, String> {
        Ok(instance_path(&instance_dir_from_env(tag)?, tag, ".lock"))
    }

    fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let file = match options.clone().create_new(true).mode(0o600).open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(path)?.is_file() {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "Lock path is not a regular file",
                    ));
                }
                options.open(path)?
            }
            Err(err) => return Err(err),
        };
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }

    fn bind_ipc_socket(path: &Path) -> io::Result<UnixListener> {
        // The caller holds the IPC lock throughout bind, recovery, and shutdown.
        // Probe an existing socket to leave live listeners from older hosts alone.
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == ErrorKind::AddrInUse => {
                if !fs::symlink_metadata(path)?.file_type().is_socket() {
                    return Err(err);
                }
                match UnixStream::connect(path) {
                    Err(probe) if probe.kind() == ErrorKind::ConnectionRefused => {
                        fs::remove_file(path)?;
                        UnixListener::bind(path)?
                    }
                    _ => return Err(err),
                }
            }
            Err(err) => return Err(err),
        };
        if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .and_then(|()| listener.set_nonblocking(true))
        {
            let _ = fs::remove_file(path);
            return Err(err);
        }
        Ok(listener)
    }

    /// An OS-held advisory lock. Leave the lock file in place so every
    /// process locks the same inode, including after a crash or forced exit.
    pub struct UnixSingleInstanceLock {
        _file: File,
    }

    impl UnixSingleInstanceLock {
        pub fn acquire(name: &str) -> Result<Option<Self>, String> {
            let path = unix_instance_lock_path(name)?;
            try_lock_file(&path)
                .map(|file| file.map(|file| Self { _file: file }))
                .map_err(|err| format!("Could not lock {}: {err}", path.display()))
        }
    }

    /// Background Unix domain socket listener receiving second-instance payloads.
    pub struct UnixInstanceServer {
        _lock: File,
        socket_path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl UnixInstanceServer {
        pub fn start(
            instance_tag: &str,
            on_payload: impl Fn(String) + Send + 'static,
        ) -> Result<Self, String> {
            let path = unix_instance_socket_path(instance_tag)?;
            let lock = try_lock_file(&path.with_extension("ipc.lock"))
                .map_err(|err| format!("Could not lock Unix IPC socket: {err}"))?
                .ok_or_else(|| "Unix IPC socket is already in use".to_owned())?;
            let listener = bind_ipc_socket(&path)
                .map_err(|err| format!("Could not bind Unix IPC socket: {err}"))?;
            let stop = Arc::new(AtomicBool::new(false));
            let stop_bg = Arc::clone(&stop);
            let thread = thread::Builder::new()
                .name("dn-unix-ipc".to_owned())
                .spawn(move || {
                    while !stop_bg.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                                let mut buf = Vec::new();
                                let mut limited =
                                    stream.take((MAX_UNIX_IPC_PAYLOAD_BYTES + 1) as u64);
                                if limited.read_to_end(&mut buf).is_ok()
                                    && buf.len() <= MAX_UNIX_IPC_PAYLOAD_BYTES
                                    && let Ok(text) = String::from_utf8(buf)
                                    && !text.is_empty()
                                {
                                    on_payload(text);
                                }
                            }
                            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(80));
                            }
                            Err(_) => {
                                thread::sleep(Duration::from_millis(120));
                            }
                        }
                    }
                })
                .map_err(|err| {
                    let _ = fs::remove_file(&path);
                    format!("Could not start Unix IPC thread: {err}")
                })?;
            Ok(Self {
                _lock: lock,
                socket_path: path,
                stop,
                thread: Some(thread),
            })
        }
    }

    impl Drop for UnixInstanceServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = UnixStream::connect(&self.socket_path);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    pub fn forward_to_running_instance(instance_tag: &str, json_payload: &str) -> bool {
        if json_payload.is_empty() || json_payload.len() > MAX_UNIX_IPC_PAYLOAD_BYTES {
            return false;
        }
        let Ok(path) = unix_instance_socket_path(instance_tag) else {
            return false;
        };
        // Retry briefly in case the primary instance acquired the advisory lock
        // moments ago and is still binding the IPC listener socket.
        let mut stream = None;
        for attempt in 0..10 {
            match UnixStream::connect(&path) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) && attempt < 9 =>
                {
                    thread::sleep(Duration::from_millis(30));
                }
                Err(_) => return false,
            }
        }
        let Some(mut stream) = stream else {
            return false;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        stream.write_all(json_payload.as_bytes()).is_ok()
    }

    #[cfg(target_os = "macos")]
    pub fn open_external_url(url: &str) -> Result<(), String> {
        let status = Command::new("open")
            .args(["--", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| format!("open 起動失敗: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("外部 URL の起動に失敗しました (status={status})"))
        }
    }

    #[cfg(target_os = "linux")]
    pub fn open_external_url(url: &str) -> Result<(), String> {
        let status = Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| format!("xdg-open 起動失敗: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("外部 URL の起動に失敗しました (status={status})"))
        }
    }

    #[cfg(target_os = "macos")]
    pub fn show_alert(title: &str, message: &str) {
        let script = build_macos_alert_script(title, message);
        let _ = run_command_stdout("osascript", &["-e", &script]);
    }

    #[cfg(target_os = "linux")]
    pub fn show_alert(title: &str, message: &str) {
        let title_arg = format!("--title={title}");
        let text_arg = format!("--text={message}");
        if Command::new("zenity")
            .args(["--info", "--no-markup", &title_arg, &text_arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return;
        }
        let _ = Command::new("kdialog")
            .args(["--title", title, "--msgbox", message])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(target_os = "macos")]
    pub fn show_confirm(title: &str, message: &str) -> bool {
        let script = build_macos_confirm_script(title, message);
        run_command_stdout("osascript", &["-e", &script]).is_some()
    }

    #[cfg(target_os = "linux")]
    pub fn show_confirm(title: &str, message: &str) -> bool {
        let title_arg = format!("--title={title}");
        let text_arg = format!("--text={message}");
        if let Ok(status) = Command::new("zenity")
            .args(["--question", "--no-markup", &title_arg, &text_arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            return status.success();
        }
        Command::new("kdialog")
            .args(["--title", title, "--yesno", message])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[cfg(target_os = "macos")]
    pub fn show_open_file_dialog(
        extensions: &[String],
        allow_multiple: bool,
    ) -> Option<Vec<PathBuf>> {
        let script = build_macos_open_file_script(extensions, allow_multiple);
        let out = run_command_stdout("osascript", &["-e", &script])?;
        parse_open_dialog_paths(&out, allow_multiple)
    }

    #[cfg(target_os = "linux")]
    pub fn show_open_file_dialog(
        extensions: &[String],
        allow_multiple: bool,
    ) -> Option<Vec<PathBuf>> {
        let mut cmd = Command::new("zenity");
        cmd.arg("--file-selection");
        if allow_multiple {
            cmd.arg(format!("--separator={DIALOG_PATH_SEPARATOR}"))
                .arg("--multiple");
        }
        if let Some(filter) = build_zenity_file_filter(extensions) {
            cmd.arg(format!("--file-filter={filter}"));
        }
        let (out, multiple) = match cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) => (out, allow_multiple),
            Err(_) => (
                Command::new("kdialog")
                    .args(["--getopenfilename", "."])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()?,
                false,
            ),
        };
        if !out.status.success() {
            return None;
        }
        parse_open_dialog_paths(std::str::from_utf8(&out.stdout).ok()?, multiple)
    }

    #[cfg(target_os = "macos")]
    pub fn show_save_file_dialog(default_name: &str, _extensions: &[String]) -> Option<PathBuf> {
        let script = build_macos_save_file_script(default_name);
        let out = run_command_stdout("osascript", &["-e", &script])?;
        parse_dialog_path(&out)
    }

    #[cfg(target_os = "linux")]
    pub fn show_save_file_dialog(default_name: &str, extensions: &[String]) -> Option<PathBuf> {
        let mut cmd = Command::new("zenity");
        cmd.args(["--file-selection", "--save", "--confirm-overwrite"]);
        let trimmed_default = default_name.trim();
        if !trimmed_default.is_empty() {
            cmd.arg(format!("--filename={trimmed_default}"));
        }
        if let Some(filter) = build_zenity_file_filter(extensions) {
            cmd.arg(format!("--file-filter={filter}"));
        }
        let out = match cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) => out,
            Err(_) => {
                let target = if trimmed_default.is_empty() {
                    "."
                } else {
                    trimmed_default
                };
                Command::new("kdialog")
                    .args(["--getsavefilename", target])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()?
            }
        };
        if !out.status.success() {
            return None;
        }
        let text = std::str::from_utf8(&out.stdout).ok()?;
        parse_dialog_path(text)
    }

    #[cfg(target_os = "macos")]
    pub fn show_select_folder_dialog(title: &str) -> Option<PathBuf> {
        let script = build_macos_select_folder_script(title);
        let out = run_command_stdout("osascript", &["-e", &script])?;
        parse_dialog_path(&out)
    }

    #[cfg(target_os = "linux")]
    pub fn show_select_folder_dialog(title: &str) -> Option<PathBuf> {
        let mut cmd = Command::new("zenity");
        cmd.args(["--file-selection", "--directory"]);
        let trimmed_title = title.trim();
        if !trimmed_title.is_empty() {
            cmd.arg(format!("--title={trimmed_title}"));
        }
        let out = match cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) => out,
            Err(_) => {
                let mut kd = Command::new("kdialog");
                if !trimmed_title.is_empty() {
                    kd.args(["--title", trimmed_title]);
                }
                kd.args(["--getexistingdirectory", "."])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()?
            }
        };
        if !out.status.success() {
            return None;
        }
        let text = std::str::from_utf8(&out.stdout).ok()?;
        parse_dialog_path(text)
    }

    #[cfg(target_os = "macos")]
    pub fn show_color_picker(initial: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
        let script = build_macos_color_picker_script(initial);
        let out = run_command_stdout("osascript", &["-e", &script])?;
        parse_macos_color_rgb(&out)
    }

    #[cfg(target_os = "linux")]
    pub fn show_color_picker(initial: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
        let init_css = format!("rgb({},{},{})", initial.0, initial.1, initial.2);
        let out = match Command::new("zenity")
            .args(["--color-selection", &format!("--color={init_css}")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            Ok(out) => out,
            Err(_) => {
                let init_hex = format!("#{:02x}{:02x}{:02x}", initial.0, initial.1, initial.2);
                Command::new("kdialog")
                    .args(["--getcolor", "--default", &init_hex])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()?
            }
        };
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        parse_zenity_color_rgb(&text)
    }

    #[cfg(target_os = "macos")]
    pub fn show_notification(title: &str, body: &str) -> bool {
        let script = build_macos_notification_script(title, body);
        run_command_stdout("osascript", &["-e", &script]).is_some()
    }

    #[cfg(target_os = "linux")]
    pub fn show_notification(title: &str, body: &str) -> bool {
        Command::new("notify-send")
            .args(["--", title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[cfg(target_os = "macos")]
    pub fn detect_system_dark_mode() -> Option<bool> {
        let out = run_command_stdout("defaults", &["read", "-g", "AppleInterfaceStyle"])
            .unwrap_or_default();
        parse_macos_dark_mode(&out)
    }

    #[cfg(target_os = "linux")]
    pub fn detect_system_dark_mode() -> Option<bool> {
        let scheme = run_command_stdout(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "color-scheme"],
        )
        .unwrap_or_default();
        let gtk_theme = run_command_stdout(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "gtk-theme"],
        )
        .or_else(|| std::env::var("GTK_THEME").ok())
        .unwrap_or_default();
        parse_linux_dark_mode(&scheme, &gtk_theme)
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_read_text() -> Result<String, String> {
        let out = read_bounded_stdout(
            Command::new("pbpaste").env("LC_CTYPE", "UTF-8"),
            MAX_CLIPBOARD_BYTES,
        )?
        .ok_or_else(|| "pbpaste 実行失敗".to_owned())?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_read_text() -> Result<String, String> {
        let bytes = read_linux_clipboard(
            &[
                ("wl-paste", &["--no-newline"]),
                ("xclip", &["-o", "-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--output"]),
            ],
            true,
        )?
        .ok_or_else(|| "wl-paste / xclip / xsel が利用できません".to_owned())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_write_text(text: &str) -> Result<(), String> {
        let mut child = Command::new("pbcopy")
            .env("LC_CTYPE", "UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("pbcopy 起動失敗: {err}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|err| format!("pbcopy 書込失敗: {err}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("pbcopy 終了待機失敗: {err}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(format!("pbcopy 失敗: {err}"))
        }
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_write_text(text: &str) -> Result<(), String> {
        if write_linux_clipboard("wl-copy", &[], text.as_bytes()).is_ok() {
            return Ok(());
        }
        if write_linux_clipboard("xclip", &["-i", "-selection", "clipboard"], text.as_bytes())
            .is_ok()
        {
            return Ok(());
        }
        write_linux_clipboard("xsel", &["--clipboard", "--input"], text.as_bytes())
    }

    #[cfg(target_os = "macos")]
    fn read_macos_clipboard_data(script: &str, prefix: &str) -> Result<Option<Vec<u8>>, String> {
        let Some(raw) = read_bounded_stdout(
            Command::new("osascript").args(["-e", script]),
            MAX_CLIPBOARD_HEX_BYTES,
        )?
        else {
            return Ok(None);
        };
        let Ok(raw) = std::str::from_utf8(&raw) else {
            return Ok(None);
        };
        let Some(hex) = raw
            .trim()
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix('»'))
        else {
            return Ok(None);
        };
        if hex.len() > 2 * MAX_CLIPBOARD_BYTES {
            return Err("Clipboard data exceeds 16 MiB".to_owned());
        }
        Ok(decode_hex_bytes(hex))
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_read_html() -> Result<String, String> {
        Ok(
            read_macos_clipboard_data("the clipboard as «class HTML»", "«data HTML")?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default(),
        )
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_read_image_png() -> Result<Vec<u8>, String> {
        read_macos_clipboard_data("the clipboard as «class PNGf»", "«data PNGf")?
            .filter(|png| !png.is_empty())
            .ok_or_else(|| "クリップボードに画像がありません".to_owned())
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_write_image_png(png_bytes: &[u8]) -> Result<(), String> {
        let hex = encode_hex_upper(png_bytes);
        let script = format!("set the clipboard to «data PNGf{hex}»\n");
        run_command_with_stdin("osascript", &["-"], script.as_bytes())
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_read_image_png() -> Result<Vec<u8>, String> {
        read_linux_clipboard(
            &[
                ("wl-paste", &["--type", "image/png"]),
                (
                    "xclip",
                    &["-o", "-selection", "clipboard", "-t", "image/png"],
                ),
            ],
            false,
        )?
        .ok_or_else(|| "クリップボードに画像がありません".to_owned())
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_write_image_png(png_bytes: &[u8]) -> Result<(), String> {
        if write_linux_clipboard("wl-copy", &["--type", "image/png"], png_bytes).is_ok() {
            return Ok(());
        }
        write_linux_clipboard(
            "xclip",
            &["-i", "-selection", "clipboard", "-t", "image/png"],
            png_bytes,
        )
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_read_html() -> Result<String, String> {
        Ok(read_linux_clipboard(
            &[
                ("wl-paste", &["--type", "text/html"]),
                (
                    "xclip",
                    &["-o", "-selection", "clipboard", "-t", "text/html"],
                ),
            ],
            true,
        )?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default())
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_write_html(fragment: &str) -> Result<(), String> {
        let hex = encode_hex_upper(fragment.as_bytes());
        let script = format!("set the clipboard to «data HTML{hex}»\n");
        run_command_with_stdin("osascript", &["-"], script.as_bytes())
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_write_html(fragment: &str) -> Result<(), String> {
        if write_linux_clipboard("wl-copy", &["--type", "text/html"], fragment.as_bytes()).is_ok() {
            return Ok(());
        }
        write_linux_clipboard(
            "xclip",
            &["-i", "-selection", "clipboard", "-t", "text/html"],
            fragment.as_bytes(),
        )
    }

    #[cfg(target_os = "macos")]
    pub fn clipboard_clear() -> Result<(), String> {
        clipboard_write_text("")
    }

    #[cfg(target_os = "linux")]
    pub fn clipboard_clear() -> Result<(), String> {
        if Command::new("wl-copy")
            .arg("--clear")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Ok(());
        }
        clipboard_write_text("")
    }

    #[cfg(target_os = "macos")]
    fn autostart_file_path(app_name: &str) -> Result<PathBuf, String> {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "HOME 環境変数が未設定です".to_owned())?;
        let label = macos_launch_agent_label(app_name);
        Ok(PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist")))
    }

    #[cfg(target_os = "linux")]
    fn xdg_config_dir() -> Result<PathBuf, String> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .ok_or_else(|| "XDG_CONFIG_HOME or HOME must be set".to_owned())
    }

    #[cfg(target_os = "linux")]
    fn autostart_file_path(app_name: &str) -> Result<PathBuf, String> {
        let id = sanitize_unix_identifier(app_name);
        Ok(xdg_config_dir()?
            .join("autostart")
            .join(format!("{id}.desktop")))
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn autostart_is_enabled(app_name: &str) -> Result<bool, String> {
        if app_name.trim().is_empty() {
            return Err("アプリ名が空です".to_owned());
        }
        Ok(autostart_file_path(app_name)?.is_file())
    }

    #[cfg(target_os = "macos")]
    pub fn autostart_set_enabled_args(
        app_name: &str,
        enabled: bool,
        exe: &Path,
        args: &[&str],
    ) -> Result<(), String> {
        if app_name.trim().is_empty() {
            return Err("アプリ名が空です".to_owned());
        }
        let path = autostart_file_path(app_name)?;
        if !enabled {
            if path.is_file() {
                std::fs::remove_file(&path)
                    .map_err(|err| format!("LaunchAgent 削除失敗: {err}"))?;
            }
            return Ok(());
        }
        if exe.as_os_str().is_empty() {
            return Err("実行ファイルパスが空です".to_owned());
        }
        let mut program_args = Vec::with_capacity(1 + args.len());
        program_args.push(exe.to_string_lossy().into_owned());
        for &arg in args {
            program_args.push(arg.to_owned());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("LaunchAgents ディレクトリ作成失敗: {err}"))?;
        }
        let label = macos_launch_agent_label(app_name);
        let plist = build_launch_agent_plist(&label, &program_args);
        std::fs::write(&path, plist.as_bytes())
            .map_err(|err| format!("LaunchAgent 書込失敗: {err}"))
    }

    #[cfg(target_os = "linux")]
    pub fn autostart_set_enabled_args(
        app_name: &str,
        enabled: bool,
        exe: &Path,
        args: &[&str],
    ) -> Result<(), String> {
        if app_name.trim().is_empty() {
            return Err("アプリ名が空です".to_owned());
        }
        let path = autostart_file_path(app_name)?;
        if !enabled {
            if path.is_file() {
                std::fs::remove_file(&path)
                    .map_err(|err| format!("XDG autostart 削除失敗: {err}"))?;
            }
            return Ok(());
        }
        if exe.as_os_str().is_empty() {
            return Err("実行ファイルパスが空です".to_owned());
        }
        let entry = build_xdg_autostart_desktop(app_name, exe, args)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("XDG autostart ディレクトリ作成失敗: {err}"))?;
        }
        std::fs::write(&path, entry.as_bytes())
            .map_err(|err| format!("XDG autostart 書込失敗: {err}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ipc_dir_must_be_private_and_owned_by_the_current_uid() {
            use std::os::unix::fs::symlink;

            let uid = current_uid().unwrap();
            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let dir = std::env::temp_dir().join(format!(
                "dn-ipc-test-{}-{}",
                std::process::id(),
                timestamp.as_nanos()
            ));
            DirBuilder::new().mode(0o700).create(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            verify_private_dir(&dir, uid).unwrap();
            assert!(verify_private_dir(&dir, uid.wrapping_add(1)).is_err());

            let link = dir.join("link");
            symlink(&dir, &link).unwrap();
            assert!(verify_private_dir(&link, uid).is_err());
            fs::remove_file(link).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(verify_private_dir(&dir, uid).is_err());
            fs::remove_dir(dir).unwrap();

            let fallback = instance_dir("test", uid, None).unwrap();
            let overlong = Path::new("/tmp").join("x".repeat(100));
            assert_eq!(instance_dir("test", uid, Some(overlong)).unwrap(), fallback);
            verify_private_dir(&fallback, uid).unwrap();
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        #[test]
        fn bounded_stdout_checks_size_and_process_status() {
            assert_eq!(
                read_bounded_stdout(Command::new("sh").args(["-c", "printf 1234"]), 4).unwrap(),
                Some(b"1234".to_vec())
            );
            assert_eq!(
                read_bounded_stdout(Command::new("sh").args(["-c", "printf 1234; exit 1"]), 4)
                    .unwrap(),
                None
            );
            let mut oversized = Command::new("sh");
            oversized.args(["-c", "while :; do printf 123456789; done"]);
            assert!(
                read_bounded_stdout(&mut oversized, 4)
                    .unwrap_err()
                    .contains("exceeds 4 bytes")
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn clipboard_writer_returns_while_background_owner_retains_standard_handles() {
            let (send, receive) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let result =
                    write_linux_clipboard("sh", &["-c", "cat >/dev/null; sleep 4 &"], b"clipboard");
                send.send(result).unwrap();
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
        }
    }
}

#[cfg(unix)]
pub use runtime::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_capabilities_json_and_presets() {
        let current = platform_capabilities();
        assert!(!current.os.is_empty());
        assert!(current.to_json().starts_with(r#"{"os":""#));
        assert_eq!(current.media_gstreamer, cfg!(feature = "media-gstreamer"));

        let win = PlatformCapabilities::windows();
        assert_eq!(win.os, "windows");
        assert!(win.safe_storage && win.system_tray && win.custom_uri_scheme);
        assert!(!win.media_gstreamer);
        let win_json = win.to_json();
        assert!(win_json.contains(r#""os":"windows""#), "{win_json}");
        assert!(win_json.contains(r#""nativeDialogs":true"#), "{win_json}");
        assert!(win_json.contains(r#""mediaGstreamer":false"#), "{win_json}");

        let media_enabled = PlatformCapabilities {
            media_gstreamer: true,
            ..win
        };
        assert!(media_enabled.media_gstreamer);
        assert!(media_enabled.to_json().contains(r#""mediaGstreamer":true"#));

        let mac = PlatformCapabilities::macos();
        assert_eq!(mac.os, "macos");
        assert!(mac.native_dialogs && mac.clipboard_text && mac.autostart && mac.single_instance);
        assert!(mac.clipboard_image);
        assert!(!mac.custom_uri_scheme);
        assert!(!mac.safe_storage && !mac.media_gstreamer);
        assert!(mac.to_json().contains(r#""mediaGstreamer":false"#));

        let linux = PlatformCapabilities::linux();
        assert_eq!(linux.os, "linux");
        assert!(linux.native_dialogs && linux.notifications && linux.autostart);
        assert!(linux.clipboard_image && linux.custom_uri_scheme);
        assert!(!linux.system_tray && !linux.media_gstreamer);
        assert!(linux.to_json().contains(r#""mediaGstreamer":false"#));

        let other = PlatformCapabilities::other();
        assert_eq!(other.os, "other");
        assert!(!other.native_dialogs && !other.single_instance && !other.media_gstreamer);
        assert!(other.to_json().contains(r#""mediaGstreamer":false"#));

        let other_unix = PlatformCapabilities::other_unix();
        assert_eq!(other_unix.os, "other");
        assert!(!other_unix.native_dialogs && other_unix.single_instance);
        assert!(!other_unix.media_gstreamer);
        assert!(other_unix.to_json().contains(r#""mediaGstreamer":false"#));
    }

    #[test]
    fn sanitize_unix_identifier_and_build_instance_paths() {
        assert_eq!(
            sanitize_unix_identifier(r"Local\Draconium.Workspace"),
            "Local-Draconium.Workspace"
        );
        assert_eq!(sanitize_unix_identifier("../..///"), "draconium");
        let dir = Path::new("/run/user/1000");
        let socket = instance_path(dir, "Local\\App", ".sock");
        let lock = instance_path(dir, "Local\\App", ".lock");
        assert_eq!(socket.with_extension("lock"), lock);
        assert_ne!(socket, instance_path(dir, "Local-App", ".sock"));
        let long_tag = r"Local\Draconium.Workspace.VeryLongInstanceIdentifier";
        assert!(
            instance_path(Path::new("/tmp/dn-1000"), long_tag, ".sock")
                .as_os_str()
                .len()
                <= 96
        );
    }

    #[test]
    fn dialog_paths_preserve_single_filenames_and_validate_multiple_paths() {
        assert_eq!(
            parse_dialog_path("/tmp/report \n"),
            Some(PathBuf::from("/tmp/report "))
        );
        assert_eq!(
            parse_dialog_path("/tmp/report\n\n"),
            Some(PathBuf::from("/tmp/report\n"))
        );
        assert_eq!(
            parse_dialog_path("/tmp/report\n\r\n"),
            Some(PathBuf::from("/tmp/report\n"))
        );
        assert_eq!(
            parse_dialog_path("/tmp/report\r\n"),
            Some(PathBuf::from("/tmp/report"))
        );
        assert_eq!(
            parse_dialog_path("/tmp/report"),
            Some(PathBuf::from("/tmp/report"))
        );
        assert_eq!(parse_dialog_path("/tmp/a\0b\n"), None);
        assert_eq!(parse_dialog_path("\n"), None);
        assert_eq!(
            parse_open_dialog_paths("/tmp/one \x1e/tmp/two\n", true),
            Some(vec![PathBuf::from("/tmp/one "), PathBuf::from("/tmp/two")])
        );
        for output in [
            "",
            "/tmp/one\x1e",
            "/tmp/one\x1erelative\n",
            "/tmp/a\nb\n",
            "/tmp/a\tb\n",
            "/tmp/a\0b\n",
            "/tmp/a\r\n",
        ] {
            assert_eq!(parse_open_dialog_paths(output, true), None, "{output:?}");
        }
        assert_eq!(
            parse_open_dialog_paths("/tmp/a\nb\n", false),
            Some(vec![PathBuf::from("/tmp/a\nb")])
        );
        assert!(build_macos_open_file_script(&[], true).contains("character id 30"));
    }

    #[test]
    fn applescript_escaping_and_builders_prevent_injection() {
        let escaped = escape_applescript_string("hi \"world\" \\ \n\0test");
        assert_eq!(escaped, r#""hi \"world\" \\ \ntest""#);

        let alert = build_macos_alert_script("Title", "Body");
        assert!(alert.contains(r#"display alert "Title" message "Body""#));

        let confirm = build_macos_confirm_script("Confirm", "Proceed?");
        assert!(confirm.contains(r#"display dialog "Proceed?" with title "Confirm""#));

        let open_multi = build_macos_open_file_script(
            &[".md".to_owned(), "txt".to_owned(), "../bad".to_owned()],
            true,
        );
        assert!(
            open_multi.contains(r#"of type {"md", "txt"}"#),
            "{open_multi}"
        );
        assert!(open_multi.contains("with multiple selections allowed"));
        assert!(!open_multi.contains("bad"));

        let save = build_macos_save_file_script("note.md");
        assert!(save.contains(r#"default name "note.md""#));
        assert_eq!(
            build_macos_save_file_script("  "),
            "return POSIX path of (choose file name)"
        );

        let folder = build_macos_select_folder_script("Pick workspace");
        assert!(folder.contains(r#"with prompt "Pick workspace""#));
        assert_eq!(
            build_macos_select_folder_script(""),
            "return POSIX path of (choose folder)"
        );

        let notify = build_macos_notification_script("Folio", "Saved");
        assert_eq!(notify, r#"display notification "Saved" with title "Folio""#);
    }

    #[test]
    fn color_parsers_and_theme_detectors() {
        assert_eq!(
            parse_macos_color_rgb("65535, 32768, 0"),
            Some((255, 127, 0))
        );
        assert_eq!(parse_macos_color_rgb("invalid"), None);
        assert!(build_macos_color_picker_script((255, 0, 128)).contains("{65535, 0, 32896}"));

        assert_eq!(
            parse_zenity_color_rgb("rgb(12, 34, 56)"),
            Some((12, 34, 56))
        );
        assert_eq!(
            parse_zenity_color_rgb("rgba(255,128,64,0.5)"),
            Some((255, 128, 64))
        );
        assert_eq!(parse_zenity_color_rgb("#1d4ed8"), Some((0x1d, 0x4e, 0xd8)));
        assert_eq!(parse_zenity_color_rgb("#xyz"), None);

        assert_eq!(parse_macos_dark_mode("Dark\n"), Some(true));
        assert_eq!(parse_macos_dark_mode(""), Some(false));

        assert_eq!(parse_linux_dark_mode("'prefer-dark'", ""), Some(true));
        assert_eq!(
            parse_linux_dark_mode("'prefer-light'", "Adwaita-dark"),
            Some(false)
        );
        assert_eq!(
            parse_linux_dark_mode("'default'", "'Adwaita-dark'"),
            Some(true)
        );
        assert_eq!(parse_linux_dark_mode("'default'", "'Adwaita'"), Some(false));
    }

    #[test]
    fn autostart_plist_and_xdg_desktop_generators() {
        let label = macos_launch_agent_label("Draconium.Folio");
        assert_eq!(label, "io.draconium.Draconium.Folio");
        let plist = build_launch_agent_plist(
            &label,
            &[
                "/usr/local/bin/draconium & <test>".to_owned(),
                "--workspace".to_owned(),
            ],
        );
        assert!(plist.contains("<string>io.draconium.Draconium.Folio</string>"));
        assert!(plist.contains("&amp; &lt;test&gt;"));
        assert!(plist.contains("<key>RunAtLoad</key>"));

        let desktop = build_xdg_autostart_desktop(
            "Draconium Folio",
            Path::new("/opt/my;apps/draconium 100%"),
            &["--workspace", "--tag=$HOME"],
        )
        .unwrap();
        assert!(desktop.contains("[Desktop Entry]"));
        assert!(desktop.contains("Name=Draconium Folio"));
        assert!(
            desktop.contains(r#"Exec="/opt/my;apps/draconium 100%%" --workspace "--tag=\\$HOME""#),
            "{desktop}"
        );
    }

    #[test]
    fn zenity_filter_and_hex_encoding() {
        assert_eq!(
            build_zenity_file_filter(&[".md".to_owned(), "TXT".to_owned(), ";rm -rf".to_owned()]),
            Some("Supported files | *.md *.txt".to_owned())
        );
        assert_eq!(build_zenity_file_filter(&[]), None);
        assert_eq!(encode_hex_upper(b"<b>"), "3C623E");
    }

    #[test]
    fn png_dimension_parser_checks_signature_header_and_pixel_limit() {
        let mut png_header = [0_u8; 24];
        png_header[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png_header[8..12].copy_from_slice(&13_u32.to_be_bytes());
        png_header[12..16].copy_from_slice(b"IHDR");
        png_header[16..20].copy_from_slice(&320_u32.to_be_bytes());
        png_header[20..24].copy_from_slice(&240_u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&png_header), Some((320, 240)));

        let mut bad_magic = png_header;
        bad_magic[0] = 0;
        assert_eq!(parse_png_dimensions(&bad_magic), None);

        let mut zero_width = png_header;
        zero_width[16..20].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&zero_width), None);

        let mut zero_height = png_header;
        zero_height[20..24].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&zero_height), None);

        let mut bad_chunk_length = png_header;
        bad_chunk_length[8..12].copy_from_slice(&12_u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&bad_chunk_length), None);

        let mut bad_chunk_type = png_header;
        bad_chunk_type[12..16].copy_from_slice(b"IDAT");
        assert_eq!(parse_png_dimensions(&bad_chunk_type), None);

        let mut too_many_pixels = png_header;
        too_many_pixels[16..20].copy_from_slice(&8_000_u32.to_be_bytes());
        too_many_pixels[20..24].copy_from_slice(&8_001_u32.to_be_bytes());
        assert_eq!(parse_png_dimensions(&too_many_pixels), None);

        let mut max_dimensions = png_header;
        max_dimensions[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        max_dimensions[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse_png_dimensions(&max_dimensions), None);
    }

    #[test]
    fn hex_decoder_accepts_only_even_ascii_hex() {
        assert_eq!(decode_hex_bytes("00a1Ff"), Some(vec![0x00, 0xa1, 0xff]));
        assert_eq!(decode_hex_bytes(""), Some(Vec::new()));
        assert_eq!(decode_hex_bytes("ABC"), None);
        assert_eq!(decode_hex_bytes("0G"), None);
        assert_eq!(decode_hex_bytes("é0"), None);
    }

    #[test]
    fn xdg_exec_arguments_apply_both_escape_layers() {
        assert_eq!(format_xdg_exec_arg("").unwrap(), r#""""#);
        assert_eq!(format_xdg_exec_arg("100%").unwrap(), "100%%");
        assert_eq!(
            format_xdg_exec_arg("a\\b\"c\x60d$e\tf").unwrap(),
            "\"a\\\\\\\\b\\\\\"c\\\\\x60d\\\\$e\\tf\""
        );
        for input in ["\0", "with\nline", "with\rreturn"] {
            assert!(format_xdg_exec_arg(input).is_err());
            assert!(build_xdg_autostart_desktop(input, Path::new("/opt/draconium"), &[]).is_err());
            assert!(
                build_xdg_protocol_handler_desktop(
                    "Example",
                    "example",
                    Path::new("/opt/draconium"),
                    &[input]
                )
                .is_err()
            );
        }
    }

    #[test]
    fn xdg_protocol_handler_filename_and_desktop_escape_exec_values() {
        let plus = xdg_protocol_handler_filename("foo+bar");
        let dash = xdg_protocol_handler_filename("foo-bar");
        assert_eq!(
            plus,
            format!("dn-scheme-foo-bar-{:08x}.desktop", fnv1a_32("foo+bar"))
        );
        assert_ne!(plus, dash);

        let desktop = build_xdg_protocol_handler_desktop(
            r"Example\App",
            "example",
            Path::new("/opt/quantum view/app%"),
            &["--workspace", "--tag=$HOME"],
        )
        .unwrap();
        assert!(desktop.contains("Name=Example\\\\App\n"), "{desktop}");
        assert!(
            desktop.contains(
                r#"Exec="/opt/quantum view/app%%" --workspace "--tag=\\$HOME" --deep-link %u"#
            ),
            "{desktop}"
        );
        assert!(desktop.contains("MimeType=x-scheme-handler/example;\n"));
        assert!(desktop.contains("Type=Application\nVersion=1.0\n"));
        assert!(desktop.contains("Terminal=false\nNoDisplay=true\n"));
    }

    #[test]
    fn xdg_mime_cleanup_preserves_other_schemes_and_handlers() {
        let input = concat!(
            "[Default Applications]\r\n",
            "x-scheme-handler/foo+bar=ours.desktop;\r\n",
            "x-scheme-handler/foo-bar=ours.desktop;\r\n",
            "[Added Associations]\r\n",
            "x-scheme-handler/foo+bar=other.desktop;ours.desktop;backup-ours.desktop;\r\n",
            "# x-scheme-handler/foo+bar=ours.desktop;\n",
            "text/plain=ours.desktop;",
        );
        assert_eq!(
            remove_xdg_mime_handler(input, "foo+bar", "ours.desktop"),
            concat!(
                "[Default Applications]\r\n",
                "x-scheme-handler/foo-bar=ours.desktop;\r\n",
                "[Added Associations]\r\n",
                "x-scheme-handler/foo+bar=other.desktop;backup-ours.desktop;\r\n",
                "# x-scheme-handler/foo+bar=ours.desktop;\n",
                "text/plain=ours.desktop;",
            )
        );
        assert_eq!(remove_xdg_mime_handler(input, "foo", "ours.desktop"), input);
    }
}

//! Windows OS integration & engine-gap host primitives.
//!
//! Bridges capabilities that Servo 0.7.0 / winit 0.30 do not provide directly:
//! - Win11 DWM rounded corners & dark titlebar (`DwmSetWindowAttribute`)
//! - Layered window opacity & click-through (`SetLayeredWindowAttributes`, `WS_EX_TRANSPARENT`)
//! - Windows system theme detection (`AppsUseLightTheme` via `RegGetValueW`)
//! - DPAPI `safeStorage` (`CryptProtectData` / `CryptUnprotectData`)
//! - Single-instance mutex (`CreateMutexW`) + `WM_COPYDATA` second-instance / deep-link forwarding
//! - Rich Win32 Clipboard (`CF_UNICODETEXT`, `"HTML Format"`, `CF_DIB` <-> RGBA/PNG/BMP)
//! - Registry autostart (`HKCU\...\Run`) and custom URI scheme registration (`HKCU\Software\Classes\<scheme>`)
//! - Structured crash report persistence (`.crash.json`) with automatic rotation
//! - Pure-Rust uncompressed PNG & single-page raster PDF (`%PDF-1.4`) encoders
//! - Capability-scoped file handle registry for Drag & Drop and native File Dialogs
//! - Global hotkey accelerator parser, RFC 4648 base64, and tiny JSON payload helpers
//!
//! All pure-logic helpers live outside `#[cfg(target_os = "windows")]` so they
//! are unit-tested on any OS via `rustc --edition 2024 --test`.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard cap on capability-scoped file reads (16 MiB) so a huge dropped file
/// cannot exhaust memory inside the JS bridge.
pub const MAX_CAPABILITY_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Hard cap on clipboard payload reads/writes (16 MiB).
pub const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;
/// Hard cap on clipboard/export image dimensions (4096 x 4096 pixels).
pub const MAX_CLIPBOARD_IMAGE_PIXELS: usize = 4096 * 4096;
/// Maximum number of active file tokens kept in memory per host session.
const MAX_CAPABILITY_TOKENS: usize = 64;
/// Maximum number of `.crash.json` reports retained in the crash directory.
const MAX_CRASH_REPORTS: usize = 10;

/// Magic `dwData` tag identifying Draconium single-instance `WM_COPYDATA` packets (`"DNIP"`).
pub const DN_COPYDATA_MAGIC: usize = 0x444E_4950;
/// Maximum byte length of a `WM_COPYDATA` second-instance JSON payload.
pub const MAX_COPYDATA_BYTES: usize = 65_536;

/// Win32 `RegisterHotKey` modifier flags.
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_NOREPEAT: u32 = 0x4000;

/// Escape a string as a JSON string literal (including surrounding quotes).
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Convert a requested opacity (`0.0..=1.0`) into a Win32 alpha byte.
///
/// Clamps to `[0.1, 1.0]` so an accidental `0.0` never traps the user with an
/// invisible active window; rejects non-finite values.
pub fn opacity_to_alpha(value: f64) -> Option<u8> {
    if !value.is_finite() {
        return None;
    }
    let clamped = value.clamp(0.1, 1.0);
    Some((clamped * 255.0).round() as u8)
}

/// Parse an Electron-style global shortcut accelerator such as `"Ctrl+Alt+U"` or
/// `"Control+Shift+F12"` into `(win32_modifiers, win32_vk)`.
///
/// At least one modifier is required so plain typing keys cannot be captured
/// system-wide.
pub fn parse_hotkey(spec: &str) -> Result<(u32, u32), String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("ホットキー指定が空です".to_owned());
    }
    let mut modifiers = 0u32;
    let mut vk: Option<u32> = None;

    for raw_part in trimmed.split('+') {
        let part = raw_part.trim();
        if part.is_empty() {
            return Err(format!("ホットキーの書式が不正です: {spec}"));
        }
        let upper = part.to_ascii_uppercase();
        match upper.as_str() {
            "CTRL" | "CONTROL" | "CMDORCTRL" | "COMMANDORCONTROL" => modifiers |= MOD_CONTROL,
            "ALT" | "OPTION" => modifiers |= MOD_ALT,
            "SHIFT" => modifiers |= MOD_SHIFT,
            "WIN" | "SUPER" | "META" | "CMD" | "COMMAND" => modifiers |= MOD_WIN,
            _ => {
                if vk.is_some() {
                    return Err(format!("メインキーは 1 つだけ指定できます: {spec}"));
                }
                vk = Some(
                    parse_virtual_key(&upper)
                        .ok_or_else(|| format!("未対応のホットキーです: {part}"))?,
                );
            }
        }
    }

    if modifiers == 0 {
        return Err(format!(
            "グローバルホットキーには Ctrl / Alt / Shift / Win のいずれかが必要です: {spec}"
        ));
    }
    let key = vk.ok_or_else(|| format!("メインキーが指定されていません: {spec}"))?;
    Ok((modifiers | MOD_NOREPEAT, key))
}

fn parse_virtual_key(upper: &str) -> Option<u32> {
    if upper.len() == 1 {
        let byte = upper.as_bytes()[0];
        if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    if let Some(num) = upper.strip_prefix('F')
        && let Ok(n) = num.parse::<u32>()
        && (1..=24).contains(&n)
    {
        return Some(0x70 + (n - 1));
    }
    match upper {
        "SPACE" => Some(0x20),
        "TAB" => Some(0x09),
        "ENTER" | "RETURN" => Some(0x0D),
        "ESC" | "ESCAPE" => Some(0x1B),
        "BACKSPACE" => Some(0x08),
        "DELETE" | "DEL" => Some(0x2E),
        "INSERT" | "INS" => Some(0x2D),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" | "PGUP" => Some(0x21),
        "PAGEDOWN" | "PGDN" => Some(0x22),
        "LEFT" => Some(0x25),
        "UP" => Some(0x26),
        "RIGHT" => Some(0x27),
        "DOWN" => Some(0x28),
        _ => None,
    }
}

/// Sanitize a suggested filename for native Save dialogs: strip any path
/// components, control characters, and Windows-reserved characters.
pub fn sanitize_suggested_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let cleaned: String = base
        .chars()
        .filter(|&ch| {
            !ch.is_control() && !matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .take(128)
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "download.bin".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Validate a `--devtools` listen address so it can only bind loopback.
pub fn validate_devtools_addr(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let (host, port_str) = trimmed
        .rsplit_once(':')
        .ok_or_else(|| format!("devtools アドレスは host:port 形式で指定してください: {raw}"))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("devtools ポート番号が不正です: {port_str}"))?;
    if port == 0 {
        return Err("devtools ポート番号に 0 は指定できません".to_owned());
    }
    let normalized_host = match host.to_ascii_lowercase().as_str() {
        "127.0.0.1" | "localhost" => "127.0.0.1",
        "[::1]" => "[::1]",
        _ => {
            return Err(format!(
                "devtools はループバック (127.0.0.1 / localhost / [::1]) のみ許可されます: {host}"
            ));
        }
    };
    Ok(format!("{normalized_host}:{port}"))
}

/// RFC 4648 standard base64 encoder (dependency-free).
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// RFC 4648 standard base64 decoder with a strict decoded-size cap.
pub fn base64_decode(input: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut vals = Vec::<u8>::new();
    let mut pad_count = 0usize;
    let mut saw_pad = false;
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            saw_pad = true;
            pad_count += 1;
            vals.push(0);
            continue;
        }
        if saw_pad {
            return Err("base64 パディングの後に文字があります".to_owned());
        }
        let v = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("base64 に不正な文字が含まれています".to_owned()),
        };
        vals.push(v);
        if (vals.len() / 4) * 3 > max_bytes + 3 {
            return Err("データサイズが上限を超えています".to_owned());
        }
    }
    if vals.len() % 4 != 0 || pad_count > 2 {
        return Err("base64 の長さが不正です".to_owned());
    }
    if vals.is_empty() {
        return Ok(Vec::new());
    }
    let decoded_len = (vals.len() / 4) * 3 - pad_count;
    if decoded_len > max_bytes {
        return Err("データサイズが上限を超えています".to_owned());
    }
    let mut out = Vec::with_capacity(decoded_len);
    for (idx, chunk) in vals.chunks_exact(4).enumerate() {
        let is_last = idx + 1 == vals.len() / 4;
        let n = (u32::from(chunk[0]) << 18)
            | (u32::from(chunk[1]) << 12)
            | (u32::from(chunk[2]) << 6)
            | u32::from(chunk[3]);
        out.push(((n >> 16) & 0xff) as u8);
        if !is_last || pad_count < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if !is_last || pad_count == 0 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

/// Capability-scoped registry mapping opaque tokens to OS-approved file paths
/// (from winit `DroppedFile` or Win32 `GetOpenFileNameW`).
///
/// The page NEVER passes an arbitrary filesystem path to the host; it only
/// receives opaque tokens for files the user explicitly dropped or picked.
pub struct FileAccessRegistry {
    salt: u64,
    next_id: u64,
    order: VecDeque<String>,
    entries: HashMap<String, PathBuf>,
}

impl Default for FileAccessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FileAccessRegistry {
    pub fn new() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        Self {
            salt: hasher.finish(),
            next_id: 1,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    /// Register a user-approved file path and return its capability token.
    pub fn register(&mut self, path: PathBuf) -> String {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let token = format!("dnfile-{:016x}-{id}", self.salt ^ id.rotate_left(17));
        if self.entries.len() >= MAX_CAPABILITY_TOKENS
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(token.clone());
        self.entries.insert(token.clone(), path);
        token
    }

    /// Register a batch of paths and return a JSON array of
    /// `{"token":"...","name":"...","size":123}` descriptors (basename only,
    /// no directory path disclosure).
    pub fn register_batch_json(&mut self, paths: &[PathBuf]) -> String {
        let mut items = Vec::with_capacity(paths.len());
        for path in paths {
            let token = self.register(path.clone());
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_owned());
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            items.push(format!(
                "{{\"token\":{},\"name\":{},\"size\":{size}}}",
                json_string(&token),
                json_string(&name),
            ));
        }
        format!("[{}]", items.join(","))
    }

    /// Resolve an authorized token to its underlying `PathBuf` (used by main-thread
    /// validation before offloading file I/O to an async worker thread).
    pub fn resolve_path(&self, token: &str) -> Result<PathBuf, String> {
        self.entries
            .get(token.trim())
            .cloned()
            .ok_or_else(|| "許可されていないファイルトークンです".to_owned())
    }

    /// Read raw bytes from an already-authorized path (capped at `MAX_CAPABILITY_FILE_BYTES`).
    pub fn read_authorized_path_bytes(path: &Path) -> Result<Vec<u8>, String> {
        let meta = std::fs::metadata(path).map_err(|err| format!("ファイル情報取得失敗: {err}"))?;
        if !meta.is_file() {
            return Err("通常ファイル以外は読み込めません".to_owned());
        }
        if meta.len() > MAX_CAPABILITY_FILE_BYTES as u64 {
            return Err(format!(
                "ファイルサイズが上限 ({} MiB) を超えています",
                MAX_CAPABILITY_FILE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|err| format!("ファイル読み込み失敗: {err}"))?;
        if bytes.len() > MAX_CAPABILITY_FILE_BYTES {
            return Err("ファイルサイズが上限を超えています".to_owned());
        }
        Ok(bytes)
    }

    pub fn read_authorized_path_text(path: &Path) -> Result<String, String> {
        let bytes = Self::read_authorized_path_bytes(path)?;
        String::from_utf8(bytes).map_err(|_| "UTF-8 テキストとして読み込めませんでした".to_owned())
    }

    pub fn read_authorized_path_base64(path: &Path) -> Result<String, String> {
        let bytes = Self::read_authorized_path_bytes(path)?;
        Ok(base64_encode(&bytes))
    }

    /// Read raw bytes for an authorized token (capped at `MAX_CAPABILITY_FILE_BYTES`).
    #[cfg(test)]
    pub fn read_bytes(&self, token: &str) -> Result<Vec<u8>, String> {
        let path = self.resolve_path(token)?;
        Self::read_authorized_path_bytes(&path)
    }

    pub fn read_text(&self, token: &str) -> Result<String, String> {
        let path = self.resolve_path(token)?;
        Self::read_authorized_path_text(&path)
    }

    pub fn read_base64(&self, token: &str) -> Result<String, String> {
        let path = self.resolve_path(token)?;
        Self::read_authorized_path_base64(&path)
    }
}

// ---------------------------------------------------------------------------
// CF_HTML ("HTML Format") & CF_DIB / PNG / BMP / Raster-PDF Codecs (Pure Rust)
// ---------------------------------------------------------------------------

/// Build a Windows `"HTML Format"` (`CF_HTML`) clipboard payload with exact
/// 10-digit zero-padded UTF-8 byte offsets.
pub fn build_cf_html(fragment: &str) -> String {
    const HEADER_LEN: usize = 105;
    const PREFIX: &str = "<!DOCTYPE html><html><body><!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment--></body></html>";
    let start_html = HEADER_LEN;
    let start_fragment = start_html + PREFIX.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + SUFFIX.len();
    let header = format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    );
    debug_assert_eq!(header.len(), HEADER_LEN);
    format!("{header}{PREFIX}{fragment}{SUFFIX}")
}

/// Extract the HTML fragment from a Windows `"HTML Format"` (`CF_HTML`) buffer.
pub fn parse_cf_html(raw: &[u8]) -> Option<String> {
    let without_nul = raw.split(|&b| b == 0).next().unwrap_or(raw);
    let text = std::str::from_utf8(without_nul).ok()?;
    let mut start_frag: Option<usize> = None;
    let mut end_frag: Option<usize> = None;
    for line in text.lines().take(12) {
        if let Some(rest) = line.strip_prefix("StartFragment:") {
            start_frag = rest.trim().parse::<usize>().ok();
        } else if let Some(rest) = line.strip_prefix("EndFragment:") {
            end_frag = rest.trim().parse::<usize>().ok();
        }
    }
    if let (Some(s), Some(e)) = (start_frag, end_frag)
        && s <= e
        && e <= text.len()
        && text.is_char_boundary(s)
        && text.is_char_boundary(e)
    {
        return Some(text[s..e].to_owned());
    }
    const START_MARKER: &str = "<!--StartFragment-->";
    const END_MARKER: &str = "<!--EndFragment-->";
    if let Some(s_idx) = text.find(START_MARKER) {
        let content_start = s_idx + START_MARKER.len();
        if let Some(rel_end) = text[content_start..].find(END_MARKER) {
            return Some(text[content_start..content_start + rel_end].to_owned());
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Encode top-down 8-bit RGBA pixels into a Win32 `CF_DIB` buffer (40-byte
/// `BITMAPINFOHEADER` + bottom-up 32-bpp BGRA rows).
pub fn rgba_to_dib(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("画像サイズが 0 です".to_owned());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "画像サイズがオーバーフローしました".to_owned())?;
    if pixels > MAX_CLIPBOARD_IMAGE_PIXELS {
        return Err("画像解像度が上限を超えています".to_owned());
    }
    let byte_len = pixels
        .checked_mul(4)
        .ok_or_else(|| "画像バイト数がオーバーフローしました".to_owned())?;
    if rgba.len() != byte_len {
        return Err("RGBA バッファ長が width * height * 4 と一致しません".to_owned());
    }
    let mut out = Vec::with_capacity(40 + byte_len);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight (positive = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&(byte_len as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    let row_bytes = (width as usize) * 4;
    for y in (0..height as usize).rev() {
        let row = &rgba[y * row_bytes..(y + 1) * row_bytes];
        for px in row.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Ok(out)
}

/// Decode a Win32 `CF_DIB` buffer (24-bpp or 32-bpp `BI_RGB` / `BI_BITFIELDS`,
/// bottom-up or top-down) into top-down `(width, height, rgba_bytes)`.
pub fn dib_to_rgba(dib: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if dib.len() < 40 {
        return Err("DIB ヘッダが 40 バイト未満です".to_owned());
    }
    let bi_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    if bi_size < 40 || bi_size > dib.len() {
        return Err("DIB biSize が不正です".to_owned());
    }
    let bi_width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
    let bi_height = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    let bi_bit_count = u16::from_le_bytes([dib[14], dib[15]]);
    let bi_compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let bi_clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]) as usize;

    if bi_width <= 0 || bi_height == 0 || bi_height == i32::MIN {
        return Err("DIB の幅または高さが不正です".to_owned());
    }
    let width = bi_width as u32;
    let top_down = bi_height < 0;
    let height = bi_height.unsigned_abs();
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "DIB ピクセル数がオーバーフローしました".to_owned())?;
    if pixels > MAX_CLIPBOARD_IMAGE_PIXELS {
        return Err("DIB 解像度が上限を超えています".to_owned());
    }
    if !matches!(bi_bit_count, 24 | 32) || !matches!(bi_compression, 0 | 3) {
        return Err(format!(
            "未対応の DIB 形式です (bpp={bi_bit_count}, compression={bi_compression})"
        ));
    }
    let extra_masks = if bi_compression == 3 && bi_size == 40 && bi_clr_used == 0 {
        12usize
    } else {
        0usize
    };
    let pixel_offset = bi_size
        .checked_add(bi_clr_used.saturating_mul(4))
        .and_then(|v| v.checked_add(extra_masks))
        .ok_or_else(|| "DIB オフセットが不正です".to_owned())?;
    let stride = if bi_bit_count == 24 {
        ((width as usize) * 3 + 3) & !3
    } else {
        (width as usize) * 4
    };
    let required = pixel_offset
        .checked_add(stride.saturating_mul(height as usize))
        .ok_or_else(|| "DIB サイズが不正です".to_owned())?;
    if dib.len() < required {
        return Err("DIB データが不足しています".to_owned());
    }

    let mut rgba = vec![0u8; pixels * 4];
    let mut any_non_zero_alpha = false;
    for dst_y in 0..height as usize {
        let src_y = if top_down {
            dst_y
        } else {
            (height as usize) - 1 - dst_y
        };
        let src_row = &dib[pixel_offset + src_y * stride..pixel_offset + (src_y + 1) * stride];
        let dst_row = &mut rgba[dst_y * (width as usize) * 4..(dst_y + 1) * (width as usize) * 4];
        if bi_bit_count == 24 {
            for x in 0..width as usize {
                let b = src_row[x * 3];
                let g = src_row[x * 3 + 1];
                let r = src_row[x * 3 + 2];
                dst_row[x * 4..x * 4 + 4].copy_from_slice(&[r, g, b, 255]);
            }
            any_non_zero_alpha = true;
        } else {
            for x in 0..width as usize {
                let b = src_row[x * 4];
                let g = src_row[x * 4 + 1];
                let r = src_row[x * 4 + 2];
                let a = src_row[x * 4 + 3];
                if a != 0 {
                    any_non_zero_alpha = true;
                }
                dst_row[x * 4..x * 4 + 4].copy_from_slice(&[r, g, b, a]);
            }
        }
    }
    if !any_non_zero_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Ok((width, height, rgba))
}

/// Prepend a 14-byte `BITMAPFILEHEADER` (`"BM"`) to a `CF_DIB` buffer.
#[cfg(test)]
pub fn encode_dib_as_bmp(dib: &[u8]) -> Result<Vec<u8>, String> {
    if dib.len() < 40 {
        return Err("DIB ヘッダが不足しています".to_owned());
    }
    let bi_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]);
    let bi_compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let bi_clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]);
    let color_table_bytes = if bi_clr_used > 0 {
        bi_clr_used.saturating_mul(4)
    } else if bi_compression == 3 && bi_size == 40 {
        12
    } else {
        0
    };
    let file_size = u32::try_from(14 + dib.len()).map_err(|_| "BMP が大きすぎます".to_owned())?;
    let pixel_offset = 14u32
        .saturating_add(bi_size)
        .saturating_add(color_table_bytes);
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Ok(bmp)
}

/// Strip the 14-byte `"BM"` file header from a BMP image and validate its DIB header.
pub fn decode_bmp_to_dib(bmp: &[u8]) -> Result<Vec<u8>, String> {
    if bmp.len() < 54 || &bmp[0..2] != b"BM" {
        return Err("有効な BMP データではありません".to_owned());
    }
    let dib = &bmp[14..];
    let _ = dib_to_rgba(dib)?;
    Ok(dib.to_vec())
}

fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

fn zlib_compress_stored(raw: &[u8]) -> Vec<u8> {
    let num_blocks = raw.len().div_ceil(65_535).max(1);
    let mut out = Vec::with_capacity(2 + num_blocks * 5 + raw.len() + 4);
    out.extend_from_slice(&[0x78, 0x01]); // zlib header (CMF/FLG)
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (idx, chunk) in chunks.iter().enumerate() {
            let bfinal = u8::from(idx + 1 == chunks.len());
            let len = chunk.len() as u16;
            let nlen = !len;
            out.push(bfinal);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn write_png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let crc = crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode top-down 8-bit RGBA pixels into a standard PNG image using stored
/// (uncompressed) zlib DEFLATE blocks (zero external dependencies).
pub fn encode_rgba_as_stored_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("PNG 画像サイズが 0 です".to_owned());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "PNG ピクセル数がオーバーフローしました".to_owned())?;
    if pixels > MAX_CLIPBOARD_IMAGE_PIXELS || rgba.len() != pixels * 4 {
        return Err("PNG RGBA バッファサイズが不整合です".to_owned());
    }
    let row_bytes = (width as usize) * 4;
    let mut raw_scanlines = Vec::with_capacity((row_bytes + 1) * (height as usize));
    for y in 0..height as usize {
        raw_scanlines.push(0x00); // Filter type 0 (None)
        raw_scanlines.extend_from_slice(&rgba[y * row_bytes..(y + 1) * row_bytes]);
    }
    let idat = zlib_compress_stored(&raw_scanlines);

    let mut png = Vec::with_capacity(64 + idat.len());
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, non-interlaced
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &idat);
    write_png_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// Encode top-down 8-bit RGBA pixels into a single-page `%PDF-1.4` document
/// embedding a `/FlateDecode` RGB `/Image` XObject (zero external dependencies).
pub fn encode_rgba_as_raster_pdf(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("PDF 画像サイズが 0 です".to_owned());
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "PDF ピクセル数がオーバーフローしました".to_owned())?;
    if pixels > MAX_CLIPBOARD_IMAGE_PIXELS || rgba.len() != pixels * 4 {
        return Err("PDF RGBA バッファサイズが不整合です".to_owned());
    }
    let mut rgb = Vec::with_capacity(pixels * 3);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&[px[0], px[1], px[2]]);
    }
    let flate_stream = zlib_compress_stored(&rgb);
    let content_stream = format!("q {width} 0 0 {height} 0 0 cm /Im0 Do Q\n");

    let mut pdf = Vec::with_capacity(512 + flate_stream.len());
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let mut offsets = [0usize; 6];
    offsets[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets[3] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    offsets[4] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            flate_stream.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&flate_stream);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[5] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{content_stream}endstream\nendobj\n",
            content_stream.len()
        )
        .as_bytes(),
    );

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for &off in &offsets[1..] {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    Ok(pdf)
}

// ---------------------------------------------------------------------------
// Registry Autostart, Custom URI Protocol, Single-Instance, & Crash Reports
// ---------------------------------------------------------------------------

/// Validate a registry value name for `HKCU\...\Run` autostart entries.
pub fn validate_registry_app_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err("アプリ登録名は 1〜64 文字で指定してください".to_owned());
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("アプリ登録名に使用できない文字が含まれています".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// Format an executable path and static flags into a properly quoted Win32
/// command line (`CommandLineToArgvW`-compatible).
pub fn format_win32_command_line(exe: &Path, args: &[&str]) -> String {
    fn quote_arg(raw: &str) -> String {
        if !raw.is_empty()
            && !raw
                .chars()
                .any(|ch| ch.is_whitespace() || ch == '"' || ch.is_control())
        {
            return raw.to_owned();
        }
        let mut out = String::from("\"");
        let mut backslashes = 0usize;
        for ch in raw.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                if !ch.is_control() {
                    out.push(ch);
                }
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push('\\');
        }
        out.push('"');
        out
    }

    let mut parts = Vec::with_capacity(1 + args.len());
    parts.push(quote_arg(&exe.to_string_lossy()));
    for arg in args {
        parts.push(quote_arg(arg));
    }
    parts.join(" ")
}

/// Validate a custom URI scheme name for `HKCU\Software\Classes\<scheme>`
/// registration, rejecting system, browser, and internal schemes.
pub fn validate_custom_uri_scheme(scheme: &str) -> Result<String, String> {
    let lower = scheme.trim().to_ascii_lowercase();
    if lower.len() < 3 || lower.len() > 32 {
        return Err("カスタム URI スキームは 3〜32 文字で指定してください".to_owned());
    }
    let mut chars = lower.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase()) {
        return Err("カスタム URI スキームは英小文字で始まる必要があります".to_owned());
    }
    if !chars
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '+' | '.' | '-'))
    {
        return Err("カスタム URI スキームに使用できない文字が含まれています".to_owned());
    }
    const FORBIDDEN: &[&str] = &[
        "http",
        "https",
        "file",
        "ftp",
        "ws",
        "wss",
        "dn",
        "about",
        "blob",
        "data",
        "javascript",
        "vbscript",
        "mailto",
        "tel",
        "shell",
        "cmd",
        "res",
        "mk",
        "view-source",
    ];
    if FORBIDDEN.contains(&lower.as_str())
        || lower.starts_with("ms-")
        || lower.starts_with("search")
        || lower.starts_with("windows")
    {
        return Err(format!("予約済みスキームは登録できません: {lower}"));
    }
    Ok(lower)
}

/// Sanitize a single-instance / app tag into a safe suffix for the hidden OS
/// event window class and title (`"Draconium.TrayWindow.<tag>"`).
pub fn sanitize_instance_tag(tag: &str) -> String {
    let cleaned: String = tag
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        .take(48)
        .collect();
    if cleaned.is_empty() {
        "Default".to_owned()
    } else {
        cleaned
    }
}

/// Compute the scoped Win32 window class/title name for a given instance tag.
pub fn instance_window_name(tag: &str) -> String {
    format!("Draconium.TrayWindow.{}", sanitize_instance_tag(tag))
}

/// Build the compact JSON payload sent from a second instance to the primary
/// instance over `WM_COPYDATA`.
pub fn build_second_instance_json(args: &[String], deep_link: Option<&str>) -> String {
    let argv_items: Vec<String> = args.iter().take(32).map(|arg| json_string(arg)).collect();
    let deep = deep_link
        .map(json_string)
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "{{\"argv\":[{}],\"deepLink\":{deep}}}",
        argv_items.join(",")
    )
}

/// Strip query strings, fragments, and credentials from a URL before recording
/// it in a crash report.
pub fn sanitize_url_for_crash_report(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or("");
    if let Some((scheme, rest)) = base.split_once("://") {
        let cleaned_rest = if let Some((_, after_at)) = rest.split_once('@') {
            after_at
        } else {
            rest
        };
        format!("{scheme}://{cleaned_rest}")
    } else {
        base.to_owned()
    }
}

/// Structured crash diagnostics persisted on renderer crashes or host panics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashReport {
    pub timestamp_unix_ms: u128,
    pub pid: u32,
    pub app_title: String,
    pub url_sanitized: String,
    pub source: &'static str,
    pub reason: String,
    pub backtrace: Option<String>,
}

impl CrashReport {
    pub fn new(
        app_title: &str,
        url: &str,
        source: &'static str,
        reason: &str,
        backtrace: Option<String>,
    ) -> Self {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Self {
            timestamp_unix_ms,
            pid: std::process::id(),
            app_title: app_title.to_owned(),
            url_sanitized: sanitize_url_for_crash_report(url),
            source,
            reason: reason.to_owned(),
            backtrace,
        }
    }

    pub fn to_json(&self) -> String {
        let bt = self
            .backtrace
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned());
        format!(
            "{{\n  \"timestampUnixMs\": {},\n  \"pid\": {},\n  \"appTitle\": {},\n  \"url\": {},\n  \"source\": {},\n  \"reason\": {},\n  \"backtrace\": {bt}\n}}\n",
            self.timestamp_unix_ms,
            self.pid,
            json_string(&self.app_title),
            json_string(&self.url_sanitized),
            json_string(self.source),
            json_string(&self.reason),
        )
    }
}

/// Write a [`CrashReport`] as `<dir>/crash-<ts>-<pid>.crash.json` and prune
/// older reports beyond `MAX_CRASH_REPORTS`.
pub fn write_crash_report(dir: &Path, report: &CrashReport) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("クラッシュレポート保存先作成失敗: {err}"))?;
    let file_name = format!(
        "crash-{:015}-{}.crash.json",
        report.timestamp_unix_ms, report.pid
    );
    let path = dir.join(file_name);
    std::fs::write(&path, report.to_json())
        .map_err(|err| format!("クラッシュレポート書き込み失敗: {err}"))?;

    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".crash.json"))
            })
            .collect();
        files.sort();
        if files.len() > MAX_CRASH_REPORTS {
            for old in &files[..files.len() - MAX_CRASH_REPORTS] {
                let _ = std::fs::remove_file(old);
            }
        }
    }
    Ok(path)
}

/// Minimal flat JSON object reader for bridge payloads (zero external crates).
/// Supports top-level string, boolean, u32, and string-array fields with
/// standard JSON escape sequences (`\"`, `\\`, `\n`, `\r`, `\t`, `\uXXXX`).
pub fn json_get_string(json: &str, key: &str) -> Option<String> {
    let val = find_top_level_field(json, key)?;
    let (s, _) = parse_json_str(val)?;
    Some(s)
}

pub fn json_get_bool(json: &str, key: &str) -> Option<bool> {
    let val = find_top_level_field(json, key)?.trim_start();
    if val.starts_with("true") {
        Some(true)
    } else if val.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub fn json_get_u32(json: &str, key: &str) -> Option<u32> {
    let val = find_top_level_field(json, key)?.trim_start();
    let end = val.find(|c: char| !c.is_ascii_digit()).unwrap_or(val.len());
    if end == 0 {
        None
    } else {
        val[..end].parse::<u32>().ok()
    }
}

pub fn json_get_strict_u32(json: &str, key: &str) -> Option<u32> {
    let value = find_top_level_field(json, key)?;
    let end = value.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, rest) = value.split_at(end);
    if (digits.len() > 1 && digits.starts_with('0'))
        || !rest
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .starts_with([',', '}'])
    {
        return None;
    }
    digits.parse().ok()
}

pub fn json_get_string_array(json: &str, key: &str) -> Option<Vec<String>> {
    let mut rest = find_top_level_field(json, key)?.trim_start();
    rest = rest.strip_prefix('[')?;
    let mut out = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') {
            return Some(out);
        }
        let (item, after) = parse_json_str(rest)?;
        out.push(item);
        rest = after.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
        } else if rest.starts_with(']') {
            return Some(out);
        } else {
            return None;
        }
    }
}

fn find_top_level_field<'a>(json: &'a str, target_key: &str) -> Option<&'a str> {
    let mut rest = json.trim_start().strip_prefix('{')?;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('}') {
            return None;
        }
        let (key, after_key) = parse_json_str(rest)?;
        let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
        if key == target_key {
            return Some(after_colon);
        }
        let after_val = skip_json_value(after_colon)?.trim_start();
        if let Some(next) = after_val.strip_prefix(',') {
            rest = next;
        } else {
            return None;
        }
    }
}

fn parse_json_str(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => return Some((out, &rest[idx + 1..])),
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{08}'),
                    'f' => out.push('\u{0C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let code = read_hex4(&mut chars)?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            let (_, b1) = chars.next()?;
                            let (_, b2) = chars.next()?;
                            if b1 != '\\' || b2 != 'u' {
                                return None;
                            }
                            let low = read_hex4(&mut chars)?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return None;
                            }
                            let cp = 0x10000
                                + ((u32::from(code) - 0xD800) << 10)
                                + (u32::from(low) - 0xDC00);
                            out.push(char::from_u32(cp)?);
                        } else {
                            out.push(char::from_u32(u32::from(code))?);
                        }
                    }
                    _ => return None,
                }
            }
            ch => out.push(ch),
        }
    }
    None
}

fn read_hex4(chars: &mut std::str::CharIndices<'_>) -> Option<u16> {
    let mut val = 0u16;
    for _ in 0..4 {
        let (_, ch) = chars.next()?;
        let digit = ch.to_digit(16)? as u16;
        val = (val << 4) | digit;
    }
    Some(val)
}

fn skip_json_value(input: &str) -> Option<&str> {
    let s = input.trim_start();
    let first = s.chars().next()?;
    match first {
        '"' => parse_json_str(s).map(|(_, after)| after),
        '{' | '[' => {
            let mut depth = 0usize;
            let mut in_str = false;
            let mut escaped = false;
            for (idx, ch) in s.char_indices() {
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_str = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_str = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some(&s[idx + 1..]);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => {
            let end = s
                .find(|c: char| c == ',' || c == '}' || c == ']' || c.is_whitespace())
                .unwrap_or(s.len());
            if end == 0 { None } else { Some(&s[end..]) }
        }
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::ptr;
    use std::time::Duration;

    use super::{
        DN_COPYDATA_MAGIC, MAX_CLIPBOARD_BYTES, MAX_COPYDATA_BYTES, base64_encode, build_cf_html,
        dib_to_rgba, encode_rgba_as_stored_png, instance_window_name, parse_cf_html,
        validate_custom_uri_scheme, validate_registry_app_name,
    };

    fn wide_nul(s: &str) -> Vec<u16> {
        let mut out: Vec<u16> = s
            .chars()
            .filter(|&ch| ch != '\0')
            .flat_map(|ch| {
                let mut buf = [0u16; 2];
                ch.encode_utf16(&mut buf).to_vec()
            })
            .collect();
        out.push(0);
        out
    }

    type Hwnd = isize;
    type Handle = isize;
    type Hkey = isize;

    const GWL_EXSTYLE: i32 = -20;
    const WS_EX_TRANSPARENT: isize = 0x0000_0020;
    const WS_EX_LAYERED: isize = 0x0008_0000;
    const LWA_ALPHA: u32 = 0x0000_0002;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;

    const HKEY_CURRENT_USER: Hkey = -2147483647i32 as Hkey;
    const DELETE: u32 = 0x0001_0000;
    const KEY_READ: u32 = 0x0002_0019;
    const KEY_WRITE: u32 = 0x0002_0006;
    const REG_OPTION_NON_VOLATILE: u32 = 0;
    const REG_SZ: u32 = 1;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    const ERROR_SUCCESS: u32 = 0;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_ALREADY_EXISTS: u32 = 183;

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x0000_0001;

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    const WM_COPYDATA: u32 = 0x004A;
    const SMTO_BLOCK: u32 = 0x0001;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CryptIntegerBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CopyDataStruct {
        dw_data: usize,
        cb_data: u32,
        lp_data: *const c_void,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, new_long: isize) -> isize;
        fn SetLayeredWindowAttributes(hwnd: Hwnd, cr_key: u32, b_alpha: u8, dw_flags: u32) -> i32;
        fn OpenClipboard(hwnd_new_owner: Hwnd) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
        fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn RegisterClipboardFormatW(format: *const u16) -> u32;
        fn FindWindowExW(
            parent: Hwnd,
            child_after: Hwnd,
            class_name: *const u16,
            window_name: *const u16,
        ) -> Hwnd;
        fn SendMessageTimeoutW(
            hwnd: Hwnd,
            msg: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout: u32,
            result: *mut usize,
        ) -> isize;
    }

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: Hwnd,
            dw_attribute: u32,
            pv_attribute: *const c_void,
            cb_attribute: u32,
        ) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: Hkey,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            out_type: *mut u32,
            data: *mut c_void,
            data_len: *mut u32,
        ) -> u32;
        fn RegCreateKeyExW(
            hkey: Hkey,
            sub_key: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            sam_desired: u32,
            security_attributes: *const c_void,
            out_result: *mut Hkey,
            out_disposition: *mut u32,
        ) -> u32;
        fn RegOpenKeyExW(
            hkey: Hkey,
            sub_key: *const u16,
            options: u32,
            sam_desired: u32,
            out_result: *mut Hkey,
        ) -> u32;
        fn RegSetValueExW(
            hkey: Hkey,
            value_name: *const u16,
            reserved: u32,
            dw_type: u32,
            data: *const u8,
            cb_data: u32,
        ) -> u32;
        fn RegDeleteValueW(hkey: Hkey, value_name: *const u16) -> u32;
        fn RegDeleteTreeW(hkey: Hkey, sub_key: *const u16) -> u32;
        fn RegCloseKey(hkey: Hkey) -> u32;
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const CryptIntegerBlob,
            data_descr: *const u16,
            optional_entropy: *const CryptIntegerBlob,
            reserved: *const c_void,
            prompt_struct: *const c_void,
            flags: u32,
            data_out: *mut CryptIntegerBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const CryptIntegerBlob,
            data_descr: *mut *mut u16,
            optional_entropy: *const CryptIntegerBlob,
            reserved: *const c_void,
            prompt_struct: *const c_void,
            flags: u32,
            data_out: *mut CryptIntegerBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(hmem: *mut c_void) -> *mut c_void;
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(hmem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(hmem: *mut c_void) -> i32;
        fn GlobalSize(hmem: *mut c_void) -> usize;
        fn GlobalFree(hmem: *mut c_void) -> *mut c_void;
        fn CreateMutexW(
            mutex_attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> Handle;
        fn CloseHandle(object: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open(hwnd: Hwnd) -> Result<Self, String> {
            for attempt in 0..8 {
                // SAFETY: `OpenClipboard` accepts 0 or a valid HWND.
                let ok = unsafe { OpenClipboard(hwnd) };
                if ok != 0 {
                    return Ok(Self);
                }
                if attempt < 7 {
                    std::thread::sleep(Duration::from_millis(8));
                }
            }
            Err("クリップボードを開けませんでした（他プロセスが使用中）".to_owned())
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            // SAFETY: Paired with successful `OpenClipboard`.
            unsafe {
                CloseClipboard();
            }
        }
    }

    struct GlobalLockGuard {
        hmem: *mut c_void,
        ptr: *mut u8,
    }

    impl GlobalLockGuard {
        fn lock(hmem: *mut c_void) -> Result<Self, String> {
            if hmem.is_null() {
                return Err("クリップボードメモリハンドルが空です".to_owned());
            }
            // SAFETY: Valid `HGLOBAL` from `GetClipboardData` or `GlobalAlloc`.
            let ptr = unsafe { GlobalLock(hmem).cast::<u8>() };
            if ptr.is_null() {
                return Err("GlobalLock に失敗しました".to_owned());
            }
            Ok(Self { hmem, ptr })
        }
    }

    impl Drop for GlobalLockGuard {
        fn drop(&mut self) {
            // SAFETY: Paired with successful `GlobalLock`.
            unsafe {
                GlobalUnlock(self.hmem);
            }
        }
    }

    fn set_clipboard_bytes(hwnd: Hwnd, format: u32, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_CLIPBOARD_BYTES {
            return Err("クリップボード書き込みサイズが上限を超えています".to_owned());
        }
        let _clip = ClipboardGuard::open(hwnd)?;
        // SAFETY: Clipboard is open on this thread.
        unsafe {
            if EmptyClipboard() == 0 {
                return Err("EmptyClipboard に失敗しました".to_owned());
            }
            let alloc_len = bytes.len().max(1);
            let hmem = GlobalAlloc(GMEM_MOVEABLE, alloc_len);
            if hmem.is_null() {
                return Err("GlobalAlloc に失敗しました".to_owned());
            }
            {
                let lock = match GlobalLockGuard::lock(hmem) {
                    Ok(lock) => lock,
                    Err(err) => {
                        GlobalFree(hmem);
                        return Err(err);
                    }
                };
                if !bytes.is_empty() {
                    ptr::copy_nonoverlapping(bytes.as_ptr(), lock.ptr, bytes.len());
                }
            }
            // Ownership of `hmem` transfers to the OS on success; only call
            // `GlobalFree` if `SetClipboardData` fails.
            let transferred = SetClipboardData(format, hmem);
            if transferred.is_null() {
                GlobalFree(hmem);
                return Err("SetClipboardData に失敗しました".to_owned());
            }
        }
        Ok(())
    }

    fn get_clipboard_bytes(hwnd: Hwnd, format: u32) -> Result<Vec<u8>, String> {
        // SAFETY: Querying format availability does not require open clipboard.
        if unsafe { IsClipboardFormatAvailable(format) } == 0 {
            return Ok(Vec::new());
        }
        let _clip = ClipboardGuard::open(hwnd)?;
        // SAFETY: Clipboard is open.
        unsafe {
            let hmem = GetClipboardData(format);
            if hmem.is_null() {
                return Ok(Vec::new());
            }
            let size = GlobalSize(hmem);
            if size == 0 {
                return Ok(Vec::new());
            }
            if size > MAX_CLIPBOARD_BYTES {
                return Err("クリップボードデータが上限 (16 MiB) を超えています".to_owned());
            }
            let lock = GlobalLockGuard::lock(hmem)?;
            let slice = std::slice::from_raw_parts(lock.ptr, size);
            Ok(slice.to_vec())
        }
    }

    fn html_clipboard_format() -> u32 {
        let name = wide_nul("HTML Format");
        // SAFETY: Static NUL-terminated format name.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    }

    pub fn clipboard_read_text(hwnd: Hwnd) -> Result<String, String> {
        let raw = get_clipboard_bytes(hwnd, CF_UNICODETEXT)?;
        if raw.len() < 2 {
            return Ok(String::new());
        }
        let mut u16s = Vec::with_capacity(raw.len() / 2);
        for pair in raw.chunks_exact(2) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                break;
            }
            u16s.push(unit);
        }
        Ok(String::from_utf16_lossy(&u16s))
    }

    pub fn clipboard_write_text(hwnd: Hwnd, text: &str) -> Result<(), String> {
        let wide = wide_nul(text);
        let mut bytes = Vec::with_capacity(wide.len() * 2);
        for unit in wide {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        set_clipboard_bytes(hwnd, CF_UNICODETEXT, &bytes)
    }

    pub fn clipboard_read_html(hwnd: Hwnd) -> Result<String, String> {
        let fmt = html_clipboard_format();
        if fmt == 0 {
            return Err("HTML Format の登録に失敗しました".to_owned());
        }
        let raw = get_clipboard_bytes(hwnd, fmt)?;
        if raw.is_empty() {
            return Ok(String::new());
        }
        Ok(parse_cf_html(&raw).unwrap_or_default())
    }

    pub fn clipboard_write_html(hwnd: Hwnd, fragment: &str) -> Result<(), String> {
        let fmt = html_clipboard_format();
        if fmt == 0 {
            return Err("HTML Format の登録に失敗しました".to_owned());
        }
        let mut payload = build_cf_html(fragment).into_bytes();
        payload.push(0);
        set_clipboard_bytes(hwnd, fmt, &payload)
    }

    pub fn clipboard_read_image_json(hwnd: Hwnd) -> Result<String, String> {
        let dib = get_clipboard_bytes(hwnd, CF_DIB)?;
        if dib.is_empty() {
            return Ok("null".to_owned());
        }
        let (width, height, rgba) = dib_to_rgba(&dib)?;
        let png = encode_rgba_as_stored_png(width, height, &rgba)?;
        let b64 = base64_encode(&png);
        Ok(format!(
            "{{\"width\":{width},\"height\":{height},\"pngBase64\":\"{b64}\",\"dataUrl\":\"data:image/png;base64,{b64}\"}}"
        ))
    }

    pub fn clipboard_write_image_dib(hwnd: Hwnd, dib: &[u8]) -> Result<(), String> {
        let _ = dib_to_rgba(dib)?;
        set_clipboard_bytes(hwnd, CF_DIB, dib)
    }

    pub fn clipboard_clear(hwnd: Hwnd) -> Result<(), String> {
        let _clip = ClipboardGuard::open(hwnd)?;
        // SAFETY: Clipboard is open.
        if unsafe { EmptyClipboard() } == 0 {
            return Err("EmptyClipboard に失敗しました".to_owned());
        }
        Ok(())
    }

    pub fn autostart_is_enabled(app_name: &str) -> Result<bool, String> {
        let clean = validate_registry_app_name(app_name)?;
        let subkey = wide_nul("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let val_name = wide_nul(&clean);
        let mut size: u32 = 0;
        // SAFETY: Querying value size with NULL buffer.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                val_name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut size,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(size > 2)
        } else if status == ERROR_FILE_NOT_FOUND {
            Ok(false)
        } else {
            Err(format!("自動起動状態の確認に失敗しました (code={status})"))
        }
    }

    pub fn autostart_set_enabled(
        app_name: &str,
        enabled: bool,
        command_line: &str,
    ) -> Result<(), String> {
        let clean = validate_registry_app_name(app_name)?;
        let subkey = wide_nul("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let val_name = wide_nul(&clean);
        let mut hkey: Hkey = 0;
        // SAFETY: Opening or creating HKCU Run key for write (fresh profiles may lack `Run`).
        let open_status = if enabled {
            unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    ptr::null(),
                    &raw mut hkey,
                    ptr::null_mut(),
                )
            }
        } else {
            unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    0,
                    KEY_WRITE,
                    &raw mut hkey,
                )
            }
        };
        if !enabled && open_status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if open_status != ERROR_SUCCESS || hkey == 0 {
            return Err(format!(
                "Run レジストリキーを開けませんでした (code={open_status})"
            ));
        }
        let res = if enabled {
            let cmd_w = wide_nul(command_line);
            let cb_data = (cmd_w.len() * 2) as u32;
            // SAFETY: `cmd_w` is NUL-terminated UTF-16 of `cb_data` bytes.
            let st = unsafe {
                RegSetValueExW(
                    hkey,
                    val_name.as_ptr(),
                    0,
                    REG_SZ,
                    cmd_w.as_ptr().cast(),
                    cb_data,
                )
            };
            if st == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(format!("自動起動の登録に失敗しました (code={st})"))
            }
        } else {
            // SAFETY: Deleting value on open key.
            let st = unsafe { RegDeleteValueW(hkey, val_name.as_ptr()) };
            if st == ERROR_SUCCESS || st == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("自動起動の解除に失敗しました (code={st})"))
            }
        };
        unsafe {
            RegCloseKey(hkey);
        }
        res
    }

    pub fn register_uri_scheme(scheme: &str, command_line: &str) -> Result<(), String> {
        let clean = validate_custom_uri_scheme(scheme)?;
        let root_path = format!("Software\\Classes\\{clean}");
        let cmd_path = format!("{root_path}\\shell\\open\\command");
        let root_w = wide_nul(&root_path);
        let cmd_w = wide_nul(&cmd_path);

        let mut root_key: Hkey = 0;
        // SAFETY: Creating per-user `HKCU\Software\Classes\<scheme>`.
        let st = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                root_w.as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                ptr::null(),
                &raw mut root_key,
                ptr::null_mut(),
            )
        };
        if st != ERROR_SUCCESS || root_key == 0 {
            return Err(format!("URI スキームキーの作成に失敗しました (code={st})"));
        }
        let desc_w = wide_nul(&format!("URL:{clean} Protocol"));
        let empty_w = wide_nul("");
        let url_proto_name = wide_nul("URL Protocol");
        // SAFETY: Valid UTF-16 values on open `root_key`.
        unsafe {
            let _ = RegSetValueExW(
                root_key,
                ptr::null(),
                0,
                REG_SZ,
                desc_w.as_ptr().cast(),
                (desc_w.len() * 2) as u32,
            );
            let _ = RegSetValueExW(
                root_key,
                url_proto_name.as_ptr(),
                0,
                REG_SZ,
                empty_w.as_ptr().cast(),
                (empty_w.len() * 2) as u32,
            );
            RegCloseKey(root_key);
        }

        let mut cmd_key: Hkey = 0;
        // SAFETY: Creating `shell\open\command` key.
        let st_cmd = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                cmd_w.as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                ptr::null(),
                &raw mut cmd_key,
                ptr::null_mut(),
            )
        };
        if st_cmd != ERROR_SUCCESS || cmd_key == 0 {
            return Err(format!(
                "URI スキームコマンドキーの作成に失敗しました (code={st_cmd})"
            ));
        }
        let cmd_val_w = wide_nul(command_line);
        // SAFETY: Setting default value of `shell\open\command`.
        let st_set = unsafe {
            RegSetValueExW(
                cmd_key,
                ptr::null(),
                0,
                REG_SZ,
                cmd_val_w.as_ptr().cast(),
                (cmd_val_w.len() * 2) as u32,
            )
        };
        unsafe {
            RegCloseKey(cmd_key);
        }
        if st_set == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "URI スキームコマンドの設定に失敗しました (code={st_set})"
            ))
        }
    }

    pub fn unregister_uri_scheme(scheme: &str) -> Result<(), String> {
        let clean = validate_custom_uri_scheme(scheme)?;
        let classes_w = wide_nul("Software\\Classes");
        let scheme_w = wide_nul(&clean);
        let mut classes_key: Hkey = 0;
        // SAFETY: Opening `HKCU\Software\Classes` with delete & enumerate permissions for `RegDeleteTreeW`.
        let st = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                classes_w.as_ptr(),
                0,
                DELETE | KEY_READ | KEY_WRITE,
                &raw mut classes_key,
            )
        };
        if st != ERROR_SUCCESS || classes_key == 0 {
            return Err(format!("Software\\Classes を開けませんでした (code={st})"));
        }
        // SAFETY: Deleting only the validated custom scheme subtree.
        let del_st = unsafe { RegDeleteTreeW(classes_key, scheme_w.as_ptr()) };
        unsafe {
            RegCloseKey(classes_key);
        }
        if del_st == ERROR_SUCCESS || del_st == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("URI スキームの削除に失敗しました (code={del_st})"))
        }
    }

    pub fn forward_to_running_instance(instance_tag: &str, json_payload: &str) -> bool {
        if json_payload.len() > MAX_COPYDATA_BYTES {
            return false;
        }
        let win_name = wide_nul(&instance_window_name(instance_tag));
        // SAFETY: Searching top-level windows for the primary instance's OS event window.
        let target_hwnd = unsafe { FindWindowExW(0, 0, win_name.as_ptr(), win_name.as_ptr()) };
        if target_hwnd == 0 {
            return false;
        }
        let cds = CopyDataStruct {
            dw_data: DN_COPYDATA_MAGIC,
            cb_data: json_payload.len() as u32,
            lp_data: json_payload.as_ptr().cast(),
        };
        let mut result: usize = 0;
        // SAFETY: `cds` and `json_payload` remain alive for the synchronous `SendMessageTimeoutW` call.
        let sent = unsafe {
            SendMessageTimeoutW(
                target_hwnd,
                WM_COPYDATA,
                0,
                (&raw const cds) as isize,
                SMTO_BLOCK | SMTO_ABORTIFHUNG,
                2000,
                &raw mut result,
            )
        };
        sent != 0 && result != 0
    }

    pub fn apply_dwm_rounded_corners(hwnd: Hwnd) {
        if hwnd == 0 {
            return;
        }
        let pref: u32 = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const pref).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    pub fn apply_dwm_dark_mode(hwnd: Hwnd, dark: bool) {
        if hwnd == 0 {
            return;
        }
        let value: i32 = i32::from(dark);
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const value).cast(),
                std::mem::size_of::<i32>() as u32,
            );
        }
    }

    pub fn set_window_opacity(hwnd: Hwnd, alpha: u8) -> Result<(), String> {
        if hwnd == 0 {
            return Err("ウィンドウハンドルが取得できません".to_owned());
        }
        unsafe {
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if (ex_style & WS_EX_LAYERED) == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED);
            }
            if SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA) == 0 {
                return Err("SetLayeredWindowAttributes に失敗しました".to_owned());
            }
        }
        Ok(())
    }

    pub fn set_window_click_through(hwnd: Hwnd, enabled: bool) -> Result<(), String> {
        if hwnd == 0 {
            return Err("ウィンドウハンドルが取得できません".to_owned());
        }
        unsafe {
            let mut ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if enabled {
                ex_style |= WS_EX_TRANSPARENT | WS_EX_LAYERED;
            } else {
                ex_style &= !WS_EX_TRANSPARENT;
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);
        }
        Ok(())
    }

    pub fn detect_system_dark_mode() -> Option<bool> {
        let subkey = wide_nul("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let value = wide_nul("AppsUseLightTheme");
        let mut light_theme: u32 = 1;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                ptr::null_mut(),
                (&raw mut light_theme).cast(),
                &raw mut size,
            )
        };
        if status == ERROR_SUCCESS {
            Some(light_theme == 0)
        } else {
            None
        }
    }

    pub fn dpapi_encrypt(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let mut input_buf = plaintext.to_vec();
        let data_in = CryptIntegerBlob {
            cb_data: u32::try_from(input_buf.len())
                .map_err(|_| "暗号化データが大きすぎます".to_owned())?,
            pb_data: if input_buf.is_empty() {
                ptr::null_mut()
            } else {
                input_buf.as_mut_ptr()
            },
        };
        let mut data_out = CryptIntegerBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &data_in,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut data_out,
            )
        };
        if ok == 0 {
            return Err("DPAPI CryptProtectData に失敗しました".to_owned());
        }
        let out = if data_out.pb_data.is_null() || data_out.cb_data == 0 {
            Vec::new()
        } else {
            let slice =
                unsafe { std::slice::from_raw_parts(data_out.pb_data, data_out.cb_data as usize) };
            let copied = slice.to_vec();
            unsafe {
                LocalFree(data_out.pb_data.cast());
            }
            copied
        };
        Ok(out)
    }

    pub fn dpapi_decrypt(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let mut input_buf = ciphertext.to_vec();
        let data_in = CryptIntegerBlob {
            cb_data: u32::try_from(input_buf.len())
                .map_err(|_| "復号データが大きすぎます".to_owned())?,
            pb_data: if input_buf.is_empty() {
                ptr::null_mut()
            } else {
                input_buf.as_mut_ptr()
            },
        };
        let mut data_out = CryptIntegerBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &data_in,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut data_out,
            )
        };
        if ok == 0 {
            return Err("DPAPI CryptUnprotectData に失敗しました".to_owned());
        }
        let out = if data_out.pb_data.is_null() || data_out.cb_data == 0 {
            Vec::new()
        } else {
            let slice =
                unsafe { std::slice::from_raw_parts(data_out.pb_data, data_out.cb_data as usize) };
            let copied = slice.to_vec();
            unsafe {
                LocalFree(data_out.pb_data.cast());
            }
            copied
        };
        Ok(out)
    }

    pub struct SingleInstanceLock {
        handle: Handle,
    }

    impl SingleInstanceLock {
        pub fn acquire(name: &str) -> Result<Option<Self>, String> {
            let wide = wide_nul(name);
            let handle = unsafe { CreateMutexW(ptr::null(), 0, wide.as_ptr()) };
            if handle == 0 {
                return Err("単一インスタンス Mutex の作成に失敗しました".to_owned());
            }
            let last_err = unsafe { GetLastError() };
            if last_err == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(handle);
                }
                return Ok(None);
            }
            Ok(Some(Self { handle }))
        }
    }

    impl Drop for SingleInstanceLock {
        fn drop(&mut self) {
            if self.handle != 0 {
                unsafe {
                    CloseHandle(self.handle);
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use win32::SingleInstanceLock;

#[cfg(unix)]
pub use crate::unix_platform::UnixSingleInstanceLock as SingleInstanceLock;

#[cfg(not(any(target_os = "windows", unix)))]
pub struct SingleInstanceLock;

#[cfg(not(any(target_os = "windows", unix)))]
impl SingleInstanceLock {
    pub fn acquire(_name: &str) -> Result<Option<Self>, String> {
        Ok(Some(Self))
    }
}

pub fn apply_dwm_rounded_corners(hwnd: isize) {
    #[cfg(target_os = "windows")]
    win32::apply_dwm_rounded_corners(hwnd);
    #[cfg(not(target_os = "windows"))]
    let _ = hwnd;
}

pub fn apply_dwm_dark_mode(hwnd: isize, dark: bool) {
    #[cfg(target_os = "windows")]
    win32::apply_dwm_dark_mode(hwnd, dark);
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, dark);
}

pub fn set_window_opacity(hwnd: isize, opacity: f64) -> Result<(), String> {
    let alpha = opacity_to_alpha(opacity).ok_or_else(|| "不正な不透明度です".to_owned())?;
    #[cfg(target_os = "windows")]
    {
        win32::set_window_opacity(hwnd, alpha)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd, alpha);
        Err("ウィンドウ不透明度変更は Windows のみ対応です".to_owned())
    }
}

pub fn set_window_click_through(hwnd: isize, enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::set_window_click_through(hwnd, enabled)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd, enabled);
        Err("クリックスルー切替は Windows のみ対応です".to_owned())
    }
}

pub fn detect_system_dark_mode() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        win32::detect_system_dark_mode()
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::unix_platform::detect_system_dark_mode()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

pub fn dpapi_encrypt(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::dpapi_encrypt(plaintext)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = plaintext;
        Err("safeStorage (DPAPI) は Windows のみ対応です".to_owned())
    }
}

pub fn dpapi_decrypt(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::dpapi_decrypt(ciphertext)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = ciphertext;
        Err("safeStorage (DPAPI) は Windows のみ対応です".to_owned())
    }
}

pub fn clipboard_read_text(hwnd: isize) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_read_text(hwnd)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::clipboard_read_text()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        Err("クリップボードはこの OS では未対応です".to_owned())
    }
}

pub fn clipboard_write_text(hwnd: isize, text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_write_text(hwnd, text)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::clipboard_write_text(text)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, text);
        Err("クリップボードはこの OS では未対応です".to_owned())
    }
}

pub fn clipboard_read_html(hwnd: isize) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_read_html(hwnd)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::clipboard_read_html()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        Err("HTML クリップボードはこの OS では未対応です".to_owned())
    }
}

pub fn clipboard_write_html(hwnd: isize, fragment: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_write_html(hwnd, fragment)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::clipboard_write_html(fragment)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, fragment);
        Err("HTML クリップボードはこの OS では未対応です".to_owned())
    }
}

pub fn clipboard_read_image_json(hwnd: isize) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_read_image_json(hwnd)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        let png = crate::unix_platform::clipboard_read_image_png()?;
        if png.len() > MAX_CLIPBOARD_BYTES {
            return Err("Clipboard image exceeds the size limit".to_owned());
        }
        let (width, height) = crate::unix_platform::parse_png_dimensions(&png)
            .ok_or_else(|| "Clipboard PNG header is invalid".to_owned())?;
        let b64 = base64_encode(&png);
        Ok(format!(
            "{{\"width\":{width},\"height\":{height},\"dataUrl\":\"data:image/png;base64,{b64}\"}}"
        ))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        Err("Image clipboard is unsupported on this OS".to_owned())
    }
}
pub fn clipboard_write_image_dib(hwnd: isize, dib: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_write_image_dib(hwnd, dib)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let (width, height, rgba) = dib_to_rgba(dib)?;
        clipboard_write_image_rgba(hwnd, width, height, &rgba)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, dib);
        Err("Image clipboard is unsupported on this OS".to_owned())
    }
}

pub fn clipboard_write_image_rgba(
    hwnd: isize,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let dib = rgba_to_dib(width, height, rgba)?;
        win32::clipboard_write_image_dib(hwnd, &dib)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        if rgba.len() > MAX_CLIPBOARD_BYTES {
            return Err("Clipboard image exceeds 16 MiB".to_owned());
        }
        let png = encode_rgba_as_stored_png(width, height, rgba)?;
        if png.len() > MAX_CLIPBOARD_BYTES {
            return Err("Clipboard PNG exceeds 16 MiB".to_owned());
        }
        crate::unix_platform::clipboard_write_image_png(&png)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, width, height, rgba);
        Err("Image clipboard is unsupported on this OS".to_owned())
    }
}

pub fn clipboard_clear(hwnd: isize) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::clipboard_clear(hwnd)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::clipboard_clear()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        Err("クリップボードはこの OS では未対応です".to_owned())
    }
}

pub fn autostart_is_enabled(app_name: &str) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = validate_registry_app_name(app_name)?;
        win32::autostart_is_enabled(app_name)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::unix_platform::autostart_is_enabled(app_name)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = app_name;
        Err("自動起動設定はこの OS では未対応です".to_owned())
    }
}

#[allow(dead_code)]
pub fn autostart_set_enabled(
    app_name: &str,
    enabled: bool,
    command_line: &str,
) -> Result<(), String> {
    let _ = validate_registry_app_name(app_name)?;
    #[cfg(target_os = "windows")]
    {
        win32::autostart_set_enabled(app_name, enabled, command_line)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (enabled, command_line);
        Err(
            "自動起動設定はこの OS では autostart_set_enabled_for_exe を使用してください"
                .to_owned(),
        )
    }
}

pub fn autostart_set_enabled_for_exe(
    app_name: &str,
    enabled: bool,
    exe: &Path,
    args: &[&str],
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let _ = validate_registry_app_name(app_name)?;
        let cmd = format_win32_command_line(exe, args);
        win32::autostart_set_enabled(app_name, enabled, &cmd)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::unix_platform::autostart_set_enabled_args(app_name, enabled, exe, args)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (app_name, enabled, exe, args);
        Err("自動起動設定はこの OS では未対応です".to_owned())
    }
}

#[allow(dead_code)]
pub fn register_uri_scheme(scheme: &str, command_line: &str) -> Result<(), String> {
    let _ = validate_custom_uri_scheme(scheme)?;
    #[cfg(target_os = "windows")]
    {
        win32::register_uri_scheme(scheme, command_line)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = command_line;
        Err("URI スキーム登録は Windows のみ対応です".to_owned())
    }
}

pub fn register_uri_scheme_for_exe(
    scheme: &str,
    app_name: &str,
    exe: &Path,
    args: &[&str],
) -> Result<(), String> {
    let clean = validate_custom_uri_scheme(scheme)?;
    #[cfg(target_os = "windows")]
    {
        let _ = app_name;
        let mut full_args = args.to_vec();
        full_args.push("--deep-link");
        full_args.push("%1");
        let command_line = format_win32_command_line(exe, &full_args);
        win32::register_uri_scheme(&clean, &command_line)
    }
    #[cfg(target_os = "linux")]
    {
        crate::unix_platform::register_uri_scheme_args(&clean, app_name, exe, args)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = (app_name, exe, args);
        let _ = clean;
        Err("URI \u{30B9}\u{30AD}\u{30FC}\u{30E0}\u{767B}\u{9332}\u{306F}\u{3053}\u{306E} OS \u{3067}\u{306F}\u{672A}\u{5BFE}\u{5FDC}\u{3067}\u{3059}".to_owned())
    }
}
pub fn unregister_uri_scheme(scheme: &str) -> Result<(), String> {
    let clean = validate_custom_uri_scheme(scheme)?;
    #[cfg(target_os = "windows")]
    {
        win32::unregister_uri_scheme(&clean)
    }
    #[cfg(target_os = "linux")]
    {
        crate::unix_platform::unregister_uri_scheme(&clean)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = clean;
        Err("URI scheme removal is unsupported on this OS".to_owned())
    }
}
pub fn forward_to_running_instance(instance_tag: &str, json_payload: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        win32::forward_to_running_instance(instance_tag, json_payload)
    }
    #[cfg(unix)]
    {
        crate::unix_platform::forward_to_running_instance(instance_tag, json_payload)
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = (instance_tag, json_payload);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_and_limits() {
        for sample in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            "日本語データ😀".as_bytes(),
        ] {
            let encoded = base64_encode(sample);
            let decoded = base64_decode(&encoded, 1024).unwrap();
            assert_eq!(decoded, sample);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert!(base64_decode("Zm9vYmFy", 4).is_err());
        assert!(base64_decode("!!!invalid!!!", 1024).is_err());
        assert!(base64_decode("Zm=v", 1024).is_err());
    }

    #[test]
    fn hotkey_parser_validates_modifiers_and_keys() {
        assert_eq!(
            parse_hotkey("Ctrl+Alt+U").unwrap(),
            (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, u32::from(b'U'))
        );
        assert_eq!(
            parse_hotkey("Control + Shift + F12").unwrap(),
            (MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, 0x7B)
        );
        assert_eq!(
            parse_hotkey("Win+Space").unwrap(),
            (MOD_WIN | MOD_NOREPEAT, 0x20)
        );
        assert!(parse_hotkey("U").is_err());
        assert!(parse_hotkey("Ctrl+").is_err());
        assert!(parse_hotkey("Ctrl+A+B").is_err());
        assert!(parse_hotkey("").is_err());
    }

    #[test]
    fn opacity_clamps_and_rejects_nan() {
        assert_eq!(opacity_to_alpha(1.0), Some(255));
        assert_eq!(opacity_to_alpha(0.5), Some(128));
        assert_eq!(opacity_to_alpha(0.0), Some(26));
        assert_eq!(opacity_to_alpha(2.0), Some(255));
        assert_eq!(opacity_to_alpha(f64::NAN), None);
        assert_eq!(opacity_to_alpha(f64::INFINITY), None);
    }

    #[test]
    fn suggested_filename_sanitizes_traversal_and_reserved() {
        assert_eq!(sanitize_suggested_filename("report.csv"), "report.csv");
        assert_eq!(
            sanitize_suggested_filename("../../evil/secret:name?.txt"),
            "secretname.txt"
        );
        assert_eq!(sanitize_suggested_filename("C:\\Windows\\a.png"), "a.png");
        assert_eq!(sanitize_suggested_filename("..."), "download.bin");
        assert_eq!(sanitize_suggested_filename(""), "download.bin");
    }

    #[test]
    fn devtools_addr_restricts_to_loopback() {
        assert_eq!(
            validate_devtools_addr("127.0.0.1:7000").unwrap(),
            "127.0.0.1:7000"
        );
        assert_eq!(
            validate_devtools_addr("localhost:9222").unwrap(),
            "127.0.0.1:9222"
        );
        assert_eq!(validate_devtools_addr("[::1]:7000").unwrap(), "[::1]:7000");
        assert!(validate_devtools_addr("0.0.0.0:7000").is_err());
        assert!(validate_devtools_addr("192.168.1.5:7000").is_err());
        assert!(validate_devtools_addr("127.0.0.1:0").is_err());
    }

    #[test]
    fn capability_file_registry_scopes_reads_and_evicts() {
        let dir = std::env::temp_dir().join(format!("dn-cap-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("sample.txt");
        std::fs::write(&file, "こんにちは😀").unwrap();

        let mut reg = FileAccessRegistry::new();
        let json = reg.register_batch_json(&[file.clone()]);
        assert!(json.contains("\"name\":\"sample.txt\""));
        assert!(!json.contains(&dir.to_string_lossy().to_string()));
        let token = reg.order.back().unwrap().clone();
        assert_eq!(reg.read_text(&token).unwrap(), "こんにちは😀");
        assert_eq!(
            base64_decode(&reg.read_base64(&token).unwrap(), 1024).unwrap(),
            "こんにちは😀".as_bytes()
        );
        assert!(reg.read_text("dnfile-forged-999").is_err());

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn strict_json_u32_requires_a_complete_integer_token() {
        assert_eq!(
            json_get_strict_u32(r#"{"width":800,"height":120}"#, "width"),
            Some(800)
        );
        assert_eq!(
            json_get_strict_u32("{\"width\":4294967295 \t\r\n}", "width"),
            Some(u32::MAX)
        );
        assert_eq!(json_get_strict_u32(r#"{"width":0}"#, "width"), Some(0));
        for value in [
            "800.5",
            "1e3",
            "-1",
            "+1",
            "\"800\"",
            "８００",
            "4294967296",
            "01",
            "800 .5",
            "800px",
        ] {
            assert_eq!(
                json_get_strict_u32(&format!(r#"{{"width":{value}}}"#), "width"),
                None,
                "{value}"
            );
        }
        assert_eq!(json_get_strict_u32(r#"{"width":800"#, "width"), None);
    }

    #[test]
    fn flat_json_helpers_parse_escapes_arrays_and_u32() {
        let payload = r#"{"title":"通知\nテスト","body":"😀","width":320,"multiple":true,"extensions":["txt","md"]}"#;
        assert_eq!(
            json_get_string(payload, "title").as_deref(),
            Some("通知\nテスト")
        );
        assert_eq!(json_get_string(payload, "body").as_deref(), Some("😀"));
        assert_eq!(json_get_u32(payload, "width"), Some(320));
        assert_eq!(json_get_bool(payload, "multiple"), Some(true));
        assert_eq!(
            json_get_string_array(payload, "extensions"),
            Some(vec!["txt".to_owned(), "md".to_owned()])
        );
        assert_eq!(json_get_string(payload, "missing"), None);
    }

    #[test]
    fn cf_html_roundtrips_multibyte_utf8_with_exact_offsets() {
        let fragment = "<p>日本語 <b>テスト</b> 😀</p>";
        let cf = build_cf_html(fragment);
        assert!(cf.starts_with("Version:0.9\r\nStartHTML:0000000105\r\n"));
        let parsed = parse_cf_html(cf.as_bytes()).unwrap();
        assert_eq!(parsed, fragment);
    }

    #[test]
    fn dib_rgba_bmp_png_and_pdf_codecs_roundtrip() {
        // 2x2 RGBA test image (Red, Green, Blue, White)
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, // row 0
            0, 0, 255, 255, 255, 255, 255, 128, // row 1
        ];
        let dib = rgba_to_dib(2, 2, &rgba).unwrap();
        let (w, h, decoded_rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(decoded_rgba, rgba);

        let bmp = encode_dib_as_bmp(&dib).unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        let dib_from_bmp = decode_bmp_to_dib(&bmp).unwrap();
        assert_eq!(dib_from_bmp, dib);

        let png = encode_rgba_as_stored_png(2, 2, &rgba).unwrap();
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));

        let pdf = encode_rgba_as_raster_pdf(2, 2, &rgba).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn registry_uri_scheme_and_crash_report_helpers_validate() {
        assert_eq!(
            validate_registry_app_name("Draconium.Folio").unwrap(),
            "Draconium.Folio"
        );
        assert!(validate_registry_app_name("bad\\name").is_err());
        assert!(validate_registry_app_name("").is_err());

        let cmd = format_win32_command_line(
            Path::new(r"C:\Program Files\Draconium\draconium.exe"),
            &["--workspace", "--tray"],
        );
        assert_eq!(
            cmd,
            r#""C:\Program Files\Draconium\draconium.exe" --workspace --tray"#
        );

        assert_eq!(
            validate_custom_uri_scheme("Draconium").unwrap(),
            "draconium"
        );
        assert!(validate_custom_uri_scheme("http").is_err());
        assert!(validate_custom_uri_scheme("https").is_err());
        assert!(validate_custom_uri_scheme("file").is_err());
        assert!(validate_custom_uri_scheme("javascript").is_err());
        assert!(validate_custom_uri_scheme("dn").is_err());
        assert!(validate_custom_uri_scheme("ms-settings").is_err());

        assert_eq!(
            instance_window_name("Local\\Draconium.Workspace"),
            "Draconium.TrayWindow.LocalDraconiumWorkspace"
        );

        let report = CrashReport::new(
            "Draconium Folio",
            "http://user:pass@127.0.0.1:5173/workspace.html?token=secret#hash",
            "webview-crashed",
            "pipeline panic",
            Some("frame0\nframe1".to_owned()),
        );
        assert_eq!(report.url_sanitized, "http://127.0.0.1:5173/workspace.html");
        let json = report.to_json();
        assert!(!json.contains("secret"));
        assert!(!json.contains("pass"));

        let crash_dir = std::env::temp_dir().join(format!("dn-crash-test-{}", std::process::id()));
        let written = write_crash_report(&crash_dir, &report).unwrap();
        assert!(written.is_file());
        let _ = std::fs::remove_file(&written);
        let _ = std::fs::remove_dir(&crash_dir);
    }
}

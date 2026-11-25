/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Host handler for Draco Shot (`--shot`), a tray-resident screenshot tool
//! with minimal annotations.
//!
//! The flow (hotkey -> region select -> annotate -> copy / save) lives in the
//! page (`ui/shot.js`); this module owns the persistent, testable parts:
//! settings (`shot-settings.json`), file-name pattern expansion, and writing
//! the final PNG to the configured save folder without a dialog.
//!
//! Screen capture itself is a host primitive
//! (`draconium_host::platform::capture_desktop_image_json`) intercepted in
//! `main.rs`, so this module stays `std`-only and can be unit-tested
//! standalone via `rustc --edition 2024 --test -D warnings
//! probes/servo/src/shot.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+S";
pub const DEFAULT_FILENAME_PATTERN: &str = "shot-{yyyy}{MM}{dd}-{HH}{mm}{ss}";

const SETTINGS_FILE: &str = "shot-settings.json";
const MAX_SHOT_PNG_BYTES: usize = 32 * 1024 * 1024;
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Data directory for Draco Shot settings (`%LOCALAPPDATA%\Draconium\shot`
/// on Windows), mirroring Folio's `default_workspace_dir`.
pub fn default_shot_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA")
        && !local_app_data.is_empty()
    {
        return PathBuf::from(local_app_data).join("Draconium").join("shot");
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("draconium").join("shot");
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("draconium")
            .join("shot");
    }
    std::env::temp_dir().join("draconium-shot")
}

/// Default quick-save folder (`%USERPROFILE%\Pictures\Draco Shot`).
pub fn default_save_dir() -> PathBuf {
    if let Some(profile) = std::env::var_os("USERPROFILE")
        && !profile.is_empty()
    {
        return PathBuf::from(profile).join("Pictures").join("Draco Shot");
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join("Pictures").join("Draco Shot");
    }
    std::env::temp_dir().join("Draco Shot")
}

/// The four user-facing settings (plus a persisted `{num}` counter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotSettings {
    /// Global hotkey spec (e.g. `Ctrl+Shift+S`); applied at next launch.
    pub hotkey: String,
    /// Quick-save folder; empty means [`default_save_dir`].
    pub save_dir: String,
    /// File-name pattern with `{yyyy}`/`{MM}`/`{dd}`/`{HH}`/`{mm}`/`{ss}`/`{num}` tokens.
    pub filename_pattern: String,
    /// Also copy to the clipboard after a quick save.
    pub auto_copy: bool,
    /// Monotonic counter substituted for `{num}`.
    pub counter: u64,
}

impl Default for ShotSettings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_owned(),
            save_dir: String::new(),
            filename_pattern: DEFAULT_FILENAME_PATTERN.to_owned(),
            auto_copy: true,
            counter: 0,
        }
    }
}

impl ShotSettings {
    /// Load settings from `data_dir`, falling back to defaults per field and
    /// for a missing or unreadable file.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::from_json(&raw)
    }

    fn from_json(raw: &str) -> Self {
        let defaults = Self::default();
        Self {
            hotkey: json_get_string(raw, "hotkey")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(defaults.hotkey),
            save_dir: json_get_string(raw, "saveDir").unwrap_or_default(),
            filename_pattern: json_get_string(raw, "filenamePattern")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(defaults.filename_pattern),
            auto_copy: json_get_bool(raw, "autoCopy").unwrap_or(defaults.auto_copy),
            counter: json_get_u64(raw, "counter").unwrap_or(0),
        }
    }

    /// Persist settings atomically (temporary file + rename).
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        fs::create_dir_all(data_dir)
            .map_err(|err| format!("設定フォルダを作成できませんでした: {err}"))?;
        let path = data_dir.join(SETTINGS_FILE);
        let tmp = data_dir.join(format!("{SETTINGS_FILE}.tmp"));
        fs::write(&tmp, self.to_json())
            .map_err(|err| format!("設定を書き込めませんでした: {err}"))?;
        fs::rename(&tmp, &path).map_err(|err| format!("設定を保存できませんでした: {err}"))?;
        Ok(())
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"hotkey\":{},\"saveDir\":{},\"filenamePattern\":{},\"autoCopy\":{},\"counter\":{}}}",
            json_escape(&self.hotkey),
            json_escape(&self.save_dir),
            json_escape(&self.filename_pattern),
            self.auto_copy,
            self.counter,
        )
    }

    /// The folder quick saves actually write to.
    pub fn effective_save_dir(&self) -> PathBuf {
        let trimmed = self.save_dir.trim();
        if trimmed.is_empty() {
            default_save_dir()
        } else {
            PathBuf::from(trimmed)
        }
    }
}

/// The hotkey spec the next launch should register, read from saved settings.
pub fn configured_hotkey(data_dir: &Path) -> String {
    ShotSettings::load(data_dir).hotkey
}

/// Broken-down calendar/clock fields used for file-name pattern expansion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTimeParts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTimeParts {
    /// UTC fields for a Unix timestamp (seconds).
    pub fn from_unix_secs(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let day_secs = secs % 86_400;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (day_secs / 3_600) as u32,
            minute: ((day_secs % 3_600) / 60) as u32,
            second: (day_secs % 60) as u32,
        }
    }

    fn now_utc() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        Self::from_unix_secs(secs)
    }
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian calendar
/// (Howard Hinnant's `civil_from_days` algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Expand a file-name pattern into a sanitized `.png` file name.
///
/// Tokens: `{yyyy}` `{MM}` `{dd}` `{HH}` `{mm}` `{ss}` `{num}`. Characters
/// Windows forbids in file names (`<>:"/\|?*` and control characters) are
/// removed, trailing dots/spaces are trimmed, the stem is capped at 120
/// characters, and `.png` is appended unless already present. An empty result
/// falls back to `shot.png`.
pub fn expand_filename_pattern(pattern: &str, parts: &DateTimeParts, counter: u64) -> String {
    let expanded = pattern
        .replace("{yyyy}", &format!("{:04}", parts.year))
        .replace("{MM}", &format!("{:02}", parts.month))
        .replace("{dd}", &format!("{:02}", parts.day))
        .replace("{HH}", &format!("{:02}", parts.hour))
        .replace("{mm}", &format!("{:02}", parts.minute))
        .replace("{ss}", &format!("{:02}", parts.second))
        .replace("{num}", &counter.to_string());
    let mut name: String = expanded
        .chars()
        .filter(|ch| !matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .filter(|ch| !ch.is_control())
        .collect();
    while name.ends_with(['.', ' ']) {
        name.pop();
    }
    if name.len() > 120 {
        name.truncate(120);
        while name.ends_with(['.', ' ']) {
            name.pop();
        }
    }
    if name.trim().is_empty() {
        return "shot.png".to_owned();
    }
    if name.to_ascii_lowercase().ends_with(".png") {
        name
    } else {
        format!("{name}.png")
    }
}

/// Pick a non-colliding path for `name` inside `dir`: the name itself, or
/// `stem-2.ext`, `stem-3.ext`, ... when earlier files exist.
pub fn resolve_collision(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_owned(), format!(".{ext}")),
        _ => (name.to_owned(), String::new()),
    };
    for n in 2..1_000 {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-1000{ext}"))
}

/// Decode standard base64 (whitespace tolerated, padding optional), rejecting
/// invalid characters and outputs larger than `max_bytes`.
pub fn base64_decode(input: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut acc_bits: u32 = 0;
    let mut padding = 0;
    for ch in input.chars() {
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            '=' => {
                padding += 1;
                continue;
            }
            ch if ch.is_whitespace() => continue,
            _ => return Err(format!("base64 に不正な文字が含まれています: {ch}")),
        };
        if padding > 0 {
            return Err("base64 のパディング位置が不正です".to_owned());
        }
        acc = (acc << 6) | value;
        acc_bits += 6;
        if acc_bits >= 8 {
            acc_bits -= 8;
            out.push((acc >> acc_bits) as u8);
            if out.len() > max_bytes {
                return Err("画像データが上限を超えています".to_owned());
            }
        }
    }
    if padding > 2 {
        return Err("base64 のパディングが不正です".to_owned());
    }
    Ok(out)
}

/// Draco Shot application state and command handler.
pub struct ShotApp {
    data_dir: PathBuf,
    settings: ShotSettings,
}

impl ShotApp {
    pub fn new(data_dir: PathBuf) -> Self {
        let settings = ShotSettings::load(&data_dir);
        Self { data_dir, settings }
    }

    /// Handles `get_settings` / `save_settings` / `save_shot` commands from
    /// `Draconium.invoke` / `invokeAsync`. (`capture_screen` is intercepted in
    /// `main.rs` because it needs the host's platform capture primitive.)
    pub fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command {
            "get_settings" => Ok(self.settings_json()),
            "save_settings" => self.save_settings(payload),
            "save_shot" => self.save_shot(payload),
            other => Err(format!("Unsupported shot command: {other}")),
        }
    }

    fn settings_json(&self) -> String {
        format!(
            "{{\"hotkey\":{},\"saveDir\":{},\"effectiveSaveDir\":{},\"filenamePattern\":{},\"autoCopy\":{}}}",
            json_escape(&self.settings.hotkey),
            json_escape(&self.settings.save_dir),
            json_escape(&self.settings.effective_save_dir().to_string_lossy()),
            json_escape(&self.settings.filename_pattern),
            self.settings.auto_copy,
        )
    }

    fn save_settings(&mut self, payload: &str) -> Result<String, String> {
        let mut next = self.settings.clone();
        if let Some(hotkey) = json_get_string(payload, "hotkey") {
            let hotkey = hotkey.trim().to_owned();
            if hotkey.is_empty() || hotkey.chars().count() > 64 {
                return Err("ホットキーの指定が不正です".to_owned());
            }
            next.hotkey = hotkey;
        }
        if let Some(save_dir) = json_get_string(payload, "saveDir") {
            if save_dir.chars().count() > 512 {
                return Err("保存先フォルダの指定が長すぎます".to_owned());
            }
            next.save_dir = save_dir.trim().to_owned();
        }
        if let Some(pattern) = json_get_string(payload, "filenamePattern") {
            let pattern = pattern.trim().to_owned();
            if pattern.is_empty() || pattern.chars().count() > 128 {
                return Err("ファイル名パターンの指定が不正です".to_owned());
            }
            next.filename_pattern = pattern;
        }
        if let Some(auto_copy) = json_get_bool(payload, "autoCopy") {
            next.auto_copy = auto_copy;
        }
        next.save(&self.data_dir)?;
        self.settings = next;
        Ok(self.settings_json())
    }

    fn save_shot(&mut self, payload: &str) -> Result<String, String> {
        let encoded = json_get_string(payload, "pngBase64")
            .ok_or_else(|| "pngBase64 が指定されていません".to_owned())?;
        let bytes = base64_decode(&encoded, MAX_SHOT_PNG_BYTES)?;
        if !bytes.starts_with(&PNG_SIGNATURE) {
            return Err("PNG ではないデータです".to_owned());
        }
        let dir = self.settings.effective_save_dir();
        fs::create_dir_all(&dir)
            .map_err(|err| format!("保存先フォルダを作成できませんでした: {err}"))?;
        self.settings.counter += 1;
        let parts = json_get_datetime(payload).unwrap_or_else(DateTimeParts::now_utc);
        let name = expand_filename_pattern(
            &self.settings.filename_pattern,
            &parts,
            self.settings.counter,
        );
        let path = resolve_collision(&dir, &name);
        fs::write(&path, &bytes).map_err(|err| format!("画像を保存できませんでした: {err}"))?;
        // Persist the bumped {num} counter (best effort: the shot is saved).
        let _ = self.settings.save(&self.data_dir);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(format!(
            "{{\"path\":{},\"fileName\":{}}}",
            json_escape(&path.to_string_lossy()),
            json_escape(&file_name),
        ))
    }
}

/// Read the page-provided local clock (`{"localTime":{...}}`) so saved file
/// names use the user's wall-clock time instead of UTC.
fn json_get_datetime(payload: &str) -> Option<DateTimeParts> {
    let section = json_find_value(payload, "localTime")?;
    Some(DateTimeParts {
        year: json_get_u64(section, "year")? as i64,
        month: json_get_u64(section, "month")? as u32,
        day: json_get_u64(section, "day")? as u32,
        hour: json_get_u64(section, "hour")? as u32,
        minute: json_get_u64(section, "minute")? as u32,
        second: json_get_u64(section, "second")? as u32,
    })
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Locate `"key":` and return the raw JSON value text that follows it.
fn json_find_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut search_from = 0;
    while let Some(found) = json[search_from..].find(&needle) {
        let after_key = search_from + found + needle.len();
        let rest = json[after_key..].trim_start();
        if let Some(value) = rest.strip_prefix(':') {
            return Some(value.trim_start());
        }
        search_from = after_key;
    }
    None
}

fn json_get_string(json: &str, key: &str) -> Option<String> {
    let value = json_find_value(json, key)?;
    if !value.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = value[1..].chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{08}'),
                'f' => out.push('\u{0C}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() < 4 {
                        return None;
                    }
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                _ => return None,
            },
            ch => out.push(ch),
        }
    }
}

fn json_get_bool(json: &str, key: &str) -> Option<bool> {
    let value = json_find_value(json, key)?;
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_get_u64(json: &str, key: &str) -> Option<u64> {
    let value = json_find_value(json, key)?;
    let digits: String = value.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_test_dir(label: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "draco-shot-test-{}-{label}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parts_2026() -> DateTimeParts {
        DateTimeParts {
            year: 2026,
            month: 11,
            day: 25,
            hour: 1,
            minute: 2,
            second: 3,
        }
    }

    #[test]
    fn unix_secs_convert_to_civil_utc() {
        assert_eq!(
            DateTimeParts::from_unix_secs(0),
            DateTimeParts {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0
            }
        );
        // 2000-02-29 01:02:03 UTC (leap day).
        assert_eq!(
            DateTimeParts::from_unix_secs(951_782_400 + 3_723),
            DateTimeParts {
                year: 2000,
                month: 2,
                day: 29,
                hour: 1,
                minute: 2,
                second: 3
            }
        );
        // 2026-11-25 00:00:00 UTC.
        let parts = DateTimeParts::from_unix_secs(1_795_564_800);
        assert_eq!((parts.year, parts.month, parts.day), (2026, 11, 25));
    }

    #[test]
    fn filename_pattern_expands_tokens_and_sanitizes() {
        let parts = parts_2026();
        assert_eq!(
            expand_filename_pattern(DEFAULT_FILENAME_PATTERN, &parts, 1),
            "shot-20261125-010203.png"
        );
        assert_eq!(
            expand_filename_pattern("cap-{num}", &parts, 42),
            "cap-42.png"
        );
        // Forbidden characters are stripped; an existing extension is kept.
        assert_eq!(expand_filename_pattern("a/b:c*?", &parts, 1), "abc.png");
        assert_eq!(expand_filename_pattern("x.PNG", &parts, 1), "x.PNG");
        // A pattern that sanitizes to nothing falls back.
        assert_eq!(expand_filename_pattern("///", &parts, 1), "shot.png");
        assert_eq!(expand_filename_pattern("", &parts, 1), "shot.png");
    }

    #[test]
    fn base64_decode_validates_and_caps() {
        assert_eq!(base64_decode("aGVsbG8=", 16).unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs bG8", 16).unwrap(), b"hello");
        assert!(base64_decode("aGVsbG8!", 16).is_err());
        assert!(base64_decode("aGVsbG8=", 4).is_err());
        assert!(base64_decode("ab===", 16).is_err());
    }

    #[test]
    fn settings_roundtrip_through_disk() {
        let dir = temp_test_dir("settings");
        let mut settings = ShotSettings {
            hotkey: "Ctrl+Alt+P".to_owned(),
            save_dir: "D:\\Shots".to_owned(),
            filename_pattern: "cap-{num}".to_owned(),
            auto_copy: false,
            counter: 7,
        };
        settings.save(&dir).unwrap();
        let loaded = ShotSettings::load(&dir);
        assert_eq!(loaded, settings);
        settings.counter = 8;
        assert_eq!(ShotSettings::load(&dir).counter, 7);
        // Missing file yields defaults.
        let empty = temp_test_dir("settings-empty");
        assert_eq!(ShotSettings::load(&empty), ShotSettings::default());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn save_shot_writes_png_with_collision_suffix() {
        let data_dir = temp_test_dir("save-data");
        let save_dir = temp_test_dir("save-out");
        let mut app = ShotApp::new(data_dir.clone());
        app.settings.save_dir = save_dir.to_string_lossy().into_owned();
        // Minimal payload: PNG signature + padding bytes, local clock fixed.
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(b"payload");
        let encoded = base64_encode_for_test(&png);
        let payload = format!(
            "{{\"pngBase64\":\"{encoded}\",\"localTime\":{{\"year\":2026,\"month\":11,\"day\":25,\"hour\":1,\"minute\":2,\"second\":3}}}}"
        );
        let first = app.handle_request("save_shot", &payload).unwrap();
        assert!(first.contains("shot-20261125-010203.png"), "{first}");
        assert!(save_dir.join("shot-20261125-010203.png").exists());
        // Same second again -> collision suffix.
        let second = app.handle_request("save_shot", &payload).unwrap();
        assert!(second.contains("shot-20261125-010203-2.png"), "{second}");
        // Non-PNG data is rejected.
        let bad = format!(
            "{{\"pngBase64\":\"{}\"}}",
            base64_encode_for_test(b"not a png")
        );
        assert!(app.handle_request("save_shot", &bad).is_err());
        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&save_dir);
    }

    #[test]
    fn save_settings_validates_and_persists() {
        let data_dir = temp_test_dir("save-settings");
        let mut app = ShotApp::new(data_dir.clone());
        let updated = app
            .handle_request(
                "save_settings",
                "{\"hotkey\":\"Ctrl+PrintScreen\",\"autoCopy\":false}",
            )
            .unwrap();
        assert!(updated.contains("Ctrl+PrintScreen"), "{updated}");
        assert!(updated.contains("\"autoCopy\":false"), "{updated}");
        assert_eq!(configured_hotkey(&data_dir), "Ctrl+PrintScreen");
        assert!(
            app.handle_request("save_settings", "{\"hotkey\":\"\"}")
                .is_err()
        );
        assert!(app.handle_request("unknown", "").is_err());
        let _ = fs::remove_dir_all(&data_dir);
    }

    fn base64_encode_for_test(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0] as u32,
                *chunk.get(1).unwrap_or(&0) as u32,
                *chunk.get(2).unwrap_or(&0) as u32,
            ];
            let n = (b[0] << 16) | (b[1] << 8) | b[2];
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}

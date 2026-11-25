//! Window geometry and presentation state for the Draconium host.
//!
//! This module is deliberately free of `winit` / `servo` dependencies so the
//! geometry math and the `config.json` field handling can be unit-tested with
//! a plain `rustc --test` build.
//!
//! Resident HUD apps can toggle between two presentations sharing one window:
//! a compact "chips" bar and an expanded "flyout" panel. Switching between
//! them is a window resize plus a mode flag persisted in `config.json`.

/// A logical-pixel window size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalSize {
    pub width: u32,
    pub height: u32,
}

impl LogicalSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A logical-pixel window position (top-left corner, screen coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalPosition {
    pub x: i32,
    pub y: i32,
}

impl LogicalPosition {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Default logical size for standard host windows.
pub const DEFAULT_WINDOW_SIZE: LogicalSize = LogicalSize::new(1000, 720);

/// Resident HUD expanded flyout presentation size.
pub const FLYOUT_SIZE: LogicalSize = LogicalSize::new(448, 620);

/// Resident HUD compact bar presentation size.
pub const CHIPS_SIZE: LogicalSize = LogicalSize::new(448, 56);

/// Compact and expanded presentations for resident HUD windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Chips,
    Flyout,
}

impl Presentation {
    /// Logical window size for this presentation.
    pub fn size(self) -> LogicalSize {
        match self {
            Presentation::Chips => CHIPS_SIZE,
            Presentation::Flyout => FLYOUT_SIZE,
        }
    }

    /// Value stored in `config.json` under `presentation`.
    pub fn config_value(self) -> &'static str {
        match self {
            Presentation::Chips => "chips",
            Presentation::Flyout => "flyout",
        }
    }

    /// Parse the `config.json` `presentation` value.
    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "chips" => Some(Presentation::Chips),
            "flyout" => Some(Presentation::Flyout),
            _ => None,
        }
    }

    /// `config.json` keys holding this presentation's saved position.
    pub fn position_keys(self) -> (&'static str, &'static str) {
        match self {
            Presentation::Chips => ("chips_x", "chips_y"),
            Presentation::Flyout => ("flyout_x", "flyout_y"),
        }
    }
}

/// Minimum and maximum logical sizes accepted from page `window.resizeTo`
/// requests (`WebViewDelegate::request_resize_to`).
pub const MIN_REQUEST_WINDOW_SIZE: LogicalSize = LogicalSize::new(160, 56);
pub const MAX_REQUEST_WINDOW_SIZE: LogicalSize = LogicalSize::new(3840, 2160);

/// Page zoom bounds and step (Milestone C).
pub const MIN_PAGE_ZOOM: f32 = 0.5;
pub const MAX_PAGE_ZOOM: f32 = 3.0;
pub const PAGE_ZOOM_STEP: f32 = 0.1;

/// Clamp `size` into the inclusive `min`..=`max` range per axis.
pub fn clamp_size(size: LogicalSize, min: LogicalSize, max: LogicalSize) -> LogicalSize {
    LogicalSize::new(
        size.width.clamp(min.width, max.width),
        size.height.clamp(min.height, max.height),
    )
}

/// Clamp a page-requested outer window size (device pixels) into a sane range.
pub fn clamp_window_request_size(width: i32, height: i32) -> (u32, u32) {
    let w = width.max(0) as u32;
    let h = height.max(0) as u32;
    let clamped = clamp_size(
        LogicalSize::new(w, h),
        MIN_REQUEST_WINDOW_SIZE,
        MAX_REQUEST_WINDOW_SIZE,
    );
    (clamped.width, clamped.height)
}

/// Clamp a page zoom factor to the supported host range, falling back to 1.0
/// for non-finite values.
pub fn clamp_zoom(zoom: f32) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    zoom.clamp(MIN_PAGE_ZOOM, MAX_PAGE_ZOOM)
}

/// Adjust a page zoom factor by `delta_steps` (e.g. `+1` for zoom in, `-1`
/// for zoom out), rounded to one decimal place to prevent floating-point drift.
pub fn step_zoom(current: f32, delta_steps: i32) -> f32 {
    let base = clamp_zoom(current);
    let next = base + (delta_steps as f32) * PAGE_ZOOM_STEP;
    let rounded = (next * 10.0).round() / 10.0;
    clamp_zoom(rounded)
}

/// Device-pixel screen and window geometry computed for
/// `WebViewDelegate::screen_geometry` (Milestone C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRects {
    pub screen_width: i32,
    pub screen_height: i32,
    pub available_width: i32,
    pub available_height: i32,
    pub window_x: i32,
    pub window_y: i32,
    pub window_width: i32,
    pub window_height: i32,
}

/// Compute device-pixel `ScreenRects` from the active monitor's physical size
/// (if known) and the window's physical outer position and size.
pub fn compute_screen_geometry(
    monitor_size: Option<(u32, u32)>,
    window_outer_pos: (i32, i32),
    window_outer_size: (u32, u32),
) -> ScreenRects {
    let win_w = window_outer_size.0.min(i32::MAX as u32) as i32;
    let win_h = window_outer_size.1.min(i32::MAX as u32) as i32;
    let (screen_w, screen_h) = match monitor_size {
        Some((w, h)) if w > 0 && h > 0 => {
            (w.min(i32::MAX as u32) as i32, h.min(i32::MAX as u32) as i32)
        }
        _ => (win_w.max(1), win_h.max(1)),
    };
    ScreenRects {
        screen_width: screen_w,
        screen_height: screen_h,
        available_width: screen_w,
        available_height: screen_h,
        window_x: window_outer_pos.0,
        window_y: window_outer_pos.1,
        window_width: win_w,
        window_height: win_h,
    }
}

/// Convert a physical-pixel position reported by the windowing system into
/// logical pixels using the window's scale factor. A non-positive or
/// non-finite scale factor falls back to 1.0.
pub fn physical_to_logical_position(x: i32, y: i32, scale_factor: f64) -> LogicalPosition {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    LogicalPosition::new(
        (x as f64 / scale).round() as i32,
        (y as f64 / scale).round() as i32,
    )
}

#[cfg(not(test))]
use crate::json;
// Also compiled as an independent crate by the standalone test harness.
#[cfg(test)]
#[allow(dead_code)]
#[path = "json.rs"]
mod json;

/// Read a top-level integer field. Missing and non-integer values yield `None`.
pub fn json_get_i64(text: &str, key: &str) -> Option<i64> {
    let text = text.trim_start_matches('\u{FEFF}');
    let (start, end) = json::value_span(text, key)?;
    text[start..end].parse().ok()
}

/// Read and decode a top-level string field.
pub fn json_get_string(text: &str, key: &str) -> Option<String> {
    json::json_get_string(text.trim_start_matches('\u{FEFF}'), key)
}

/// Replace or insert a top-level field with a pre-rendered JSON value token.
///
/// If `text` is not a JSON object it is returned unchanged.
fn json_set_raw(text: &str, key: &str, value_token: &str) -> String {
    let text = text.trim_start_matches('\u{FEFF}');
    if let Some((start, end)) = json::value_span(text, key) {
        let mut out = String::with_capacity(text.len() + value_token.len());
        out.push_str(&text[..start]);
        out.push_str(value_token);
        out.push_str(&text[end..]);
        return out;
    }
    // Key absent: insert before the closing brace of the root object.
    let trimmed = text.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return text.to_owned();
    }
    let insert_at = text.rfind('}').unwrap_or(text.len());
    let inner = &trimmed[1..trimmed.len() - 1];
    let has_fields = !inner.trim().is_empty();
    let mut out = String::with_capacity(text.len() + key.len() + value_token.len() + 8);
    out.push_str(&text[..insert_at]);
    if has_fields {
        out.push_str(", ");
    }
    out.push_str(&format!("{}: {}", json::json_string(key), value_token));
    out.push_str(&text[insert_at..]);
    out
}

/// Set a top-level integer field; `None` writes `null`.
pub fn json_set_i64(text: &str, key: &str, value: Option<i64>) -> String {
    let token = match value {
        Some(value) => value.to_string(),
        None => "null".to_owned(),
    };
    json_set_raw(text, key, &token)
}

/// Set a top-level string field.
pub fn json_set_string(text: &str, key: &str, value: &str) -> String {
    let token = json::json_string(value);
    json_set_raw(text, key, &token)
}

/// Whether a physical-pixel window rectangle has enough visible area (at
/// least 32x32 px, or the full window dimension when smaller) on at least one
/// monitor so the user can always see and grab the window.
pub fn window_rect_visible_on_any_monitor(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    monitors: &[(i32, i32, u32, u32)],
) -> bool {
    let wx1 = i64::from(x);
    let wy1 = i64::from(y);
    let wx2 = wx1 + i64::from(width.max(1));
    let wy2 = wy1 + i64::from(height.max(1));
    let min_w = i64::from(width.clamp(1, 32));
    let min_h = i64::from(height.clamp(1, 32));
    monitors.iter().any(|&(mx, my, mw, mh)| {
        let mx1 = i64::from(mx);
        let my1 = i64::from(my);
        let mx2 = mx1 + i64::from(mw);
        let my2 = my1 + i64::from(mh);
        let overlap_w = (wx2.min(mx2) - wx1.max(mx1)).max(0);
        let overlap_h = (wy2.min(my2) - wy1.max(my1)).max(0);
        overlap_w >= min_w && overlap_h >= min_h
    })
}

/// Saved window state for one app, restored at startup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedWindowState {
    pub presentation: Option<Presentation>,
    pub chips_position: Option<LogicalPosition>,
    pub flyout_position: Option<LogicalPosition>,
}

impl SavedWindowState {
    /// Extract the window state from `config.json` text.
    pub fn from_config_json(text: &str) -> Self {
        let position = |keys: (&str, &str)| -> Option<LogicalPosition> {
            let x = json_get_i64(text, keys.0)?;
            let y = json_get_i64(text, keys.1)?;
            Some(LogicalPosition::new(
                i32::try_from(x).ok()?,
                i32::try_from(y).ok()?,
            ))
        };
        Self {
            presentation: json_get_string(text, "presentation")
                .and_then(|value| Presentation::from_config_value(&value)),
            chips_position: position(Presentation::Chips.position_keys()),
            flyout_position: position(Presentation::Flyout.position_keys()),
        }
    }

    /// The saved position for a presentation, if any.
    pub fn position_for(&self, presentation: Presentation) -> Option<LogicalPosition> {
        match presentation {
            Presentation::Chips => self.chips_position,
            Presentation::Flyout => self.flyout_position,
        }
    }

    /// Record a presentation switch and its window position in config text.
    pub fn record_presentation(
        config_text: &str,
        presentation: Presentation,
        position: Option<LogicalPosition>,
    ) -> String {
        let text = json_set_string(config_text, "presentation", presentation.config_value());
        let (x_key, y_key) = presentation.position_keys();
        let text = json_set_i64(&text, x_key, position.map(|p| i64::from(p.x)));
        json_set_i64(&text, y_key, position.map(|p| i64::from(p.y)))
    }

    /// Record a moved window position for the active presentation.
    pub fn record_position(
        config_text: &str,
        presentation: Presentation,
        position: LogicalPosition,
    ) -> String {
        let (x_key, y_key) = presentation.position_keys();
        let text = json_set_i64(config_text, x_key, Some(i64::from(position.x)));
        json_set_i64(&text, y_key, Some(i64::from(position.y)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
  "poll_interval_secs": 300,
  "chips_docked": true,
  "chips_x": 120,
  "chips_y": null,
  "presentation": "chips",
  "adapters": [ { "id": "primary", "x": 999 } ]
}"#;

    #[test]
    fn presentation_sizes_and_values() {
        assert_eq!(Presentation::Chips.size(), CHIPS_SIZE);
        assert_eq!(Presentation::Flyout.size(), FLYOUT_SIZE);
        assert_eq!(
            Presentation::from_config_value(Presentation::Chips.config_value()),
            Some(Presentation::Chips)
        );
        assert_eq!(Presentation::from_config_value("wide"), None);
    }

    #[test]
    fn clamp_size_bounds_each_axis() {
        let clamped = clamp_size(
            LogicalSize::new(10, 5000),
            LogicalSize::new(100, 100),
            LogicalSize::new(800, 600),
        );
        assert_eq!(clamped, LogicalSize::new(100, 600));
        assert_eq!(clamp_window_request_size(-10, 99999), (160, 2160));
    }

    #[test]
    fn zoom_clamping_and_stepping() {
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
        assert_eq!(clamp_zoom(0.1), MIN_PAGE_ZOOM);
        assert_eq!(clamp_zoom(9.0), MAX_PAGE_ZOOM);
        assert_eq!(step_zoom(1.0, 1), 1.1);
        assert_eq!(step_zoom(1.1, -1), 1.0);
        assert_eq!(step_zoom(MAX_PAGE_ZOOM, 1), MAX_PAGE_ZOOM);
        assert_eq!(step_zoom(MIN_PAGE_ZOOM, -1), MIN_PAGE_ZOOM);
    }

    #[test]
    fn screen_geometry_uses_monitor_or_window_fallback() {
        let geom = compute_screen_geometry(Some((1920, 1080)), (100, 50), (448, 620));
        assert_eq!(
            geom,
            ScreenRects {
                screen_width: 1920,
                screen_height: 1080,
                available_width: 1920,
                available_height: 1080,
                window_x: 100,
                window_y: 50,
                window_width: 448,
                window_height: 620,
            }
        );
        let fallback = compute_screen_geometry(None, (0, 0), (1000, 720));
        assert_eq!(fallback.screen_width, 1000);
        assert_eq!(fallback.screen_height, 720);
    }

    #[test]
    fn physical_position_scales_down() {
        assert_eq!(
            physical_to_logical_position(300, 200, 1.5),
            LogicalPosition::new(200, 133)
        );
        assert_eq!(
            physical_to_logical_position(300, 200, 0.0),
            LogicalPosition::new(300, 200)
        );
    }

    #[test]
    fn monitor_visible_rect_validation() {
        let monitors = [(0, 0, 1920, 1080), (1920, 0, 1280, 1024)];
        assert!(window_rect_visible_on_any_monitor(
            100, 100, 448, 620, &monitors
        ));
        assert!(window_rect_visible_on_any_monitor(
            2000, 500, 448, 620, &monitors
        ));
        // Slight overhang past the left edge is still mostly visible.
        assert!(window_rect_visible_on_any_monitor(
            -10, 20, 448, 620, &monitors
        ));
        // Almost completely off the right edge: visible overlap is only 4px.
        assert!(!window_rect_visible_on_any_monitor(
            3196, 100, 448, 620, &monitors
        ));
        // Windows minimized sentinel (-32000, -32000) must be rejected.
        assert!(!window_rect_visible_on_any_monitor(
            -32000, -32000, 448, 620, &monitors
        ));
        assert!(!window_rect_visible_on_any_monitor(100, 100, 448, 620, &[]));
    }

    #[test]
    fn json_get_reads_top_level_only_and_utf8_keys() {
        assert_eq!(json_get_i64(SAMPLE, "chips_x"), Some(120));
        assert_eq!(json_get_i64(SAMPLE, "chips_y"), None); // null
        assert_eq!(json_get_i64(SAMPLE, "x"), None); // nested in adapters
        assert_eq!(
            json_get_string(SAMPLE, "presentation"),
            Some("chips".to_owned())
        );
        assert_eq!(json_get_i64("not json", "chips_x"), None);
        assert_eq!(
            json_get_string(r#"{"名前": "量子"}"#, "名前"),
            Some("量子".to_owned())
        );
    }

    #[test]
    fn json_set_replaces_inserts_and_handles_whitespace_empty_objects() {
        let replaced = json_set_i64(SAMPLE, "chips_x", Some(-5));
        assert_eq!(json_get_i64(&replaced, "chips_x"), Some(-5));

        let nulled = json_set_i64(SAMPLE, "chips_x", None);
        assert_eq!(json_get_i64(&nulled, "chips_x"), None);
        assert!(nulled.contains("\"chips_x\": null"));

        let inserted = json_set_string(SAMPLE, "presentation_mode", "flyout");
        assert_eq!(
            json_get_string(&inserted, "presentation_mode"),
            Some("flyout".to_owned())
        );
        // The nested adapters array must survive untouched.
        assert!(inserted.contains("\"id\": \"primary\""));

        // Leading whitespace and whitespace-only empty objects must produce valid JSON.
        let from_empty = json_set_i64("  { \n } ", "chips_x", Some(42));
        assert!(!from_empty.contains(", \"chips_x\""));
        assert_eq!(json_get_i64(&from_empty, "chips_x"), Some(42));
    }

    #[test]
    fn saved_state_round_trip_and_traits() {
        let state = SavedWindowState::from_config_json(SAMPLE);
        assert_eq!(state.clone(), state);
        assert_eq!(SavedWindowState::default().presentation, None);
        assert_eq!(state.presentation, Some(Presentation::Chips));
        assert_eq!(state.chips_position, None); // y is null

        let text = SavedWindowState::record_presentation(
            SAMPLE,
            Presentation::Flyout,
            Some(LogicalPosition::new(10, 20)),
        );
        let state = SavedWindowState::from_config_json(&text);
        assert_eq!(state.presentation, Some(Presentation::Flyout));
        assert_eq!(
            state.position_for(Presentation::Flyout),
            Some(LogicalPosition::new(10, 20))
        );

        let text = SavedWindowState::record_position(
            &text,
            Presentation::Flyout,
            LogicalPosition::new(30, 40),
        );
        let state = SavedWindowState::from_config_json(&text);
        assert_eq!(
            state.position_for(Presentation::Flyout),
            Some(LogicalPosition::new(30, 40))
        );

        let bom_json = "\u{FEFF}{\"presentation\": \"chips\", \"chips_x\": 120, \"chips_y\": 240}";
        let bom_state = SavedWindowState::from_config_json(bom_json);
        assert_eq!(bom_state.presentation, Some(Presentation::Chips));
        assert_eq!(
            bom_state.position_for(Presentation::Chips),
            Some(LogicalPosition::new(120, 240))
        );
        let rewritten = SavedWindowState::record_presentation(bom_json, Presentation::Flyout, None);
        assert!(!rewritten.starts_with('\u{FEFF}'));
        assert_eq!(
            SavedWindowState::from_config_json(&rewritten).presentation,
            Some(Presentation::Flyout)
        );
    }
}

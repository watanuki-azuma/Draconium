/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Availability of native host services across operating systems.

/// Runtime capability matrix describing which native OS host features are
/// implemented on the current platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformCapabilities {
    pub os: &'static str,
    pub native_dialogs: bool,
    pub native_popup_menus: bool,
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
            native_popup_menus: true,
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
            native_popup_menus: false,
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
            native_popup_menus: false,
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
            native_popup_menus: false,
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
                r#"{{"os":"{}","nativeDialogs":{},"nativePopupMenus":{},"clipboardText":{},"clipboardHtml":{},"#,
                r#""clipboardImage":{},"notifications":{},"autostart":{},"singleInstance":{},"#,
                r#""systemTheme":{},"externalUrl":{},"systemTray":{},"globalHotkey":{},"#,
                r#""safeStorage":{},"windowOpacity":{},"clickThrough":{},"customUriScheme":{},"mediaGstreamer":{}}}"#
            ),
            self.os,
            self.native_dialogs,
            self.native_popup_menus,
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
        assert!(win.native_popup_menus);
        assert!(!mac.native_popup_menus);

        let linux = PlatformCapabilities::linux();
        assert_eq!(linux.os, "linux");
        assert!(linux.native_dialogs && linux.notifications && linux.autostart);
        assert!(linux.clipboard_image && linux.custom_uri_scheme);
        assert!(!linux.system_tray && !linux.media_gstreamer);
        assert!(linux.to_json().contains(r#""mediaGstreamer":false"#));
        assert!(linux.to_json().contains(r#""nativePopupMenus":false"#));

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
}

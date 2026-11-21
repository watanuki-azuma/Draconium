/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Declarative capability ACLs (`Capabilities`) for Draconium host apps.
//!
//! Each hosted app mode declares which privileged host APIs and application
//! commands its renderer is allowed to invoke. Child windows opened from a
//! page receive an attenuated capability set via
//! [`Capabilities::attenuate_for_child`].
//!
//! This module is pure Rust with zero external crate dependencies so it can
//! be unit-tested standalone on any OS with `rustc --edition 2024 --test`.

/// Policy governing which application-specific commands (`HostHandler`) a
/// page is permitted to invoke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllowedAppCommands {
    /// No application commands may be invoked (e.g. `--probe` or child popups).
    None,
    /// Only exact command names in this allowlist may be invoked.
    AllowList(Vec<String>),
}

/// Discrete host capability categories enforced before running privileged
/// window/platform operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    ExternalLinks,
    FileDialogs,
    FileDrop,
    SafeStorage,
    Notifications,
    WindowSizing,
    OpacityControl,
    Capture,
    ClipboardRead,
    ClipboardWrite,
    Autostart,
    ProtocolClient,
    SiteData,
    ChildWindows,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalLinks => "external_links",
            Self::FileDialogs => "file_dialogs",
            Self::FileDrop => "file_drop",
            Self::SafeStorage => "safe_storage",
            Self::Notifications => "notifications",
            Self::WindowSizing => "window_sizing",
            Self::OpacityControl => "opacity_control",
            Self::Capture => "capture",
            Self::ClipboardRead => "clipboard_read",
            Self::ClipboardWrite => "clipboard_write",
            Self::Autostart => "autostart",
            Self::ProtocolClient => "protocol_client",
            Self::SiteData => "site_data",
            Self::ChildWindows => "child_windows",
        }
    }
}

/// Per-application capability configuration enforced by the Draconium host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub allow_external_links: bool,
    pub allow_file_dialogs: bool,
    pub allow_file_drop: bool,
    pub allow_safe_storage: bool,
    pub allow_notifications: bool,
    pub allow_window_sizing: bool,
    pub allow_opacity_control: bool,
    pub allow_capture: bool,
    pub allow_clipboard_read: bool,
    pub allow_clipboard_write: bool,
    pub allow_autostart: bool,
    pub allow_protocol_client: bool,
    pub allow_site_data: bool,
    pub allow_child_windows: bool,
    pub allowed_app_commands: AllowedAppCommands,
}

impl Capabilities {
    /// Locked-down baseline with all privileged host APIs and application
    /// commands disabled (except screenshot capture for diagnostics if opted in).
    pub fn restricted() -> Self {
        Self {
            allow_external_links: false,
            allow_file_dialogs: false,
            allow_file_drop: false,
            allow_safe_storage: false,
            allow_notifications: false,
            allow_window_sizing: false,
            allow_opacity_control: false,
            allow_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_autostart: false,
            allow_protocol_client: false,
            allow_site_data: false,
            allow_child_windows: false,
            allowed_app_commands: AllowedAppCommands::None,
        }
    }

    /// Standard desktop application baseline enabling custom chrome, dialogs,
    /// drag-and-drop, clipboard, notifications, DPAPI storage, export capture,
    /// external `https:`/`mailto:` links, and attenuated child windows.
    ///
    /// Application commands remain disabled until explicitly granted with
    /// [`Self::with_app_commands`].
    pub fn standard_desktop() -> Self {
        Self {
            allow_external_links: true,
            allow_file_dialogs: true,
            allow_file_drop: true,
            allow_safe_storage: true,
            allow_notifications: true,
            allow_window_sizing: true,
            allow_opacity_control: true,
            allow_capture: true,
            allow_clipboard_read: true,
            allow_clipboard_write: true,
            allow_autostart: false,
            allow_protocol_client: false,
            allow_site_data: false,
            allow_child_windows: true,
            allowed_app_commands: AllowedAppCommands::None,
        }
    }

    pub fn with_app_commands<I, S>(mut self, commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let list: Vec<String> = commands.into_iter().map(Into::into).collect();
        self.allowed_app_commands = if list.is_empty() {
            AllowedAppCommands::None
        } else {
            AllowedAppCommands::AllowList(list)
        };
        self
    }

    pub fn with_external_links(mut self, allow: bool) -> Self {
        self.allow_external_links = allow;
        self
    }

    pub fn with_file_dialogs(mut self, allow: bool) -> Self {
        self.allow_file_dialogs = allow;
        self
    }

    pub fn with_file_drop(mut self, allow: bool) -> Self {
        self.allow_file_drop = allow;
        self
    }

    pub fn with_safe_storage(mut self, allow: bool) -> Self {
        self.allow_safe_storage = allow;
        self
    }

    pub fn with_notifications(mut self, allow: bool) -> Self {
        self.allow_notifications = allow;
        self
    }

    pub fn with_window_sizing(mut self, allow: bool) -> Self {
        self.allow_window_sizing = allow;
        self
    }

    pub fn with_opacity_control(mut self, allow: bool) -> Self {
        self.allow_opacity_control = allow;
        self
    }

    pub fn with_capture(mut self, allow: bool) -> Self {
        self.allow_capture = allow;
        self
    }

    pub fn with_clipboard(mut self, read: bool, write: bool) -> Self {
        self.allow_clipboard_read = read;
        self.allow_clipboard_write = write;
        self
    }

    pub fn with_autostart(mut self, allow: bool) -> Self {
        self.allow_autostart = allow;
        self
    }

    pub fn with_protocol_client(mut self, allow: bool) -> Self {
        self.allow_protocol_client = allow;
        self
    }

    pub fn with_site_data(mut self, allow: bool) -> Self {
        self.allow_site_data = allow;
        self
    }

    pub fn with_child_windows(mut self, allow: bool) -> Self {
        self.allow_child_windows = allow;
        self
    }

    /// Produce an attenuated capability set for child windows (`window.open`
    /// or `Draconium.window.openChild`), stripping sensitive OS persistence,
    /// DPAPI, registry, file-picker, clipboard, and app-command privileges.
    pub fn attenuate_for_child(&self) -> Self {
        Self {
            allow_external_links: self.allow_external_links,
            allow_file_dialogs: false,
            allow_file_drop: false,
            allow_safe_storage: false,
            allow_notifications: self.allow_notifications,
            allow_window_sizing: self.allow_window_sizing,
            allow_opacity_control: false,
            allow_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_autostart: false,
            allow_protocol_client: false,
            allow_site_data: false,
            allow_child_windows: false,
            allowed_app_commands: AllowedAppCommands::None,
        }
    }

    /// Check whether a discrete [`Capability`] is enabled.
    pub fn check(&self, cap: Capability) -> Result<(), &'static str> {
        let allowed = match cap {
            Capability::ExternalLinks => self.allow_external_links,
            Capability::FileDialogs => self.allow_file_dialogs,
            Capability::FileDrop => self.allow_file_drop,
            Capability::SafeStorage => self.allow_safe_storage,
            Capability::Notifications => self.allow_notifications,
            Capability::WindowSizing => self.allow_window_sizing,
            Capability::OpacityControl => self.allow_opacity_control,
            Capability::Capture => self.allow_capture,
            Capability::ClipboardRead => self.allow_clipboard_read,
            Capability::ClipboardWrite => self.allow_clipboard_write,
            Capability::Autostart => self.allow_autostart,
            Capability::ProtocolClient => self.allow_protocol_client,
            Capability::SiteData => self.allow_site_data,
            Capability::ChildWindows => self.allow_child_windows,
        };
        if allowed { Ok(()) } else { Err(cap.as_str()) }
    }

    /// Check whether a `dn-window:<name>` command is allowed by this policy.
    ///
    /// Accepts either the bare command suffix (e.g. `"open-external"`) or the
    /// prefixed form (`"dn-window:open-external"`).
    pub fn check_window_command_name(&self, command_name: &str) -> Result<(), &'static str> {
        let name = command_name
            .strip_prefix("dn-window:")
            .unwrap_or(command_name);
        match name {
            "drag" | "drag-resize" | "minimize" | "toggle-maximize" | "is-maximized"
            | "set-fullscreen" | "is-fullscreen" | "set-always-on-top" | "focus" | "get-bounds"
            | "set-size" | "chips" | "flyout" | "hide" | "show" => {
                self.check(Capability::WindowSizing)
            }
            "close" | "reload" | "hard-reload" | "zoom-in" | "zoom-out" | "zoom-reset"
            | "get-theme" | "get-platform" | "set-title" | "go-back" | "go-forward"
            | "can-go-back" | "can-go-forward" => Ok(()),
            "open-external" => self.check(Capability::ExternalLinks),
            "set-opacity" | "set-click-through" => self.check(Capability::OpacityControl),
            "capture-png" | "export-html" | "export-pdf" => self.check(Capability::Capture),
            "save-download" | "dialog-open-file" | "dialog-select-folder" | "dialog-save-file" => {
                self.check(Capability::FileDialogs)
            }
            "file-read-text" | "file-read-base64" => {
                if self.allow_file_drop || self.allow_file_dialogs {
                    Ok(())
                } else {
                    Err("file_access")
                }
            }
            "safe-storage-encrypt" | "safe-storage-decrypt" => self.check(Capability::SafeStorage),
            "notify" | "request-user-attention" => self.check(Capability::Notifications),
            "clipboard-read-text" | "clipboard-read-html" | "clipboard-read-image" => {
                self.check(Capability::ClipboardRead)
            }
            "clipboard-write-text"
            | "clipboard-write-html"
            | "clipboard-write-image"
            | "clipboard-clear" => self.check(Capability::ClipboardWrite),
            "autostart-get" | "autostart-set" => self.check(Capability::Autostart),
            "protocol-client-set" | "protocol-client-remove" => {
                self.check(Capability::ProtocolClient)
            }
            "session-list-site-data"
            | "session-clear-site-data"
            | "session-clear-cookies"
            | "session-clear-cache" => self.check(Capability::SiteData),
            "open-child-window" => self.check(Capability::ChildWindows),
            _ => Err("unknown_window_command"),
        }
    }

    /// Check whether an application command (`HostHandler::handle_request`) is
    /// permitted by [`Self::allowed_app_commands`].
    pub fn check_app_command(&self, command: &str) -> Result<(), String> {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return Err("空のコマンド名は許可されていません".to_owned());
        }
        match &self.allowed_app_commands {
            AllowedAppCommands::None => Err(format!("capability denied: app_command ({trimmed})")),
            AllowedAppCommands::AllowList(list) => {
                if list.iter().any(|allowed| allowed == trimmed) {
                    Ok(())
                } else {
                    Err(format!("capability denied: app_command ({trimmed})"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_desktop_and_builders_allow_expected_commands_and_chrome() {
        let caps = Capabilities::standard_desktop()
            .with_autostart(true)
            .with_site_data(true)
            .with_app_commands(["list_notes", "save_note"]);
        assert!(caps.check_app_command("list_notes").is_ok());
        assert!(caps.check_app_command("save_note").is_ok());
        assert!(caps.check_app_command("echo").is_err());

        assert!(caps.check_window_command_name("minimize").is_ok());
        assert!(caps.check_window_command_name("toggle-maximize").is_ok());
        assert!(caps.check_window_command_name("drag-resize").is_ok());
        for command in [
            "set-fullscreen",
            "is-fullscreen",
            "set-always-on-top",
            "focus",
            "get-bounds",
            "set-size",
            "request-user-attention",
            "set-title",
            "go-back",
            "go-forward",
            "can-go-back",
            "can-go-forward",
        ] {
            assert!(caps.check_window_command_name(command).is_ok(), "{command}");
        }
        assert!(caps.check_window_command_name("dialog-open-file").is_ok());
        assert!(caps.check_window_command_name("file-read-text").is_ok());
        assert!(
            caps.check_window_command_name("clipboard-write-html")
                .is_ok()
        );
        assert!(
            caps.check_window_command_name("safe-storage-encrypt")
                .is_ok()
        );
        assert!(caps.check_window_command_name("autostart-get").is_ok());
        assert!(
            caps.check_window_command_name("session-list-site-data")
                .is_ok()
        );
        assert!(
            caps.check_window_command_name("protocol-client-set")
                .is_err()
        );
    }

    #[test]
    fn restricted_defaults_and_selective_opt_in() {
        let restricted = Capabilities::restricted();
        for command in [
            "set-fullscreen",
            "is-fullscreen",
            "set-always-on-top",
            "focus",
            "get-bounds",
            "set-size",
            "request-user-attention",
        ] {
            assert!(
                restricted.check_window_command_name(command).is_err(),
                "{command}"
            );
        }
        for command in [
            "set-title",
            "go-back",
            "go-forward",
            "can-go-back",
            "can-go-forward",
        ] {
            assert!(
                restricted.check_window_command_name(command).is_ok(),
                "{command}"
            );
        }

        let minimal = Capabilities::restricted()
            .with_window_sizing(true)
            .with_capture(true)
            .with_app_commands(["echo", "host_info"]);
        assert!(minimal.check_app_command("echo").is_ok());
        assert!(minimal.check_app_command("host_info").is_ok());
        assert!(minimal.check_app_command("save_note").is_err());
        assert!(minimal.check_window_command_name("open-external").is_err());
        assert!(
            minimal
                .check_window_command_name("dialog-open-file")
                .is_err()
        );
        assert!(minimal.check_window_command_name("file-read-text").is_err());
        assert!(minimal.check_window_command_name("set-fullscreen").is_ok());
        assert!(
            minimal
                .check_window_command_name("request-user-attention")
                .is_err()
        );
        for command in [
            "set-title",
            "go-back",
            "go-forward",
            "can-go-back",
            "can-go-forward",
        ] {
            assert!(
                minimal.check_window_command_name(command).is_ok(),
                "{command}"
            );
        }
        let opted_in = Capabilities::restricted()
            .with_window_sizing(true)
            .with_notifications(true);
        assert!(opted_in.check_window_command_name("set-fullscreen").is_ok());
        assert!(
            opted_in
                .check_window_command_name("request-user-attention")
                .is_ok()
        );
        assert!(
            minimal
                .check_window_command_name("safe-storage-encrypt")
                .is_err()
        );

        let locked = Capabilities::restricted().with_capture(true);
        assert!(locked.check_app_command("echo").is_err());
        assert!(locked.check_window_command_name("drag").is_err());
        assert!(locked.check_window_command_name("capture-png").is_ok());
        assert!(locked.check_window_command_name("get-platform").is_ok());
    }

    #[test]
    fn attenuate_for_child_strips_sensitive_privileges() {
        let parent = Capabilities::standard_desktop()
            .with_autostart(true)
            .with_site_data(true)
            .with_app_commands(["list_notes"]);
        let child = parent.attenuate_for_child();
        assert!(child.check_window_command_name("open-external").is_ok());
        assert!(child.check_window_command_name("notify").is_ok());
        for command in [
            "set-fullscreen",
            "is-fullscreen",
            "set-always-on-top",
            "focus",
            "get-bounds",
            "set-size",
            "request-user-attention",
            "set-title",
            "go-back",
            "go-forward",
            "can-go-back",
            "can-go-forward",
        ] {
            assert!(
                child.check_window_command_name(command).is_ok(),
                "{command}"
            );
        }
        assert!(
            child
                .check_window_command_name("safe-storage-encrypt")
                .is_err()
        );
        assert!(child.check_window_command_name("dialog-open-file").is_err());
        assert!(child.check_window_command_name("file-read-text").is_err());
        assert!(
            child
                .check_window_command_name("clipboard-read-text")
                .is_err()
        );
        assert!(child.check_window_command_name("autostart-set").is_err());
        assert!(
            child
                .check_window_command_name("protocol-client-set")
                .is_err()
        );
        assert!(
            child
                .check_window_command_name("session-clear-cookies")
                .is_err()
        );
        assert!(
            child
                .check_window_command_name("open-child-window")
                .is_err()
        );
        assert!(child.check_app_command("list_notes").is_err());
    }
}

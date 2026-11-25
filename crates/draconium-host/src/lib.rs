/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! # `draconium-host`
//!
//! Lightweight, self-contained Rust desktop WebView host powered by **Servo 0.8.0**
//! (Stylo, WebRender, SpiderMonkey) — no Chromium or OS WebView runtime required.
//!
//! ## Quick Start
//!
//! ```no_run
//! use draconium_host::{
//!     Capabilities, EmbeddedAsset, HostConfig, HostHandler, WindowConfig, run,
//!     try_resolve_app_url_from,
//! };
//! use std::path::Path;
//!
//! const ASSETS: &[EmbeddedAsset] = &[EmbeddedAsset {
//!     path: "index.html",
//!     bytes: b"<!doctype html><html><body><h1>Hello Draconium</h1></body></html>",
//! }];
//!
//! struct MyApp;
//!
//! impl HostHandler for MyApp {
//!     fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
//!         match command {
//!             "ping" => Ok(format!("pong: {payload}")),
//!             other => Err(format!("Unsupported command: {other}")),
//!         }
//!     }
//! }
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let (url, asset_root) = try_resolve_app_url_from(
//!         Path::new(env!("CARGO_MANIFEST_DIR")),
//!         true,
//!         None,
//!         "index.html",
//!         "ui/index.html",
//!         "ui/index.html",
//!     )?;
//!     let config = HostConfig::new("My App", url)
//!         .with_window(WindowConfig::standard(960, 640))
//!         .with_asset_root(asset_root)
//!         .with_embedded_assets(ASSETS)
//!         .with_capabilities(Capabilities::restricted().with_app_commands(["ping"]));
//!     run(config, MyApp, None)
//! }
//! ```

mod bridge;
mod dn_protocol;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod embedder_ui;
mod host;
mod input;
#[doc(hidden)]
pub mod json;
mod platform_capabilities;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod platform_services;
mod policy;
mod protocol;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod tray;
mod unix_platform;
mod window_state;

pub use url::Url;

pub use crate::bridge::DevOrigin;
pub use crate::host::{
    HostConfig, HostHandler, WindowConfig, resolve_app_url, resolve_app_url_from, resolve_ui_path,
    resolve_ui_path_from, run, try_resolve_app_url_from,
};
pub use crate::policy::{AllowedAppCommands, Capabilities, Capability};
pub use crate::protocol::{APP_AUTHORITY, DEFAULT_CSP, EmbeddedAsset, SCHEME, asset_url};
pub use crate::window_state::{
    CHIPS_SIZE, DEFAULT_WINDOW_SIZE, FLYOUT_SIZE, LogicalPosition, LogicalSize, Presentation,
    SavedWindowState,
};

/// Platform validation, capability introspection, and parsing utilities for CLI
/// flags and host setup.
pub mod platform {
    pub use crate::platform_capabilities::{PlatformCapabilities, platform_capabilities};
    pub use crate::platform_services::{
        opacity_to_alpha, parse_hotkey, validate_custom_uri_scheme, validate_devtools_addr,
    };
}

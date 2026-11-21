/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Routing core for the `qv://` custom protocol.
//!
//! Servo lets an embedder register custom protocols through
//! `servo::protocol_handler::ProtocolRegistry`, so local desktop apps can be
//! served from `qv://app/<path>` instead of loose `file://` URLs.
//!
//! This module keeps the testable part — multi-segment path normalization,
//! traversal/dotfile rejection, embedded fallback bytes (`include_bytes!`),
//! default Content-Security-Policy generation, and MIME types — free of Servo
//! crate dependencies.

/// An in-binary embedded UI asset used as a self-contained fallback when
/// on-disk assets are not supplied or for single-binary distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedAsset {
    pub path: &'static str,
    pub bytes: &'static [u8],
}

/// The custom scheme the host registers for bundled assets.
pub const SCHEME: &str = "qv";

/// The authority (host part) under which app assets are served.
pub const APP_AUTHORITY: &str = "app";

/// Default strict Content-Security-Policy header injected on `qv://app/*.html`
/// responses. Because `rust-url` treats non-special schemes like `qv://` as
/// opaque origins and `content-security-policy` never matches `'self'` across
/// two opaque origins, `qv://app` is listed explicitly alongside `'self'`, and
/// `blob:` is permitted in `connect-src` for `<a download href="blob:...">`.
/// Popover polyfill styles are registered at `Origin::User` via
/// `UserStyleSheet`, so author `style-src` remains strict without
/// `'unsafe-inline'`.
pub const DEFAULT_CSP: &str = "default-src 'self' qv://app; script-src 'self' qv://app; style-src 'self' qv://app; img-src 'self' qv://app data: blob:; font-src 'self' qv://app data:; connect-src 'self' qv://app blob:; object-src 'none'; base-uri 'self' qv://app; frame-ancestors 'none'; form-action 'self' qv://app";

/// Build the `Content-Security-Policy` header string for `qv://app/` HTML
/// documents, optionally enabling `'wasm-unsafe-eval'` for WebAssembly apps.
pub fn build_csp_header(allow_wasm: bool) -> String {
    if allow_wasm {
        DEFAULT_CSP.replacen(
            "script-src 'self' qv://app;",
            "script-src 'self' qv://app 'wasm-unsafe-eval';",
            1,
        )
    } else {
        DEFAULT_CSP.to_owned()
    }
}

/// Build the `qv://` URL for a bundled asset file name or relative path.
pub fn asset_url(file_name: &str) -> String {
    let rel = file_name.strip_prefix('/').unwrap_or(file_name);
    format!("{SCHEME}://{APP_AUTHORITY}/{rel}")
}

/// MIME type for a file name or relative path, by extension. Unknown
/// extensions are served as `application/octet-stream`.
pub fn content_type_for(file_name: &str) -> &'static str {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let extension = base.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Normalize and validate a `qv://app/<path>` request path into a safe
/// relative path (supporting nested asset subdirectories such as
/// `assets/index.js` or `fonts/inter.woff2`).
///
/// Rejects empty paths, directory traversal (`..`), current-dir (`.`),
/// dotfiles/hidden directories (`.env`, `.git/config`), double or trailing
/// slashes, backslashes, percent-encoded characters, colons (drive letters /
/// NTFS ADS), NUL bytes, and control characters.
pub fn resolve_asset_path(raw_path: &str) -> Option<String> {
    let trimmed = raw_path.strip_prefix('/').unwrap_or(raw_path);
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.contains('%')
        || trimmed.contains('\0')
        || trimmed.contains(':')
        || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.') {
            return None;
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// Look up an asset in a caller-supplied [`EmbeddedAsset`] fallback table.
pub fn embedded_lookup(assets: &[EmbeddedAsset], path: &str) -> Option<&'static [u8]> {
    let normalized = resolve_asset_path(path)?;
    assets
        .iter()
        .find(|asset| asset.path == normalized)
        .map(|asset| asset.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_EMBEDDED: &[EmbeddedAsset] = &[
        EmbeddedAsset {
            path: "index.html",
            bytes: b"<!doctype html><html><body>Hello</body></html>",
        },
        EmbeddedAsset {
            path: "assets/app.js",
            bytes: b"console.log('ok');",
        },
    ];

    #[test]
    fn resolve_asset_path_finds_top_level_and_rejects_empty() {
        assert_eq!(
            resolve_asset_path("/index.html").as_deref(),
            Some("index.html")
        );
        assert_eq!(resolve_asset_path("app.js").as_deref(), Some("app.js"));
        assert_eq!(resolve_asset_path("/"), None);
        assert_eq!(resolve_asset_path(""), None);
    }

    #[test]
    fn resolve_asset_path_rejects_traversal_and_backslashes() {
        assert_eq!(resolve_asset_path("/../secret.txt"), None);
        assert_eq!(resolve_asset_path("/%2e%2e/index.html"), None);
        assert_eq!(resolve_asset_path("\\index.html"), None);
    }

    #[test]
    fn resolve_asset_path_supports_safe_nested_paths() {
        assert_eq!(
            resolve_asset_path("/assets/chunks/app-123.mjs").as_deref(),
            Some("assets/chunks/app-123.mjs")
        );
        assert_eq!(
            resolve_asset_path("fonts/inter.woff2").as_deref(),
            Some("fonts/inter.woff2")
        );
        assert_eq!(
            resolve_asset_path("/wasm/engine.wasm").as_deref(),
            Some("wasm/engine.wasm")
        );
        assert_eq!(resolve_asset_path("/assets/../secret.txt"), None);
        assert_eq!(resolve_asset_path("/.env"), None);
        assert_eq!(resolve_asset_path("/assets/.hidden/file.js"), None);
        assert_eq!(resolve_asset_path("/assets//file.js"), None);
        assert_eq!(resolve_asset_path("/assets/"), None);
        assert_eq!(resolve_asset_path("/C:/Windows/win.ini"), None);
        assert_eq!(resolve_asset_path("/index.html::$DATA"), None);
        assert_eq!(resolve_asset_path("/%2e%2e/index.html"), None);
        assert_eq!(resolve_asset_path("/assets\\file.js"), None);
    }

    #[test]
    fn embedded_lookup_resolves_normalized_caller_assets() {
        let html = embedded_lookup(TEST_EMBEDDED, "/index.html").expect("index.html embedded");
        assert!(
            std::str::from_utf8(html)
                .unwrap()
                .contains("<!doctype html>")
        );
        assert!(embedded_lookup(TEST_EMBEDDED, "assets/app.js").is_some());
        assert!(embedded_lookup(TEST_EMBEDDED, "/nonexistent.js").is_none());
        assert!(embedded_lookup(TEST_EMBEDDED, "/../index.html").is_none());
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(
            content_type_for("workspace.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type_for("x.CSS"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("assets/mod.wasm"), "application/wasm");
        assert_eq!(content_type_for("fonts/f.woff"), "font/woff");
        assert_eq!(content_type_for("fonts/f.ttf"), "font/ttf");
        assert_eq!(content_type_for("img/pic.webp"), "image/webp");
        assert_eq!(content_type_for("img/pic.avif"), "image/avif");
        assert_eq!(content_type_for("app.js.map"), "application/json");
        assert_eq!(content_type_for("notes.md"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("x.bin"), "application/octet-stream");
        assert_eq!(content_type_for("html"), "application/octet-stream");
        assert_eq!(content_type_for("README"), "application/octet-stream");
    }

    #[test]
    fn asset_url_and_csp_header_shape() {
        assert_eq!(asset_url("workspace.html"), "qv://app/workspace.html");
        assert_eq!(asset_url("/assets/app.js"), "qv://app/assets/app.js");
        let default_csp = build_csp_header(false);
        assert!(default_csp.contains("default-src 'self' qv://app"));
        assert!(default_csp.contains("connect-src 'self' qv://app blob:"));
        assert!(!default_csp.contains("wasm-unsafe-eval"));
        let wasm_csp = build_csp_header(true);
        assert!(wasm_csp.contains("script-src 'self' qv://app 'wasm-unsafe-eval';"));
    }
}

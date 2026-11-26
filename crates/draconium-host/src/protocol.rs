/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Routing core for the `dn://` custom protocol.
//!
//! Servo lets an embedder register custom protocols through
//! `servo::protocol_handler::ProtocolRegistry`, so local desktop apps can be
//! served from `dn://app/<path>` instead of loose `file://` URLs.
//!
//! This module keeps the testable part — multi-segment path normalization,
//! traversal/dotfile rejection, embedded fallback bytes (`include_bytes!`),
//! default Content-Security-Policy generation, and MIME types — free of Servo
//! crate dependencies.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// An in-binary embedded UI asset used as a self-contained fallback when
/// on-disk assets are not supplied or for single-binary distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedAsset {
    /// A dn://app/ path such as assets/app.js or /assets/app.js.
    /// Use a unique path with nonempty, visible segments and URL-safe ASCII.
    pub path: &'static str,
    pub bytes: &'static [u8],
}

/// The custom scheme the host registers for bundled assets.
pub const SCHEME: &str = "dn";

/// The authority (host part) under which app assets are served.
pub const APP_AUTHORITY: &str = "app";

/// Per-asset limit for disk reads. Bundled audio, video, and Wasm can be
/// larger than typical HTML or CSS, so allow up to 64 MiB from disk.
const MAX_DISK_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Default strict Content-Security-Policy header injected on `dn://app/*.html`
/// responses. Because `rust-url` treats non-special schemes like `dn://` as
/// opaque origins and `content-security-policy` never matches `'self'` across
/// two opaque origins, `dn://app` is listed explicitly alongside `'self'`, and
/// `blob:` is permitted in `connect-src` for `<a download href="blob:...">`.
/// Popover polyfill styles are registered at `Origin::User` via
/// `UserStyleSheet`, so author `style-src` remains strict without
/// `'unsafe-inline'`.
pub const DEFAULT_CSP: &str = "default-src 'self' dn://app; script-src 'self' dn://app; style-src 'self' dn://app; img-src 'self' dn://app data: blob:; font-src 'self' dn://app data:; connect-src 'self' dn://app blob:; object-src 'none'; base-uri 'self' dn://app; frame-ancestors 'none'; form-action 'self' dn://app";

/// Build the `Content-Security-Policy` header string for `dn://app/` HTML
/// documents, optionally enabling `'wasm-unsafe-eval'` for WebAssembly apps.
pub fn build_csp_header(allow_wasm: bool) -> String {
    if allow_wasm {
        DEFAULT_CSP.replacen(
            "script-src 'self' dn://app;",
            "script-src 'self' dn://app 'wasm-unsafe-eval';",
            1,
        )
    } else {
        DEFAULT_CSP.to_owned()
    }
}

/// Build the `dn://` URL for a bundled asset file name or relative path.
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
        "xhtml" | "xht" => "application/xhtml+xml; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
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

/// Normalize and validate a `dn://app/<path>` request path into a safe
/// relative path (supporting nested asset subdirectories such as
/// `assets/index.js` or `fonts/inter.woff2`).
///
/// Rejects empty or hidden segments, backslashes, colons (drive letters and
/// NTFS ADS), URL delimiters, and characters requiring percent encoding.
pub fn resolve_asset_path(raw_path: &str) -> Option<&str> {
    let trimmed = raw_path.strip_prefix('/').unwrap_or(raw_path);
    if trimmed.bytes().any(|byte| {
        !byte.is_ascii_graphic()
            || matches!(
                byte,
                b'"' | b'#' | b'%' | b':' | b'<' | b'>' | b'?' | b'\\' | b'`' | b'{' | b'}'
            )
    }) || trimmed
        .split('/')
        .any(|segment| segment.is_empty() || segment.starts_with('.'))
    {
        return None;
    }
    Some(trimmed)
}

/// Validate the fallback table even when disk assets take precedence.
pub fn validate_embedded_assets(assets: &[EmbeddedAsset]) -> Result<(), String> {
    let mut paths = HashSet::with_capacity(assets.len());
    for asset in assets {
        let path = resolve_asset_path(asset.path)
            .ok_or_else(|| format!("Invalid embedded asset path {:?}", asset.path))?;
        if !paths.insert(path) {
            return Err(format!("Duplicate embedded asset path {path:?}"));
        }
    }
    Ok(())
}

/// Look up an asset in a caller-supplied [`EmbeddedAsset`] fallback table.
pub fn embedded_lookup(assets: &[EmbeddedAsset], path: &str) -> Option<&'static [u8]> {
    let normalized = resolve_asset_path(path)?;
    assets
        .iter()
        .find(|asset| asset.path.strip_prefix('/').unwrap_or(asset.path) == normalized)
        .map(|asset| asset.bytes)
}

/// Read a normalized disk asset within a canonicalized asset root.
pub fn read_disk_asset(root_canon: &Path, rel_path: &str) -> Option<Vec<u8>> {
    let candidate = root_canon.join(rel_path).canonicalize().ok()?;
    if !candidate.starts_with(root_canon) || !candidate.is_file() {
        return None;
    }
    let file = File::open(candidate).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_DISK_ASSET_BYTES {
        return None;
    }
    // Check the actual read as well, since the file can grow after metadata().
    let mut bytes = Vec::new();
    file.take(MAX_DISK_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_DISK_ASSET_BYTES).then_some(bytes)
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
            path: "/assets/app.js",
            bytes: b"console.log('ok');",
        },
    ];

    #[test]
    fn resolve_asset_path_finds_top_level_and_rejects_empty() {
        assert_eq!(resolve_asset_path("/index.html"), Some("index.html"));
        assert_eq!(resolve_asset_path("app.js"), Some("app.js"));
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
            resolve_asset_path("/assets/chunks/app-123.mjs"),
            Some("assets/chunks/app-123.mjs")
        );
        assert_eq!(
            resolve_asset_path("fonts/inter.woff2"),
            Some("fonts/inter.woff2")
        );
        assert_eq!(
            resolve_asset_path("/wasm/engine.wasm"),
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
        assert_eq!(
            embedded_lookup(TEST_EMBEDDED, "assets/app.js"),
            Some(TEST_EMBEDDED[1].bytes)
        );
        assert_eq!(
            embedded_lookup(TEST_EMBEDDED, "/assets/app.js"),
            Some(TEST_EMBEDDED[1].bytes)
        );
        assert!(embedded_lookup(TEST_EMBEDDED, "/nonexistent.js").is_none());
        assert!(embedded_lookup(TEST_EMBEDDED, "/../index.html").is_none());
    }

    #[test]
    fn embedded_paths_reject_invalid_or_duplicate_entries() {
        assert_eq!(validate_embedded_assets(TEST_EMBEDDED), Ok(()));
        for path in [
            "../app.js",
            "//app.js",
            " app.js",
            "資料.js",
            "app.js?v=1",
            "app.js#top",
            "app%20a.js",
        ] {
            let error = validate_embedded_assets(&[EmbeddedAsset { path, bytes: b"" }]);
            assert!(error.unwrap_err().contains("Invalid embedded asset path"));
        }
        let duplicate = EmbeddedAsset {
            path: "/index.html",
            bytes: b"duplicate",
        };
        let error = validate_embedded_assets(&[TEST_EMBEDDED[0], duplicate]);
        assert!(error.unwrap_err().contains("Duplicate embedded asset path"));
    }

    #[test]
    fn disk_assets_have_a_size_limit() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dn-assets-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("asset.bin");
        std::fs::write(&path, b"asset").unwrap();
        let small = read_disk_asset(&root, "asset.bin");
        File::create(&path)
            .unwrap()
            .set_len(MAX_DISK_ASSET_BYTES + 1)
            .unwrap();
        let oversized = read_disk_asset(&root, "asset.bin");
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(small.as_deref(), Some(b"asset".as_slice()));
        assert_eq!(oversized, None);
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(
            content_type_for("workspace.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type_for("x.CSS"), "text/css; charset=utf-8");
        assert_eq!(
            content_type_for("page.xhtml"),
            "application/xhtml+xml; charset=utf-8"
        );
        assert_eq!(
            content_type_for("feed.xml"),
            "application/xml; charset=utf-8"
        );
        assert_eq!(content_type_for("icon.svg"), "image/svg+xml");
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
        assert_eq!(asset_url("workspace.html"), "dn://app/workspace.html");
        assert_eq!(asset_url("/assets/app.js"), "dn://app/assets/app.js");
        let default_csp = build_csp_header(false);
        assert!(default_csp.contains("default-src 'self' dn://app"));
        assert!(default_csp.contains("connect-src 'self' dn://app blob:"));
        assert!(!default_csp.contains("wasm-unsafe-eval"));
        let wasm_csp = build_csp_header(true);
        assert!(wasm_csp.contains("script-src 'self' dn://app 'wasm-unsafe-eval';"));
    }
}

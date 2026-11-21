/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Servo-facing `qv://` protocol handler (Milestone B + generalized SDK asset serving).
//!
//! The routing rules live in the dependency-free `protocol` module; this
//! file translates between Servo's fetch types and that resolver,
//! following the response-construction pattern of Servo 0.7.0's own
//! `net/protocols/data.rs`.
//!
//! Features:
//! - Serves safe nested paths under `asset_root` (`qv://app/assets/...`),
//!   verifying canonical path confinement inside `root_canon` to prevent
//!   symlink or junction escapes.
//! - Falls back to compiled-in [`protocol::EMBEDDED_ASSETS`] when `asset_root`
//!   is `None` or a core sample file is not overridden on disk.
//! - Injects `X-Content-Type-Options: nosniff` on all responses and the default
//!   `Content-Security-Policy` header on HTML documents.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, Request, ResourceFetchTiming, Response,
    ResponseBody,
};

use crate::protocol;

/// Serves bundled UI assets under `qv://app/` from an optional on-disk
/// directory with compiled-in fallback and CSP header injection.
pub struct QvAssetHandler {
    root_canon: Option<PathBuf>,
    embedded_assets: &'static [protocol::EmbeddedAsset],
    csp_header: Option<String>,
}

impl QvAssetHandler {
    pub fn new(
        root: Option<PathBuf>,
        embedded_assets: &'static [protocol::EmbeddedAsset],
        csp_header: Option<String>,
    ) -> Self {
        let root_canon = root.and_then(|path| std::fs::canonicalize(path).ok());
        Self {
            root_canon,
            embedded_assets,
            csp_header,
        }
    }

    fn read_disk_asset(&self, rel_path: &str) -> Option<Vec<u8>> {
        let root_canon = self.root_canon.as_ref()?;
        let mut candidate = root_canon.clone();
        for seg in rel_path.split('/') {
            candidate.push(seg);
        }
        let canon = std::fs::canonicalize(&candidate).ok()?;
        if !canon.starts_with(root_canon) || !canon.is_file() {
            return None;
        }
        std::fs::read(&canon).ok()
    }

    fn respond(&self, request: &Request) -> Response {
        let url = request.current_url();
        let timing = ResourceFetchTiming::new(request.timing_type());
        if url.host_str() != Some(protocol::APP_AUTHORITY) {
            return Self::not_found(url, timing);
        }
        let Some(rel_path) = protocol::resolve_asset_path(url.path()) else {
            return Self::not_found(url, timing);
        };
        let bytes = match self.read_disk_asset(&rel_path) {
            Some(bytes) => bytes,
            None => match protocol::embedded_lookup(self.embedded_assets, &rel_path) {
                Some(embedded) => embedded.to_vec(),
                None => return Self::not_found(url, timing),
            },
        };

        let mut response = Response::new(url, timing);
        let mime = protocol::content_type_for(&rel_path);
        if let Ok(content_type) = mime.parse() {
            response.headers.insert("content-type", content_type);
        }
        if let Ok(nosniff) = "nosniff".parse() {
            response.headers.insert("x-content-type-options", nosniff);
        }
        if mime.starts_with("text/html")
            && let Some(csp) = &self.csp_header
            && let Ok(csp_val) = csp.parse()
        {
            response.headers.insert("content-security-policy", csp_val);
        }
        *response.body.lock() = ResponseBody::Done(bytes);
        response
    }

    fn not_found(url: servo::ServoUrl, timing: ResourceFetchTiming) -> Response {
        let mut response = Response::new(url, timing);
        response.status = HttpStatus::new_raw(404, b"Not Found".to_vec());
        if let Ok(content_type) = "text/plain; charset=utf-8".parse() {
            response.headers.insert("content-type", content_type);
        }
        if let Ok(nosniff) = "nosniff".parse() {
            response.headers.insert("x-content-type-options", nosniff);
        }
        *response.body.lock() = ResponseBody::Done(b"not found".to_vec());
        response
    }
}

impl ProtocolHandler for QvAssetHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let response = self.respond(request);
        Box::pin(std::future::ready(response))
    }

    /// Bundled app assets may be fetched directly (ES modules, `fetch()`).
    fn is_fetchable(&self) -> bool {
        true
    }

    /// The bundled UI is first-party app content; treat it as a secure
    /// context so local APIs are not blocked as mixed content.
    fn is_secure(&self) -> bool {
        true
    }
}

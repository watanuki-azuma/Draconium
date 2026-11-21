/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Minimal standalone desktop app example embedding `draconium-host`.

use std::error::Error;
use std::path::Path;

use draconium_host::{
    Capabilities, EmbeddedAsset, HostConfig, HostHandler, WindowConfig, resolve_app_url_from, run,
};

const HELLO_HTML: &[u8] = br#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Draconium Host Example</title>
  <link rel="stylesheet" href="hello.css">
</head>
<body>
  <main class="card">
    <h1>Hello from draconium-host</h1>
    <p>Embedded Servo 0.8.0 desktop shell with zero Chromium or OS WebView dependencies.</p>
    <div class="row">
      <input id="msg" type="text" value="Hello Rust!">
      <button id="send" type="button">Invoke Rust</button>
    </div>
    <pre id="out">Waiting...</pre>
  </main>
  <script src="hello.js"></script>
</body>
</html>
"#;

const HELLO_CSS: &[u8] = br#"
body { margin: 0; font-family: "Segoe UI", system-ui, sans-serif; background: #0f172a; color: #e2e8f0; display: grid; place-items: center; min-height: 100vh; }
.card { width: min(560px, 92vw); padding: 28px; border-radius: 14px; background: #1e293b; border: 1px solid #334155; }
h1 { margin: 0 0 8px; font-size: 22px; }
p { margin: 0 0 18px; color: #94a3b8; }
.row { display: flex; gap: 8px; margin-bottom: 14px; }
input { flex: 1; padding: 8px 12px; border-radius: 8px; border: 1px solid #475569; background: #0f172a; color: #f8fafc; }
button { padding: 8px 16px; border-radius: 8px; border: none; background: #3b82f6; color: #ffffff; font-weight: 600; cursor: pointer; }
pre { margin: 0; padding: 12px; border-radius: 8px; background: #0f172a; color: #38bdf8; }
"#;

const HELLO_JS: &[u8] = br#"
document.getElementById("send").addEventListener("click", async () => {
  const text = document.getElementById("msg").value;
  const res = await window.Draconium.invokeAsync("greet", text);
  document.getElementById("out").textContent = res.ok ? res.data : ("Error: " + res.error);
});
"#;

const EMBEDDED_ASSETS: &[EmbeddedAsset] = &[
    EmbeddedAsset {
        path: "hello.html",
        bytes: HELLO_HTML,
    },
    EmbeddedAsset {
        path: "hello.css",
        bytes: HELLO_CSS,
    },
    EmbeddedAsset {
        path: "hello.js",
        bytes: HELLO_JS,
    },
];

struct HelloApp {
    calls: u64,
}

impl HostHandler for HelloApp {
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command {
            "greet" => {
                self.calls += 1;
                Ok(format!(
                    "Reply #{}: received {} chars ({payload})",
                    self.calls,
                    payload.chars().count()
                ))
            }
            other => Err(format!("Unsupported command: {other}")),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let (url, asset_root) = resolve_app_url_from(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        true,
        None,
        "hello.html",
        "ui/hello.html",
        "ui/hello.html",
    );
    let config = HostConfig::new("Draconium Host Example", url)
        .with_window(WindowConfig::standard(720, 480))
        .with_asset_root(asset_root)
        .with_embedded_assets(EMBEDDED_ASSETS)
        .with_capabilities(Capabilities::restricted().with_app_commands(["greet"]));
    run(config, HelloApp { calls: 0 }, None)
}

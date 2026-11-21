/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Minimal developer starter handler (`--starter`) showing how to bind custom
//! Rust commands to a Draconium Servo 0.8.0 desktop window.
//!
//! Pure Rust (`std` only) so it can be unit-tested standalone via
//! `rustc --edition 2024 --test -D warnings probes/servo/src/starter.rs`.

/// Minimal example `HostHandler` state.
#[derive(Default)]
pub struct StarterApp {
    request_count: u64,
}

impl StarterApp {
    /// Handles `"echo"` and `"host_info"` commands from `Draconium.invoke` / `invokeAsync`.
    pub fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command {
            "echo" => {
                self.request_count += 1;
                let char_count = payload.chars().count();
                Ok(format!(
                    r#"{{"count":{},"chars":{},"message":{},"engine":"Servo 0.8.0"}}"#,
                    self.request_count,
                    char_count,
                    json_escape(payload)
                ))
            }
            "host_info" => Ok(format!(
                r#"{{"host":"Draconium","engine":"Servo 0.8.0","protocol":"dn://app/","requests":{}}}"#,
                self.request_count
            )),
            other => Err(format!("Unsupported starter command: {other}")),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_counts_unicode_scalars_and_increments() {
        let mut app = StarterApp::default();
        let first = app.handle_request("echo", "Hello 日本😀").unwrap();
        assert!(first.contains(r#""count":1"#), "{first}");
        assert!(first.contains(r#""chars":9"#), "{first}");
        assert!(first.contains(r#""engine":"Servo 0.8.0""#), "{first}");

        let info = app.handle_request("host_info", "").unwrap();
        assert!(info.contains(r#""requests":1"#), "{info}");
    }

    #[test]
    fn rejects_unknown_command() {
        let mut app = StarterApp::default();
        assert!(app.handle_request("save", "denied").is_err());
    }
}

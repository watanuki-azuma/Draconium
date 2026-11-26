/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Shared JSON field access for local settings and bridge payloads.

/// Escape a string as a JSON string literal (including surrounding quotes).
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Minimal flat JSON object reader for bridge payloads (zero external crates).
/// Supports top-level string, boolean, u32, and string-array fields with
/// standard JSON escape sequences (`\"`, `\\`, `\n`, `\r`, `\t`, `\uXXXX`).
pub fn json_get_string(json: &str, key: &str) -> Option<String> {
    let val = find_top_level_field(json, key)?;
    let (s, _) = parse_string(val)?;
    Some(s)
}

pub fn json_get_bool(json: &str, key: &str) -> Option<bool> {
    let val = find_top_level_field(json, key)?.trim_start();
    if val.starts_with("true") {
        Some(true)
    } else if val.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub fn json_get_u32(json: &str, key: &str) -> Option<u32> {
    let val = find_top_level_field(json, key)?.trim_start();
    let end = val.find(|c: char| !c.is_ascii_digit()).unwrap_or(val.len());
    if end == 0 {
        None
    } else {
        val[..end].parse::<u32>().ok()
    }
}

pub fn json_get_strict_u32(json: &str, key: &str) -> Option<u32> {
    let value = find_top_level_field(json, key)?;
    let end = value.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, rest) = value.split_at(end);
    if (digits.len() > 1 && digits.starts_with('0'))
        || !rest
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .starts_with([',', '}'])
    {
        return None;
    }
    digits.parse().ok()
}

pub fn json_get_string_array(json: &str, key: &str) -> Option<Vec<String>> {
    let mut rest = find_top_level_field(json, key)?.trim_start();
    rest = rest.strip_prefix('[')?;
    let mut out = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') {
            return Some(out);
        }
        let (item, after) = parse_string(rest)?;
        out.push(item);
        rest = after.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
        } else if rest.starts_with(']') {
            return Some(out);
        } else {
            return None;
        }
    }
}

fn find_top_level_field<'a>(json: &'a str, target_key: &str) -> Option<&'a str> {
    let mut rest = json.trim_start().strip_prefix('{')?;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('}') {
            return None;
        }
        let (key, after_key) = parse_string(rest)?;
        let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
        if key == target_key {
            return Some(after_colon);
        }
        let after_val = skip_json_value(after_colon)?.trim_start();
        if let Some(next) = after_val.strip_prefix(',') {
            rest = next;
        } else {
            return None;
        }
    }
}

/// Decode a JSON string at the start of `input` and return the unconsumed suffix.
pub fn parse_string(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => return Some((out, &rest[idx + 1..])),
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{08}'),
                    'f' => out.push('\u{0C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let code = read_hex4(&mut chars)?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            let (_, b1) = chars.next()?;
                            let (_, b2) = chars.next()?;
                            if b1 != '\\' || b2 != 'u' {
                                return None;
                            }
                            let low = read_hex4(&mut chars)?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return None;
                            }
                            let cp = 0x10000
                                + ((u32::from(code) - 0xD800) << 10)
                                + (u32::from(low) - 0xDC00);
                            out.push(char::from_u32(cp)?);
                        } else {
                            out.push(char::from_u32(u32::from(code))?);
                        }
                    }
                    _ => return None,
                }
            }
            '\u{0000}'..='\u{001f}' => return None,
            ch => out.push(ch),
        }
    }
    None
}

fn read_hex4(chars: &mut std::str::CharIndices<'_>) -> Option<u16> {
    let mut val = 0u16;
    for _ in 0..4 {
        let (_, ch) = chars.next()?;
        let digit = ch.to_digit(16)? as u16;
        val = (val << 4) | digit;
    }
    Some(val)
}

fn skip_json_value(input: &str) -> Option<&str> {
    let s = input.trim_start();
    let first = s.chars().next()?;
    match first {
        '"' => parse_string(s).map(|(_, after)| after),
        '{' | '[' => {
            let mut depth = 0usize;
            let mut in_str = false;
            let mut escaped = false;
            for (idx, ch) in s.char_indices() {
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_str = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_str = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some(&s[idx + 1..]);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => {
            let end = s
                .find(|c: char| c == ',' || c == '}' || c == ']' || c.is_whitespace())
                .unwrap_or(s.len());
            if end == 0 { None } else { Some(&s[end..]) }
        }
    }
}

/// Byte span of a top-level field's original JSON value.
pub fn value_span(json: &str, key: &str) -> Option<(usize, usize)> {
    let value = find_top_level_field(json, key)?;
    let rest = skip_json_value(value)?;
    Some((json.len() - value.len(), json.len() - rest.len()))
}

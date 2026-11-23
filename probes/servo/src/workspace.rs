/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Host handler for Draconium Folio (`--workspace`), a local-first Markdown
//! and knowledge workspace demonstrating Draconium's Servo 0.8.0 host API.
//!
//! All notes are stored as standard UTF-8 `.md` files inside the workspace
//! directory with optional lightweight frontmatter (`title`, `tags`). This
//! module uses only the Rust standard library so it can be unit-tested
//! standalone via `rustc --edition 2024 --test -D warnings probes/servo/src/workspace.rs`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_NOTE_BYTES: usize = 4 * 1024 * 1024;
const INIT_MARKER: &str = ".folio-initialized";

const SEED_NOTES: &[(&str, &str)] = &[
    (
        "welcome",
        "---\n\
title: Welcome to Draconium Folio\n\
tags: guide, markdown, desktop\n\
---\n\n\
# Welcome to Draconium Folio\n\n\
Folio is a local-first Markdown workspace running on **Draconium** — a lightweight Rust desktop host powered by **Servo 0.8.0**, with zero Chromium or OS WebView dependencies.\n\n\
## What you can try right here\n\n\
- **Live split writing**: Switch between *Write*, *Split*, and *Preview* modes from the titlebar.\n\
- **Drag and drop**: Drop any `.md` or `.txt` file from your file manager onto this window to import it as a local note.\n\
- **Native desktop actions**: Copy formatted HTML to the system clipboard, or export the current view to **PDF**, **PNG**, or **Markdown** using the toolbar.\n\
- **System theme parity**: Folio tracks your operating system's Light and Dark mode automatically, or you can toggle it in the status bar.\n\
- **Render Lab**: Open *Lab* in the header to inspect live frame pacing and run a 60-frame rendering benchmark on WebRender.\n\n\
> Every note is stored as a plain UTF-8 `.md` file on disk. Changes are saved automatically using atomic temporary-file replacement, and unsaved drafts flush safely before the window closes.\n\n\
```rust\n\
let config = HostConfig::new(\"Draconium Folio\", url)\n\
    .with_window(WindowConfig::custom_chrome(1240, 800))\n\
    .with_capabilities(Capabilities::standard_desktop());\n\
host::run(config, WorkspaceApp::new(data_dir), None)?;\n\
```\n",
    ),
    (
        "architecture-notes",
        "---\n\
title: Host Architecture Overview\n\
tags: architecture, servo, security\n\
---\n\n\
# Host Architecture Overview\n\n\
Draconium keeps application state and OS access inside a small Rust process while delegating layout, styling, and scripting to embedded Servo 0.8.0.\n\n\
## Security Boundaries\n\n\
| Layer | Mechanism | Purpose |\n\
| --- | --- | --- |\n\
| Asset serving | `dn://app/` + embedded fallback | Eliminates `file://` path leaks and ships single-binary builds |\n\
| Content policy | Strict `Content-Security-Policy` | Blocks inline script execution and unexpected external origins |\n\
| IPC bridge | 128-bit session token + `Object.freeze` | Prevents forged privileged commands from page scripts |\n\
| Capabilities | Declarative `Capabilities` ACL | Restricts each app mode and attenuates child windows |\n\n\
## Desktop Primitives Wired in Rust\n\n\
1. Frameless custom chrome (`drag`, 8-edge `dragResize`, minimize, maximize, rounded alpha surface)\n\
2. Correlated non-blocking IPC (`Draconium.invokeAsync`) alongside synchronous calls\n\
3. Capability-scoped file access tokens for drag-and-drop and native file pickers\n\
4. Rich Win32 clipboard (`CF_UNICODETEXT`, `CF_HTML`, `CF_DIB`), DPAPI `safeStorage`, and raster PDF export\n",
    ),
    (
        "release-checklist",
        "---\n\
title: Desktop Release Checklist\n\
tags: workflow, checklist\n\
---\n\n\
# Desktop Release Checklist\n\n\
Track verification items before cutting a portable Windows distribution.\n\n\
- [x] Verify standalone Rust unit tests pass with `-D warnings`\n\
- [x] Confirm `dn://app/` embedded fallback runs without external UI files\n\
- [x] Test UTF-8 multiline, Japanese IME, and emoji round-trip persistence\n\
- [x] Validate strict CSP with zero inline scripts or stylesheets\n\
- [ ] Run `scripts/verify-windows.ps1 -Build` on a clean Windows x64 machine\n\
- [ ] Package a portable bundle with `scripts/package.ps1`\n\n\
## Keyboard Shortcuts\n\n\
- `Ctrl+N` creates a new note in the current workspace.\n\
- `Ctrl+S` flushes pending edits immediately to disk.\n\
- `Ctrl+1` / `Ctrl+2` / `Ctrl+3` switches between *Write*, *Split*, and *Preview*.\n\
- `Ctrl+Shift+L` opens or closes the Render & Host Lab.\n",
    ),
];

/// Resolves the default directory where Folio stores `.md` notes.
pub fn default_workspace_dir(custom: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = custom {
        return dir;
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA")
        && !local_app_data.is_empty()
    {
        return PathBuf::from(local_app_data)
            .join("Draconium")
            .join("workspace");
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("draconium").join("workspace");
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("draconium")
            .join("workspace");
    }
    std::env::temp_dir().join("draconium-workspace")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedNote {
    id: String,
    title: String,
    tags: Vec<String>,
    content: String,
    excerpt: String,
    bytes: usize,
    updated_at: u64,
}

/// Host handler managing local Markdown notes and rendering benchmark telemetry.
pub struct WorkspaceApp {
    data_dir: PathBuf,
}

impl WorkspaceApp {
    /// Creates a new workspace handler storing `.md` notes inside `data_dir`.
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Dispatches UI commands from `Draconium.invoke` / `Draconium.invokeAsync`.
    pub fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command {
            "list_notes" => self.list_notes(),
            "load_note" => self.load_note(payload),
            "save_note" => self.save_note(payload),
            "create_note" => self.create_note(payload),
            "delete_note" => self.delete_note(payload),
            "import_note" => self.import_note(payload),
            "record_benchmark" => self.record_benchmark(payload),
            other => Err(format!("Unsupported workspace command: {other}")),
        }
    }

    fn ensure_seeded(&self) -> Result<(), String> {
        fs::create_dir_all(&self.data_dir)
            .map_err(|err| format!("Failed to prepare workspace directory: {err}"))?;
        let marker = self.data_dir.join(INIT_MARKER);
        if marker.exists() {
            return Ok(());
        }
        let has_md = fs::read_dir(&self.data_dir)
            .map_err(|err| format!("Failed to scan workspace directory: {err}"))?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            });
        if !has_md {
            for (id, raw) in SEED_NOTES {
                let path = self.note_path(id)?;
                atomic_write(&self.data_dir, &path, &format!("{id}.md.tmp"), raw)
                    .map_err(|err| format!("Failed to seed note '{id}': {err}"))?;
            }
        }
        let _ = fs::write(&marker, b"v1\n");
        Ok(())
    }

    fn note_path(&self, id: &str) -> Result<PathBuf, String> {
        validate_note_id(id)?;
        Ok(self.data_dir.join(format!("{id}.md")))
    }

    fn read_parsed_note(&self, id: &str) -> Result<ParsedNote, String> {
        let path = self.note_path(id)?;
        let meta = fs::metadata(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => format!("Note not found: {id}"),
            _ => format!("Failed to inspect note '{id}': {err}"),
        })?;
        let raw = fs::read_to_string(&path)
            .map_err(|err| format!("Failed to read note '{id}': {err}"))?;
        let clean = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let (title, tags, content) = parse_frontmatter(clean, id);
        let excerpt = build_excerpt(&content);
        let updated_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(ParsedNote {
            id: id.to_owned(),
            title,
            tags,
            content,
            excerpt,
            bytes: raw.len(),
            updated_at,
        })
    }

    /// Lists all `.md` notes in the workspace directory, sorted by recency then ID.
    pub fn list_notes(&self) -> Result<String, String> {
        self.ensure_seeded()?;
        let entries = fs::read_dir(&self.data_dir)
            .map_err(|err| format!("Failed to list workspace directory: {err}"))?;
        let mut notes = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_md = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
            if !is_md {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if validate_note_id(stem).is_err() {
                continue;
            }
            if let Ok(parsed) = self.read_parsed_note(stem) {
                notes.push(parsed);
            }
        }
        notes.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut items = Vec::with_capacity(notes.len());
        for note in &notes {
            items.push(format!(
                r#"{{"id":{},"title":{},"tags":{},"excerpt":{},"bytes":{},"updatedAt":{}}}"#,
                json_escape(&note.id),
                json_escape(&note.title),
                json_string_array(&note.tags),
                json_escape(&note.excerpt),
                note.bytes,
                note.updated_at
            ));
        }

        let dir_display = self.data_dir.to_string_lossy();
        Ok(format!(
            r#"{{"workspaceDir":{},"noteCount":{},"notes":[{}]}}"#,
            json_escape(&dir_display),
            notes.len(),
            items.join(",")
        ))
    }

    /// Loads one note by ID (`{"id":"welcome"}`).
    pub fn load_note(&self, payload: &str) -> Result<String, String> {
        self.ensure_seeded()?;
        let id = json_get_string(payload, "id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Missing note id".to_owned())?;
        let note = self.read_parsed_note(&id)?;
        Ok(serialize_full_note(&note))
    }

    /// Atomically saves a note (`{"id":"...","title":"...","tags":"a, b","content":"..."}`).
    pub fn save_note(&self, payload: &str) -> Result<String, String> {
        let id = json_get_string(payload, "id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Missing note id".to_owned())?;
        let title_raw = json_get_string(payload, "title").unwrap_or_default();
        let tags_raw = json_get_string(payload, "tags").unwrap_or_default();
        let content =
            json_get_string(payload, "content").ok_or_else(|| "Missing note content".to_owned())?;
        if content.len() > MAX_NOTE_BYTES {
            return Err("Note exceeds the 4 MiB limit".to_owned());
        }
        let path = self.note_path(&id)?;
        let title = sanitize_title(&title_raw, &id);
        let tags = parse_tag_list(&tags_raw);
        let serialized = format_markdown_document(&title, &tags, &content);
        atomic_write(&self.data_dir, &path, &format!("{id}.md.tmp"), &serialized)
            .map_err(|err| format!("Failed to save note '{id}': {err}"))?;
        let _ = fs::write(self.data_dir.join(INIT_MARKER), b"v1\n");
        let saved = self.read_parsed_note(&id)?;
        Ok(format!(
            r#"{{"saved":true,"id":{},"title":{},"tags":{},"excerpt":{},"bytes":{},"updatedAt":{}}}"#,
            json_escape(&saved.id),
            json_escape(&saved.title),
            json_string_array(&saved.tags),
            json_escape(&saved.excerpt),
            saved.bytes,
            saved.updated_at
        ))
    }

    /// Creates a new note with a unique slugified ID.
    pub fn create_note(&self, payload: &str) -> Result<String, String> {
        self.ensure_seeded()?;
        let title_raw = json_get_string(payload, "title").unwrap_or_default();
        let tags_raw = json_get_string(payload, "tags").unwrap_or_default();
        let content_raw = json_get_string(payload, "content");

        let title = if title_raw.trim().is_empty() {
            "Untitled note".to_owned()
        } else {
            sanitize_title(&title_raw, "untitled-note")
        };
        let tags = parse_tag_list(&tags_raw);
        let id = self.allocate_unique_id(&title);
        let content = content_raw.unwrap_or_else(|| format!("# {title}\n\n"));
        if content.len() > MAX_NOTE_BYTES {
            return Err("Note exceeds the 4 MiB limit".to_owned());
        }
        let path = self.note_path(&id)?;
        let serialized = format_markdown_document(&title, &tags, &content);
        atomic_write(&self.data_dir, &path, &format!("{id}.md.tmp"), &serialized)
            .map_err(|err| format!("Failed to create note '{id}': {err}"))?;
        let created = self.read_parsed_note(&id)?;
        Ok(serialize_full_note(&created))
    }

    /// Deletes a note by ID (`{"id":"..."}`).
    pub fn delete_note(&self, payload: &str) -> Result<String, String> {
        let id = json_get_string(payload, "id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Missing note id".to_owned())?;
        let path = self.note_path(&id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(format!(r#"{{"deleted":true,"id":{}}}"#, json_escape(&id))),
            Err(err) if err.kind() == ErrorKind::NotFound => Err(format!("Note not found: {id}")),
            Err(err) => Err(format!("Failed to delete note '{id}': {err}")),
        }
    }

    /// Imports an external `.md` or `.txt` file payload as a new workspace note.
    pub fn import_note(&self, payload: &str) -> Result<String, String> {
        self.ensure_seeded()?;
        let file_name =
            json_get_string(payload, "fileName").unwrap_or_else(|| "imported.md".to_owned());
        let raw_content = json_get_string(payload, "content")
            .ok_or_else(|| "Missing imported file content".to_owned())?;
        if raw_content.len() > MAX_NOTE_BYTES {
            return Err("Imported file exceeds the 4 MiB limit".to_owned());
        }
        let clean = raw_content.strip_prefix('\u{FEFF}').unwrap_or(&raw_content);
        let base_stem = Path::new(&file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported-note");
        let (title, mut tags, content) = parse_frontmatter(clean, base_stem);
        if tags.is_empty() {
            tags.push("imported".to_owned());
        }
        let id = self.allocate_unique_id(&title);
        let path = self.note_path(&id)?;
        let serialized = format_markdown_document(&title, &tags, &content);
        atomic_write(&self.data_dir, &path, &format!("{id}.md.tmp"), &serialized)
            .map_err(|err| format!("Failed to import note '{id}': {err}"))?;
        let imported = self.read_parsed_note(&id)?;
        Ok(serialize_full_note(&imported))
    }

    /// Persists rendering benchmark metrics to `benchmark.json`.
    pub fn record_benchmark(&self, payload: &str) -> Result<String, String> {
        let trimmed = payload.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            return Err("Benchmark payload must be a JSON object".to_owned());
        }
        let frames = json_get_u32(trimmed, "frames").unwrap_or(0);
        let dom_nodes = json_get_u32(trimmed, "domNodes").unwrap_or(0);
        let path = self.data_dir.join("benchmark.json");
        atomic_write(&self.data_dir, &path, "benchmark.json.tmp", trimmed)
            .map_err(|err| format!("Failed to save benchmark metrics: {err}"))?;
        Ok(format!(
            r#"{{"recorded":true,"frames":{frames},"domNodes":{dom_nodes}}}"#
        ))
    }

    fn allocate_unique_id(&self, title: &str) -> String {
        let base = slugify(title);
        if !self.data_dir.join(format!("{base}.md")).exists() {
            return base;
        }
        for idx in 2..=999 {
            let candidate = format!("{base}-{idx}");
            if !self.data_dir.join(format!("{candidate}.md")).exists() {
                return candidate;
            }
        }
        format!("note-{}", std::process::id())
    }
}

fn serialize_full_note(note: &ParsedNote) -> String {
    format!(
        r#"{{"id":{},"title":{},"tags":{},"excerpt":{},"content":{},"bytes":{},"updatedAt":{}}}"#,
        json_escape(&note.id),
        json_escape(&note.title),
        json_string_array(&note.tags),
        json_escape(&note.excerpt),
        json_escape(&note.content),
        note.bytes,
        note.updated_at
    )
}

fn validate_note_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err("Note id must be 1..=64 characters".to_owned());
    }
    if id.starts_with('.') || id.starts_with('-') {
        return Err(format!("Invalid note id: {id}"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(format!("Note id contains disallowed characters: {id}"));
    }
    let upper = id.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        return Err(format!(
            "Reserved device name cannot be used as note id: {id}"
        ));
    }
    Ok(())
}

fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let trimmed = slug.trim_matches('-').to_owned();
    if trimmed.is_empty() || validate_note_id(&trimmed).is_err() {
        "note".to_owned()
    } else {
        trimmed
    }
}

fn sanitize_title(raw: &str, fallback_id: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned();
    if clean.is_empty() {
        fallback_id.replace(['-', '_'], " ")
    } else {
        clean.chars().take(120).collect()
    }
}

fn parse_tag_list(raw: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for part in raw.split([',', ';']) {
        let cleaned: String = part
            .trim()
            .trim_start_matches('#')
            .chars()
            .filter(|c| !c.is_control() && *c != ',' && *c != '\n' && *c != '\r')
            .collect();
        let normalized = cleaned.trim().to_owned();
        if !normalized.is_empty()
            && normalized.len() <= 32
            && !tags
                .iter()
                .any(|t: &String| t.eq_ignore_ascii_case(&normalized))
        {
            tags.push(normalized);
        }
        if tags.len() >= 8 {
            break;
        }
    }
    tags
}

fn parse_frontmatter(raw: &str, fallback_id: &str) -> (String, Vec<String>, String) {
    let normalized = raw.replace("\r\n", "\n");
    let mut title = String::new();
    let mut tags = Vec::new();
    let body: String;

    if let Some(rest) = normalized.strip_prefix("---\n")
        && let Some(end_idx) = rest.find("\n---\n")
    {
        let header = &rest[..end_idx];
        body = rest[end_idx + 5..].trim_start_matches('\n').to_owned();
        for line in header.lines() {
            if let Some(val) = line.strip_prefix("title:") {
                title = sanitize_title(val, fallback_id);
            } else if let Some(val) = line.strip_prefix("tags:") {
                tags = parse_tag_list(val);
            }
        }
    } else {
        body = normalized;
    }

    if title.is_empty() {
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix("# ") {
                title = sanitize_title(heading, fallback_id);
                break;
            }
        }
    }
    if title.is_empty() {
        title = sanitize_title(fallback_id, fallback_id);
    }
    (title, tags, body)
}

fn format_markdown_document(title: &str, tags: &[String], content: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("title: ");
    out.push_str(title);
    out.push('\n');
    if !tags.is_empty() {
        out.push_str("tags: ");
        out.push_str(&tags.join(", "));
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(&normalized);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn build_excerpt(content: &str) -> String {
    let mut fallback_heading: Option<String> = None;
    for line in content.lines() {
        let raw_trimmed = line.trim();
        if raw_trimmed.is_empty()
            || raw_trimmed.starts_with("```")
            || raw_trimmed.starts_with('|')
            || raw_trimmed.starts_with("---")
        {
            continue;
        }
        if raw_trimmed.starts_with('#') {
            if fallback_heading.is_none() {
                let heading_text = raw_trimmed.trim_start_matches('#').trim();
                if !heading_text.is_empty() {
                    fallback_heading = Some(heading_text.to_owned());
                }
            }
            continue;
        }
        let trimmed = raw_trimmed
            .trim_start_matches('>')
            .trim_start_matches("- [x] ")
            .trim_start_matches("- [ ] ")
            .trim_start_matches("- ")
            .trim();
        if trimmed.is_empty() {
            continue;
        }
        let plain = trimmed.replace("**", "").replace('`', "");
        let mut excerpt: String = plain.chars().take(92).collect();
        if plain.chars().count() > 92 {
            excerpt.push_str("...");
        }
        return excerpt;
    }
    if let Some(heading) = fallback_heading {
        let mut excerpt: String = heading.chars().take(92).collect();
        if heading.chars().count() > 92 {
            excerpt.push_str("...");
        }
        return excerpt;
    }
    "Empty note".to_owned()
}

fn atomic_write(
    dir: &Path,
    dst: &Path,
    tmp_name: &str,
    content: &str,
) -> Result<(), std::io::Error> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(tmp_name);
    fs::write(&tmp, content.as_bytes())?;
    if let Err(err) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
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
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_string_array(items: &[String]) -> String {
    let encoded: Vec<String> = items.iter().map(|item| json_escape(item)).collect();
    format!("[{}]", encoded.join(","))
}

fn json_find_key_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                let (parsed, next_i) = parse_json_string(json, i)?;
                i = next_i;
                if depth == 1 && parsed == key {
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == b':' {
                        i += 1;
                        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                            i += 1;
                        }
                        return Some(&json[i..]);
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

fn parse_json_string(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    if bytes.get(start).copied() != Some(b'"') {
        return None;
    }
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            return Some((out, i + 1));
        }
        if b == b'\\' {
            i += 1;
            let esc = *bytes.get(i)?;
            match esc {
                b'"' => {
                    out.push('"');
                    i += 1;
                }
                b'\\' => {
                    out.push('\\');
                    i += 1;
                }
                b'/' => {
                    out.push('/');
                    i += 1;
                }
                b'n' => {
                    out.push('\n');
                    i += 1;
                }
                b'r' => {
                    out.push('\r');
                    i += 1;
                }
                b't' => {
                    out.push('\t');
                    i += 1;
                }
                b'b' => {
                    out.push('\u{0008}');
                    i += 1;
                }
                b'f' => {
                    out.push('\u{000C}');
                    i += 1;
                }
                b'u' => {
                    let hex = input.get(i + 1..i + 5)?;
                    let code = u16::from_str_radix(hex, 16).ok()?;
                    i += 5;
                    if (0xD800..=0xDBFF).contains(&code) && input.get(i..i + 2) == Some("\\u") {
                        let low_hex = input.get(i + 2..i + 6)?;
                        let low = u16::from_str_radix(low_hex, 16).ok()?;
                        if (0xDC00..=0xDFFF).contains(&low) {
                            let combined =
                                0x10000 + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                            if let Some(ch) = char::from_u32(combined) {
                                out.push(ch);
                            }
                            i += 6;
                            continue;
                        }
                    }
                    if let Some(ch) = char::from_u32(code as u32) {
                        out.push(ch);
                    }
                }
                _ => return None,
            }
        } else {
            let rest = &input[i..];
            let ch = rest.chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    None
}

fn json_get_string(json: &str, key: &str) -> Option<String> {
    let val = json_find_key_value(json, key)?;
    let (parsed, _) = parse_json_string(val, 0)?;
    Some(parsed)
}

fn json_get_u32(json: &str, key: &str) -> Option<u32> {
    let val = json_find_key_value(json, key)?;
    let end = val.find(|c: char| !c.is_ascii_digit()).unwrap_or(val.len());
    val[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_workspace(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dn-workspace-{name}-{stamp}"))
    }

    #[test]
    fn seeds_starter_notes_and_lists_workspace() {
        let dir = temp_workspace("seed");
        let mut app = WorkspaceApp::new(dir.clone());
        let list = app.handle_request("list_notes", "{}").expect("list_notes");
        assert!(list.contains(r#""noteCount":3"#), "{list}");
        assert!(list.contains(r#""id":"welcome""#), "{list}");
        assert!(list.contains(r#""id":"architecture-notes""#), "{list}");
        assert!(list.contains(r#""id":"release-checklist""#), "{list}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_load_preserve_utf8_japanese_emoji_and_bom() {
        let dir = temp_workspace("utf8");
        let mut app = WorkspaceApp::new(dir.clone());
        let _ = app.handle_request("list_notes", "{}").unwrap();

        let save_payload = r#"{"id":"welcome","title":"日本語ノート","tags":"rust, servo, メモ","content":"1行目\n2行目😀\n\uD83D\uDE80 Launch"}"#;
        let saved = app
            .handle_request("save_note", save_payload)
            .expect("save_note");
        assert!(saved.contains(r#""saved":true"#), "{saved}");

        // Prepend a UTF-8 BOM externally to confirm tolerant reading.
        let note_file = dir.join("welcome.md");
        let raw = fs::read_to_string(&note_file).unwrap();
        fs::write(&note_file, format!("\u{FEFF}{raw}")).unwrap();

        let loaded = app
            .handle_request("load_note", r#"{"id":"welcome"}"#)
            .expect("load_note");
        assert!(loaded.contains("日本語ノート"), "{loaded}");
        assert!(loaded.contains("2行目😀"), "{loaded}");
        assert!(loaded.contains("🚀 Launch"), "{loaded}");
        assert!(!loaded.contains('\u{FEFF}'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_import_delete_and_benchmark_roundtrip() {
        let dir = temp_workspace("crud");
        let mut app = WorkspaceApp::new(dir.clone());

        let created = app
            .handle_request(
                "create_note",
                r##"{"title":"Design System Notes","tags":"ui, tokens","content":"# Design\nClean planes."}"##,
            )
            .expect("create_note");
        assert!(
            created.contains(r#""id":"design-system-notes""#),
            "{created}"
        );

        let imported = app
            .handle_request(
                "import_note",
                r##"{"fileName":"meeting-summary.md","content":"# Q4 Plan\nShip Servo 0.8.0."}"##,
            )
            .expect("import_note");
        assert!(imported.contains(r#""id":"q4-plan""#), "{imported}");

        let deleted = app
            .handle_request("delete_note", r#"{"id":"design-system-notes"}"#)
            .expect("delete_note");
        assert!(deleted.contains(r#""deleted":true"#), "{deleted}");

        let bench = app
            .handle_request(
                "record_benchmark",
                r#"{"frames":60,"fps":60,"avgFrameMs":16,"p95FrameMs":20,"domNodes":190}"#,
            )
            .expect("record_benchmark");
        assert!(bench.contains(r#""recorded":true"#), "{bench}");
        assert!(dir.join("benchmark.json").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn excerpt_skips_heading_lines_for_body_preview() {
        let with_body = "# Welcome Title\n\nFolio is a **local-first** workspace.\n";
        assert_eq!(
            build_excerpt(with_body),
            "Folio is a local-first workspace."
        );

        let heading_only = "# Only Heading\n\n";
        assert_eq!(build_excerpt(heading_only), "Only Heading");

        assert_eq!(build_excerpt("   \n\n"), "Empty note");
    }

    #[test]
    fn rejects_traversal_and_reserved_note_ids() {
        let dir = temp_workspace("security");
        let mut app = WorkspaceApp::new(dir.clone());
        for bad in ["../escape", "sub/note", "CON", ".hidden", "bad:id", ""] {
            let payload = format!(r#"{{"id":{}}}"#, json_escape(bad));
            assert!(
                app.handle_request("load_note", &payload).is_err(),
                "expected rejection for id={bad:?}"
            );
            assert!(
                app.handle_request("delete_note", &payload).is_err(),
                "expected delete rejection for id={bad:?}"
            );
        }
        assert!(app.handle_request("unknown_cmd", "{}").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}

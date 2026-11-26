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

#[cfg(not(test))]
use draconium_host::json;
// Also compiled as an independent crate by the standalone test harness.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../crates/draconium-host/src/json.rs"]
mod json;
use json::{json_get_string, json_get_u32, json_string as json_escape};

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_NOTE_BYTES: usize = 4 * 1024 * 1024;
// A full-size note can also have a title, tags, and a UTF-8 BOM.
const MAX_NOTE_FILE_BYTES: usize = MAX_NOTE_BYTES + 4096;
const INIT_MARKER: &str = ".folio-initialized";
const TRASH_DIR: &str = ".folio-trash";
const EXTERNAL_ID_PREFIX: &str = "file:";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
            "list_notes" => match json_get_string(payload, "query") {
                Some(query) => self.search_notes(query.trim()),
                None => self.list_notes(),
            },
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
                atomic_write(&self.data_dir, &path, raw)
                    .map_err(|err| format!("Failed to seed note '{id}': {err}"))?;
            }
        }
        let _ = fs::write(&marker, b"v1\n");
        Ok(())
    }

    fn note_path(&self, id: &str) -> Result<PathBuf, String> {
        if let Some(name) = id.strip_prefix(EXTERNAL_ID_PREFIX) {
            let path = Path::new(name);
            if let Some(Component::Normal(component)) = path.components().next()
                && component == name
                && is_markdown_file(path)
                && (!cfg!(windows) || !name.contains(':'))
            {
                return Ok(self.data_dir.join(name));
            }
            return Err("Invalid external note id".to_owned());
        }
        validate_note_id(id)?;
        Ok(self.data_dir.join(format!("{id}.md")))
    }

    fn read_parsed_note(&self, id: &str) -> Result<ParsedNote, String> {
        let path = self.note_path(id)?;
        self.read_parsed_note_at(&path, id)
    }

    fn read_parsed_note_at(&self, path: &Path, id: &str) -> Result<ParsedNote, String> {
        let (raw, meta) = read_note_file(path, id)?;
        let clean = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let document = parse_document(clean, note_stem(path));
        let content = document.content.replace("\r\n", "\n");
        check_note_body_size(&content, document.frontmatter.is_some())?;
        let excerpt = build_excerpt(&content);
        let updated_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(ParsedNote {
            id: id.to_owned(),
            title: document.title,
            tags: document.tags,
            content,
            excerpt,
            bytes: raw.len(),
            updated_at,
        })
    }

    /// Lists all `.md` notes in the workspace directory, sorted by recency then ID.
    pub fn list_notes(&self) -> Result<String, String> {
        self.search_notes("")
    }

    fn search_notes(&self, query: &str) -> Result<String, String> {
        self.ensure_seeded()?;
        let entries = fs::read_dir(&self.data_dir)
            .map_err(|err| format!("Failed to list workspace directory: {err}"))?;
        let query = query.to_lowercase();
        let mut items = Vec::new();
        let mut note_count = 0;
        let mut unreadable_count = 0;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !is_markdown_file(&path) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                unreadable_count += 1;
                continue;
            };
            let id = if let Some(stem) = name.strip_suffix(".md")
                && validate_note_id(stem).is_ok()
            {
                stem.to_owned()
            } else {
                format!("{EXTERNAL_ID_PREFIX}{name}")
            };
            let Ok(note) = self.read_parsed_note_at(&path, &id) else {
                unreadable_count += 1;
                continue;
            };
            note_count += 1;
            if !query.is_empty()
                && !note.title.to_lowercase().contains(&query)
                && !note
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&query))
                && !note.excerpt.to_lowercase().contains(&query)
                && !note.content.to_lowercase().contains(&query)
            {
                continue;
            }
            let item = format!(
                r#"{{"id":{},"title":{},"tags":{},"excerpt":{},"bytes":{},"updatedAt":{}}}"#,
                json_escape(&note.id),
                json_escape(&note.title),
                json_string_array(&note.tags),
                json_escape(&note.excerpt),
                note.bytes,
                note.updated_at
            );
            items.push((note.updated_at, note.id, item));
        }
        items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let items: Vec<_> = items.into_iter().map(|(_, _, item)| item).collect();

        let dir_display = self.data_dir.to_string_lossy();
        Ok(format!(
            r#"{{"workspaceDir":{},"noteCount":{note_count},"unreadableCount":{unreadable_count},"maxNoteBytes":{MAX_NOTE_BYTES},"notes":[{}]}}"#,
            json_escape(&dir_display),
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
        let path = self.note_path(&id)?;
        // A delayed save must not recreate a note moved to the trash.
        let (raw, _) = read_note_file(&path, &id)?;
        let clean = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let document = parse_document(clean, note_stem(&path));
        check_note_body_size(&content, document.frontmatter.is_some())?;
        let title = sanitize_title(&title_raw, note_stem(&path));
        let tags = parse_tag_list(&tags_raw);
        let serialized = format_markdown_document(&title, &tags, &content, Some(&document))?;
        atomic_write(&self.data_dir, &path, &serialized)
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
        let id = self.allocate_unique_id(&title)?;
        let content = content_raw.unwrap_or_else(|| format!("# {title}\n\n"));
        if content.len() > MAX_NOTE_BYTES {
            return Err("Note exceeds the 4 MiB limit".to_owned());
        }
        let path = self.note_path(&id)?;
        let serialized = format_markdown_document(&title, &tags, &content, None)?;
        atomic_write(&self.data_dir, &path, &serialized)
            .map_err(|err| format!("Failed to create note '{id}': {err}"))?;
        let created = self.read_parsed_note(&id)?;
        Ok(serialize_full_note(&created))
    }

    /// Moves a note into a recoverable workspace trash folder (`{"id":"..."}`).
    pub fn delete_note(&self, payload: &str) -> Result<String, String> {
        let id = json_get_string(payload, "id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Missing note id".to_owned())?;
        let path = self.note_path(&id)?;
        note_metadata(&path, &id)?;
        let trash_path = move_to_trash(&path, &self.data_dir.join(TRASH_DIR))
            .map_err(|err| format!("Failed to move note '{id}' to the trash: {err}"))?;
        Ok(format!(
            r#"{{"deleted":true,"id":{},"trashPath":{}}}"#,
            json_escape(&id),
            json_escape(&trash_path.to_string_lossy())
        ))
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
        let document = parse_document(clean, note_stem(Path::new(&file_name)));
        let tags = if document.tags.is_empty() {
            &["imported".to_owned()]
        } else {
            document.tags.as_slice()
        };
        let id = self.allocate_unique_id(&document.title)?;
        let path = self.note_path(&id)?;
        let serialized =
            format_markdown_document(&document.title, tags, document.content, Some(&document))?;
        atomic_write(&self.data_dir, &path, &serialized)
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
        atomic_write(&self.data_dir, &path, trimmed)
            .map_err(|err| format!("Failed to save benchmark metrics: {err}"))?;
        Ok(format!(
            r#"{{"recorded":true,"frames":{frames},"domNodes":{dom_nodes}}}"#
        ))
    }

    fn allocate_unique_id(&self, title: &str) -> Result<String, String> {
        let base = slugify(title);
        for candidate in
            std::iter::once(base.clone()).chain((2..=10_000).map(|i| format!("{base}-{i}")))
        {
            match fs::symlink_metadata(self.note_path(&candidate)?) {
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(candidate),
                Err(err) => return Err(format!("Failed to allocate a note filename: {err}")),
            }
        }
        Err("Could not allocate a unique note filename".to_owned())
    }
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

fn note_stem(path: &Path) -> &str {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("imported-note")
}

fn note_metadata(path: &Path, id: &str) -> Result<Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => format!("Note not found: {id}"),
        _ => format!("Failed to inspect note '{id}': {err}"),
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!("Note '{id}' is not a regular file"));
    }
    Ok(metadata)
}

fn read_note_file(path: &Path, id: &str) -> Result<(String, Metadata), String> {
    let metadata = note_metadata(path, id)?;
    let limit = MAX_NOTE_FILE_BYTES as u64;
    let too_large = || format!("Note '{id}' exceeds the 4 MiB limit plus frontmatter allowance");
    if metadata.len() > limit {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|err| format!("Failed to read note '{id}': {err}"))?;
    if bytes.len() > MAX_NOTE_FILE_BYTES {
        return Err(too_large());
    }
    let raw = String::from_utf8(bytes).map_err(|err| format!("Note '{id}' is not UTF-8: {err}"))?;
    Ok((raw, metadata))
}

fn check_note_body_size(content: &str, has_frontmatter: bool) -> Result<(), String> {
    // Older Folio versions appended a newline after checking the body length.
    let limit = MAX_NOTE_BYTES + usize::from(has_frontmatter && content.ends_with('\n'));
    if content.len() > limit {
        return Err("Note exceeds the 4 MiB limit".to_owned());
    }
    Ok(())
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
            && normalized.chars().count() <= 32
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

struct NoteDocument<'a> {
    title: String,
    tags: Vec<String>,
    content: &'a str,
    frontmatter: Option<Frontmatter<'a>>,
}

struct Frontmatter<'a> {
    raw: &'a str,
    fields: Vec<MetadataField<'a>>,
}

struct MetadataField<'a> {
    key: &'a str,
    value: &'a str,
    start: usize,
    end: usize,
}

fn parse_document<'a>(raw: &'a str, fallback_id: &str) -> NoteDocument<'a> {
    let mut title = String::new();
    let mut tags = Vec::new();
    let (frontmatter, content) = match split_frontmatter(raw) {
        Some((frontmatter, content)) => (Some(frontmatter), content),
        None => (None, raw),
    };
    if let Some(frontmatter) = &frontmatter {
        for field in &frontmatter.fields {
            match field.key {
                "title" => title = sanitize_title(&decode_yaml_scalar(field.value), fallback_id),
                "tags" => {
                    let continuation = &frontmatter.raw[field.start..field.end];
                    tags = parse_metadata_tags(field.value, continuation);
                }
                _ => {}
            }
        }
    }
    if title.is_empty() {
        for line in content.lines() {
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
    NoteDocument {
        title,
        tags,
        content,
        frontmatter,
    }
}

fn split_frontmatter(raw: &str) -> Option<(Frontmatter<'_>, &str)> {
    let (opening, rest) = raw.split_once('\n')?;
    if opening.trim_end() != "---" {
        return None;
    }

    let mut offset = 0;
    let mut fields: Vec<MetadataField<'_>> = Vec::new();
    for line in rest.split_inclusive('\n') {
        let text = line.trim_end();
        let end = offset + line.len();
        if text == "---" {
            if fields.is_empty() {
                return None;
            }
            let content = &rest[end..];
            let content = content
                .strip_prefix("\r\n")
                .or_else(|| content.strip_prefix('\n'))
                .unwrap_or(content);
            return Some((
                Frontmatter {
                    raw: &rest[..offset],
                    fields,
                },
                content,
            ));
        }

        let trimmed = text.trim_start();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if text.starts_with([' ', '\t']) || text.starts_with("- ") {
                let field = fields.last_mut()?;
                if text.starts_with("- ")
                    && !field.value.is_empty()
                    && !field.value.starts_with('#')
                {
                    return None;
                }
                field.end = end;
            } else {
                let (key, value) = mapping_entry(text)?;
                fields.push(MetadataField {
                    key,
                    value,
                    start: offset,
                    end,
                });
            }
        }
        offset = end;
    }
    None
}

fn mapping_entry(text: &str) -> Option<(&str, &str)> {
    let (raw_key, value) = text.split_once(':')?;
    if !value.is_empty() && !value.starts_with(char::is_whitespace) {
        return None;
    }
    let key = raw_key.trim_end();
    let quoted_key = key
        .strip_prefix('"')
        .and_then(|key| key.strip_suffix('"'))
        .or_else(|| {
            key.strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
        });
    if let Some(key) = quoted_key {
        return Some((key, value.trim()));
    }
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return None;
    }
    Some((key, value.trim()))
}

fn decode_yaml_scalar(value: &str) -> String {
    if value.starts_with('"')
        && let Some((decoded, remaining)) = json::parse_string(value)
        && (remaining.trim().is_empty() || remaining.trim_start().starts_with('#'))
    {
        return decoded;
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return inner.replace("''", "'");
    }
    value.to_owned()
}

fn parse_metadata_tags(value: &str, block: &str) -> Vec<String> {
    if value.is_empty() || value.starts_with('#') {
        let tags: Vec<_> = block
            .lines()
            .skip(1)
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("- ")
                    .map(|value| decode_yaml_scalar(value.trim()))
            })
            .collect();
        return parse_tag_list(&tags.join(","));
    }
    if let Some(entries) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let tags: Vec<_> = entries
            .split(',')
            .map(|entry| decode_yaml_scalar(entry.trim()))
            .collect();
        return parse_tag_list(&tags.join(","));
    }
    parse_tag_list(&decode_yaml_scalar(value))
}

fn format_markdown_document(
    title: &str,
    tags: &[String],
    content: &str,
    original: Option<&NoteDocument<'_>>,
) -> Result<String, String> {
    let normalized = content.replace("\r\n", "\n");
    let mut out = String::from("---\n");
    let mut title_seen = false;
    let mut tags_seen = false;
    if let Some(document) = original
        && let Some(header) = &document.frontmatter
    {
        let mut offset = 0;
        for field in &header.fields {
            out.push_str(&header.raw[offset..field.start]);
            match field.key {
                "title" if document.title != title => {
                    if !title_seen {
                        write_metadata_title(&mut out, title);
                    }
                }
                "tags" if document.tags != tags => {
                    if !tags_seen {
                        write_metadata_tags(&mut out, tags);
                    }
                }
                _ => out.push_str(&header.raw[field.start..field.end]),
            }
            title_seen |= field.key == "title";
            tags_seen |= field.key == "tags";
            offset = field.end;
        }
        out.push_str(&header.raw[offset..]);
    }
    if !title_seen {
        write_metadata_title(&mut out, title);
    }
    if !tags_seen {
        write_metadata_tags(&mut out, tags);
    }
    out.push_str("---\n\n");
    out.push_str(&normalized);
    if out.len() > MAX_NOTE_FILE_BYTES {
        return Err("Note exceeds the 4 MiB limit plus frontmatter allowance".to_owned());
    }
    Ok(out)
}

fn write_metadata_title(out: &mut String, title: &str) {
    out.push_str("title: ");
    out.push_str(&json_escape(title));
    out.push('\n');
}

fn write_metadata_tags(out: &mut String, tags: &[String]) {
    if tags.is_empty() {
        return;
    }
    out.push_str("tags:\n");
    for tag in tags {
        out.push_str("  - ");
        out.push_str(&json_escape(tag));
        out.push('\n');
    }
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

fn unique_suffix() -> String {
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{serial}", std::process::id())
}

fn atomic_write(dir: &Path, dst: &Path, content: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(".folio-{}.tmp", unique_suffix()));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    let written = file.write_all(content.as_bytes());
    drop(file);
    let result = written.and_then(|()| fs::rename(&tmp, dst));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn move_to_trash(path: &Path, trash: &Path) -> std::io::Result<PathBuf> {
    match fs::create_dir(trash) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(trash)?.file_type().is_dir() {
                return Err(err);
            }
        }
        Err(err) => return Err(err),
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let entry_dir = loop {
        let dir = trash.join(format!("{timestamp}-{}", unique_suffix()));
        match fs::create_dir(&dir) {
            Ok(()) => break dir,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "Missing note filename"))?;
    let target = entry_dir.join(file_name);
    if let Err(err) = fs::rename(path, &target) {
        let _ = fs::remove_dir(&entry_dir);
        return Err(err);
    }
    Ok(std::path::absolute(&target).unwrap_or(target))
}

fn json_string_array(items: &[String]) -> String {
    let encoded: Vec<String> = items.iter().map(|item| json_escape(item)).collect();
    format!("[{}]", encoded.join(","))
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
        let trashed = PathBuf::from(json_get_string(&deleted, "trashPath").unwrap());
        assert!(trashed.starts_with(dir.join(TRASH_DIR)));
        assert!(
            fs::read_to_string(&trashed)
                .unwrap()
                .contains("Clean planes.")
        );
        assert!(!dir.join("design-system-notes.md").exists());
        assert!(
            app.handle_request(
                "save_note",
                r#"{"id":"design-system-notes","content":"stale edit"}"#
            )
            .is_err()
        );

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
    fn external_filenames_round_trip_and_search_matches_the_full_body() {
        let dir = temp_workspace("external");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026.11 日本語ノート.MD");
        let long_tag = "日本語でも保存できる長いタグ";
        let unknown = "author:\n  name: Writer\n  public: false\n";
        let raw = format!(
            "---\ntitle: External\n{unknown}tags:\n  - '{long_tag}'\n  - review\n---   \n\nAn excerpt.\n\nUnindexed SEARCHTERM.\n"
        );
        fs::write(&path, &raw).unwrap();
        fs::write(dir.join("other.md"), "Unrelated note").unwrap();
        let mut app = WorkspaceApp::new(dir.clone());
        let list = app
            .handle_request("list_notes", r#"{"query":" searchterm "}"#)
            .unwrap();
        assert!(list.contains(r#""noteCount":2"#), "{list}");
        assert!(!list.contains("Unrelated note"), "{list}");
        let id = json_get_string(list.split_once("\"notes\":[").unwrap().1, "id").unwrap();
        let load_payload = format!(r#"{{"id":{}}}"#, json_escape(&id));
        let loaded = app.handle_request("load_note", &load_payload).unwrap();
        assert!(loaded.contains(long_tag), "{loaded}");
        assert!(loaded.contains("Unindexed SEARCHTERM."), "{loaded}");
        let save_payload = format!(
            r#"{{"id":{},"title":"External","tags":"{long_tag}, review","content":"Changed body."}}"#,
            json_escape(&id)
        );
        app.handle_request("save_note", &save_payload).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains(unknown), "{saved}");
        assert!(
            saved.contains(&format!("tags:\n  - '{long_tag}'\n  - review\n")),
            "{saved}"
        );
        assert!(saved.ends_with("Changed body."), "{saved}");
        assert!(
            !app.handle_request("list_notes", r#"{"query":"searchterm"}"#)
                .unwrap()
                .contains("External")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontmatter_accepts_eof_delimiter_and_preserves_thematic_breaks() {
        let raw = "---\r\ntitle: Empty document\r\ntags:\r\n- rust\r\n--- \t";
        let parsed = parse_document(raw, "fallback");
        assert_eq!(parsed.title, "Empty document");
        assert_eq!(parsed.tags, ["rust"]);
        assert_eq!(parsed.content, "");

        let raw = "---\nA Markdown paragraph.\n---\nMore Markdown.";
        let parsed = parse_document(raw, "fallback");
        assert!(parsed.frontmatter.is_none());
        assert_eq!(parsed.content, raw);
        let formatted =
            format_markdown_document("Imported", &["guide".into()], raw, Some(&parsed)).unwrap();
        assert_eq!(parse_document(&formatted, "fallback").content, raw);
    }

    #[test]
    fn oversized_notes_are_rejected_and_full_size_notes_can_be_read() {
        let dir = temp_workspace("size");
        fs::create_dir_all(&dir).unwrap();
        File::create(dir.join("oversized.md"))
            .unwrap()
            .set_len(MAX_NOTE_FILE_BYTES as u64 + 1)
            .unwrap();
        fs::write(
            dir.join("oversized-body.md"),
            "a".repeat(MAX_NOTE_BYTES + 1),
        )
        .unwrap();
        let mut app = WorkspaceApp::new(dir.clone());
        for id in ["oversized", "oversized-body"] {
            let err = app
                .handle_request("load_note", &format!(r#"{{"id":"{id}"}}"#))
                .unwrap_err();
            assert!(err.contains("4 MiB"), "{err}");
        }
        let list = app.handle_request("list_notes", "{}").unwrap();
        assert!(list.contains(r#""unreadableCount":2"#), "{list}");
        let content = "a".repeat(MAX_NOTE_BYTES);
        let payload = format!(r#"{{"title":"Full size","content":"{content}"}}"#);
        app.handle_request("create_note", &payload).unwrap();
        let parsed = app.read_parsed_note("full-size").unwrap();
        assert_eq!(parsed.content.len(), MAX_NOTE_BYTES);
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
        for bad in [
            "../escape",
            "sub/note",
            "CON",
            ".hidden",
            "bad:id",
            "file:../escape.md",
            "file:sub/note.md",
            "",
        ] {
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

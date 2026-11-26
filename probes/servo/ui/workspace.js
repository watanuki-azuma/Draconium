(function () {
  "use strict";

  const state = {
    notes: [],
    maxNoteBytes: null,
    unreadableCount: 0,
    activeId: null,
    selectionVersion: 0,
    workspaceVersion: 0,
    deletingId: null,
    filterQuery: "",
    searchMatches: null,
    searchVersion: 0,
    searchTimer: null,
    activeTag: "",
    mode: "split",
    dirty: false,
    saveTimer: null,
    previewTimer: null,
    previewPending: false,
    toastTimer: null,
    theme: "light",
    labOpen: false,
    benchPromise: null,
    frameSamples: [],
  };

  function byId(id) {
    return document.getElementById(id);
  }

  function hasBridge() {
    return Boolean(window.Draconium && typeof window.Draconium.invoke === "function");
  }

  function syncWindowTitle() {
    const titleEl = byId("input-title");
    const title = titleEl ? titleEl.value : "";
    const windowTitle = state.activeId
      ? (state.dirty ? "• " : "") + (title || "Untitled") + " — Draconium Folio"
      : "Draconium Folio";
    if (document.title === windowTitle) return;
    document.title = windowTitle;
    const hostWindow = window.Draconium && window.Draconium.window;
    if (hostWindow && typeof hostWindow.setTitle === "function") hostWindow.setTitle(windowTitle);
  }

  function callAppSync(command, payloadObj) {
    const raw = payloadObj == null ? "{}" : JSON.stringify(payloadObj);
    if (!hasBridge()) {
      return { ok: false, error: "Host bridge unavailable" };
    }
    const res = window.Draconium.invoke(command, raw);
    if (!res || !res.ok) {
      return { ok: false, error: (res && res.error) || "Request failed" };
    }
    try {
      return { ok: true, data: JSON.parse(res.data) };
    } catch (_) {
      return { ok: false, error: "Invalid JSON response from host" };
    }
  }

  async function callAppAsync(command, payloadObj) {
    const raw = payloadObj == null ? "{}" : JSON.stringify(payloadObj);
    if (window.Draconium && typeof window.Draconium.invokeAsync === "function") {
      let res;
      try {
        res = await window.Draconium.invokeAsync(command, raw);
      } catch (error) {
        return { ok: false, error: String(error) };
      }
      if (!res || !res.ok) {
        return { ok: false, error: (res && res.error) || "Request failed" };
      }
      try {
        return { ok: true, data: JSON.parse(res.data) };
      } catch (_) {
        return { ok: false, error: "Invalid JSON response from host" };
      }
    }
    return callAppSync(command, payloadObj);
  }

  function showToast(message, details = "") {
    const el = byId("status-toast");
    if (!el) return;
    el.textContent = message;
    el.title = details || message;
    if (state.toastTimer) clearTimeout(state.toastTimer);
    state.toastTimer = setTimeout(function () {
      el.textContent = "";
      el.title = "";
    }, 3200);
  }

  function setSaveState(text, kind) {
    const el = byId("save-state");
    if (el) {
      el.textContent = text;
      el.className = "save-state" + (kind ? " " + kind : "");
      el.title = text;
    }
    syncWindowTitle();
  }

  function renderChromeIcons(theme) {
    const isDark = theme === "dark";
    const accent = isDark ? "#60a5fa" : "#1d4ed8";
    const ctrl = isDark ? "#cbd5e1" : "#526071";

    const brandWrap = byId("brand-mark-wrap");
    if (brandWrap) {
      brandWrap.innerHTML =
        '<svg width="18" height="18" viewBox="0 0 24 24">' +
        '<rect x="3" y="3" width="18" height="18" rx="4" fill="none" stroke="' + accent + '" stroke-width="1.8"/>' +
        '<line x1="9" y1="3" x2="9" y2="21" stroke="' + accent + '" stroke-width="1.6"/>' +
        '<line x1="12.5" y1="9" x2="17.5" y2="9" stroke="' + accent + '" stroke-width="1.6" stroke-linecap="round"/>' +
        '<line x1="12.5" y1="13" x2="16.5" y2="13" stroke="' + accent + '" stroke-width="1.6" stroke-linecap="round"/>' +
        "</svg>";
    }
    const minBtn = byId("btn-win-min");
    if (minBtn) {
      minBtn.innerHTML =
        '<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">' +
        '<rect x="2" y="5.5" width="8" height="1.2" rx="0.6" fill="' + ctrl + '"/>' +
        "</svg>";
    }
    const maxBtn = byId("btn-win-max");
    if (maxBtn) {
      maxBtn.innerHTML =
        '<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">' +
        '<rect x="2.2" y="2.2" width="7.6" height="7.6" rx="1.2" fill="none" stroke="' + ctrl + '" stroke-width="1.2"/>' +
        "</svg>";
    }
    const closeBtn = byId("btn-win-close");
    if (closeBtn) {
      closeBtn.innerHTML =
        '<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">' +
        '<path d="M2.6 2.6L9.4 9.4M9.4 2.6L2.6 9.4" stroke="' + ctrl + '" stroke-width="1.35" stroke-linecap="round"/>' +
        "</svg>";
    }
  }

  function applyTheme(theme) {
    const normalized = theme === "dark" ? "dark" : "light";
    state.theme = normalized;
    document.documentElement.setAttribute("data-theme", normalized);
    const btn = byId("btn-theme-toggle");
    if (btn) {
      btn.textContent = normalized === "dark" ? "Light theme" : "Dark theme";
    }
    renderChromeIcons(normalized);
    drawLabScope();
  }

  function setMode(mode) {
    const valid = mode === "write" || mode === "preview" ? mode : "split";
    state.mode = valid;
    const grid = byId("editor-grid");
    if (grid) {
      grid.className = "editor-grid mode-" + valid;
    }
    const buttons = document.querySelectorAll(".mode-btn[data-mode]");
    for (const button of buttons) {
      const selected = button.getAttribute("data-mode") === valid;
      button.classList.toggle("is-active", selected);
      button.setAttribute("aria-pressed", String(selected));
    }
    if (state.previewPending) updatePreview();
  }

  function updateCounts(text) {
    let chars = 0;
    let words = 0;
    let inWord = false;
    for (const char of text) {
      const isWord = !/\s/.test(char);
      if (isWord && !inWord) words++;
      inWord = isWord;
      chars++;
    }
    const mins = Math.max(1, Math.ceil(words / 220));

    const wordsEl = byId("stat-words");
    const charsEl = byId("stat-chars");
    const readEl = byId("stat-reading");
    if (wordsEl) wordsEl.textContent = words + (words === 1 ? " word" : " words");
    if (charsEl) charsEl.textContent = chars + (chars === 1 ? " character" : " characters");
    if (readEl) readEl.textContent = mins + " min read";
  }

  function updatePreview() {
    clearTimeout(state.previewTimer);
    state.previewTimer = null;
    const text = byId("editor-textarea").value;
    if (state.previewPending && state.mode !== "write") {
      renderMarkdown(text, byId("preview-prose"));
      state.previewPending = false;
    }
    updateCounts(text);
    if (state.labOpen) {
      byId("lab-dom-nodes").textContent = String(document.querySelectorAll("*").length);
    }
  }

  function isSafeHref(url) {
    const trimmed = String(url || "").trim();
    return /^https:\/\//i.test(trimmed) || /^mailto:/i.test(trimmed);
  }

  function appendInlineNodes(parent, text) {
    const tokenRe = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[([^\]]+)\]\(([^)]+)\))/g;
    let lastIndex = 0;
    let match;

    while ((match = tokenRe.exec(text)) !== null) {
      if (match.index > lastIndex) {
        parent.appendChild(document.createTextNode(text.slice(lastIndex, match.index)));
      }
      const token = match[0];
      if (token.startsWith("`")) {
        const code = document.createElement("code");
        code.textContent = token.slice(1, -1);
        parent.appendChild(code);
      } else if (token.startsWith("**")) {
        const strong = document.createElement("strong");
        strong.textContent = token.slice(2, -2);
        parent.appendChild(strong);
      } else if (token.startsWith("*")) {
        const em = document.createElement("em");
        em.textContent = token.slice(1, -1);
        parent.appendChild(em);
      } else if (match[2] != null && match[3] != null && isSafeHref(match[3])) {
        const href = match[3];
        const link = document.createElement("a");
        link.textContent = match[2];
        link.href = href;
        link.addEventListener("click", function (ev) {
          ev.preventDefault();
          if (window.Draconium && window.Draconium.window) {
            window.Draconium.window.openExternal(href);
          }
        });
        parent.appendChild(link);
      } else {
        parent.appendChild(document.createTextNode(token));
      }
      lastIndex = tokenRe.lastIndex;
    }

    if (lastIndex < text.length) {
      parent.appendChild(document.createTextNode(text.slice(lastIndex)));
    }
  }

  function startsTable(lines, index) {
    return lines[index].trim().startsWith("|") &&
      index + 1 < lines.length &&
      /^\|\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)*\|?$/.test(lines[index + 1].trim());
  }

  function renderMarkdown(markdownText, container) {
    container.innerHTML = "";

    const lines = String(markdownText || "").replace(/\r\n/g, "\n").split("\n");
    let i = 0;

    while (i < lines.length) {
      const line = lines[i];
      const trimmed = line.trim();

      if (!trimmed) {
        i++;
        continue;
      }

      if (trimmed.startsWith("```")) {
        const codeLines = [];
        i++;
        while (i < lines.length && !lines[i].trim().startsWith("```")) {
          codeLines.push(lines[i]);
          i++;
        }
        if (i < lines.length) i++;
        const pre = document.createElement("pre");
        const code = document.createElement("code");
        code.textContent = codeLines.join("\n");
        pre.appendChild(code);
        container.appendChild(pre);
        continue;
      }

      const headingMatch = /^(#{1,4})\s+(.+)$/.exec(trimmed);
      if (headingMatch) {
        const level = Math.min(4, headingMatch[1].length);
        const h = document.createElement("h" + level);
        appendInlineNodes(h, headingMatch[2]);
        container.appendChild(h);
        i++;
        continue;
      }

      if (/^(-{3,}|\*{3,})$/.test(trimmed)) {
        container.appendChild(document.createElement("hr"));
        i++;
        continue;
      }

      if (/^>\s+/.test(trimmed)) {
        const bq = document.createElement("blockquote");
        const quoteLines = [];
        while (i < lines.length && /^>\s+/.test(lines[i].trim())) {
          quoteLines.push(lines[i].trim().replace(/^>\s+/, ""));
          i++;
        }
        const p = document.createElement("p");
        appendInlineNodes(p, quoteLines.join(" "));
        bq.appendChild(p);
        container.appendChild(bq);
        continue;
      }

      if (startsTable(lines, i)) {
        const table = document.createElement("table");
        const thead = document.createElement("thead");
        const headRow = document.createElement("tr");
        const headCells = trimmed
          .replace(/^\||\|$/g, "")
          .split("|")
          .map(function (c) {
            return c.trim();
          });
        for (let c = 0; c < headCells.length; c++) {
          const th = document.createElement("th");
          appendInlineNodes(th, headCells[c]);
          headRow.appendChild(th);
        }
        thead.appendChild(headRow);
        table.appendChild(thead);

        i += 2;
        const tbody = document.createElement("tbody");
        while (i < lines.length && lines[i].trim().startsWith("|")) {
          const rowCells = lines[i]
            .trim()
            .replace(/^\||\|$/g, "")
            .split("|")
            .map(function (c) {
              return c.trim();
            });
          const tr = document.createElement("tr");
          for (let c = 0; c < rowCells.length; c++) {
            const td = document.createElement("td");
            appendInlineNodes(td, rowCells[c]);
            tr.appendChild(td);
          }
          tbody.appendChild(tr);
          i++;
        }
        table.appendChild(tbody);
        container.appendChild(table);
        continue;
      }

      if (/^[-*]\s+/.test(trimmed)) {
        const ul = document.createElement("ul");
        while (i < lines.length && /^[-*]\s+/.test(lines[i].trim())) {
          const rawItem = lines[i].trim().replace(/^[-*]\s+/, "");
          const li = document.createElement("li");
          const taskMatch = /^\[( |x|X)\]\s+(.*)$/.exec(rawItem);
          if (taskMatch) {
            li.className = "task-item";
            const box = document.createElement("input");
            box.type = "checkbox";
            box.checked = taskMatch[1].toLowerCase() === "x";
            box.disabled = true;
            const span = document.createElement("span");
            appendInlineNodes(span, taskMatch[2]);
            li.appendChild(box);
            li.appendChild(span);
          } else {
            appendInlineNodes(li, rawItem);
          }
          ul.appendChild(li);
          i++;
        }
        container.appendChild(ul);
        continue;
      }

      if (/^\d+\.\s+/.test(trimmed)) {
        const ol = document.createElement("ol");
        while (i < lines.length && /^\d+\.\s+/.test(lines[i].trim())) {
          const rawItem = lines[i].trim().replace(/^\d+\.\s+/, "");
          const li = document.createElement("li");
          appendInlineNodes(li, rawItem);
          ol.appendChild(li);
          i++;
        }
        container.appendChild(ol);
        continue;
      }

      const paraLines = [trimmed];
      i++;
      while (
        i < lines.length &&
        lines[i].trim() !== "" &&
        !/^(?:#{1,4}\s+|```|>\s+|[-*]\s+|\d+\.\s+|(?:-{3,}|\*{3,})$)/.test(lines[i].trim()) &&
        !startsTable(lines, i)
      ) {
        paraLines.push(lines[i].trim());
        i++;
      }
      const p = document.createElement("p");
      appendInlineNodes(p, paraLines.join(" "));
      container.appendChild(p);
    }
  }

  function renderNoteArticle() {
    const article = document.createElement("article");
    renderMarkdown(byId("editor-textarea").value, article);
    return article;
  }

  function buildNoteHtml() {
    const exported = document.implementation.createHTMLDocument(byId("input-title").value || "Untitled note");
    exported.documentElement.lang = document.documentElement.lang;
    const charset = exported.createElement("meta");
    charset.setAttribute("charset", "utf-8");
    exported.head.prepend(charset);
    exported.body.appendChild(renderNoteArticle());
    return "<!DOCTYPE html>\n" + exported.documentElement.outerHTML;
  }

  function updateTagSelection() {
    for (const button of byId("tag-filter-bar").children) {
      const selected = button.dataset.tag === state.activeTag;
      button.classList.toggle("is-selected", selected);
      button.setAttribute("aria-pressed", String(selected));
    }
  }

  function renderTagStrip() {
    const bar = byId("tag-filter-bar");
    const focusedTag = bar.contains(document.activeElement) ? document.activeElement.dataset.tag : null;
    const tags = Array.from(new Set(state.notes.flatMap(note => note.tags || []))).sort();
    if (state.activeTag && !tags.includes(state.activeTag)) state.activeTag = "";
    bar.replaceChildren();
    if (tags.length) {
      for (const tag of ["", ...tags]) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "tag-pill";
        button.dataset.tag = tag;
        button.textContent = tag || "All";
        button.addEventListener("click", function () {
          state.activeTag = state.activeTag === tag ? "" : tag;
          updateTagSelection();
          renderNoteList();
        });
        bar.appendChild(button);
      }
    }
    updateTagSelection();
    if (focusedTag !== null) {
      const matchingButton = Array.from(bar.children).find(button => button.dataset.tag === focusedTag);
      (matchingButton || bar.firstElementChild || byId("input-search")).focus();
    }
  }

  function updateNoteSelection() {
    for (const button of byId("note-list").querySelectorAll(".note-item")) {
      const selected = button.dataset.noteId === state.activeId;
      button.classList.toggle("is-active", selected);
      button.setAttribute("aria-selected", String(selected));
    }
  }

  function renderNoteList() {
    const listEl = byId("note-list");
    const focusedId = listEl.contains(document.activeElement) ? document.activeElement.dataset.noteId : null;
    const fragment = document.createDocumentFragment();
    const visible = state.notes.filter(function (note) {
      return (!state.searchMatches || state.searchMatches.has(note.id)) &&
        (!state.activeTag || (note.tags || []).includes(state.activeTag));
    });
    byId("note-count").textContent = String(visible.length);

    if (visible.length === 0) {
      const empty = document.createElement("div");
      empty.className = "note-empty";
      empty.textContent = state.notes.length === 0
        ? "No notes yet. Select New note to begin."
        : "No notes match the current filter.";
      fragment.appendChild(empty);
    }

    let replacement;
    for (const note of visible) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "note-item";
      button.setAttribute("role", "option");
      button.dataset.noteId = note.id;
      if (note.id === focusedId) replacement = button;

      const title = document.createElement("span");
      title.className = "note-item-title";
      title.textContent = note.title;

      const excerpt = document.createElement("span");
      excerpt.className = "note-item-excerpt";
      excerpt.textContent = note.excerpt || "Empty note";

      button.append(title, excerpt);
      button.addEventListener("click", function () { selectNote(note.id); });
      fragment.appendChild(button);
    }

    listEl.replaceChildren(fragment);
    updateNoteSelection();
    if (focusedId !== null) (replacement || byId("input-search")).focus();
  }

  function scheduleSearch() {
    const version = ++state.searchVersion;
    const query = state.filterQuery;
    clearTimeout(state.searchTimer);
    state.searchTimer = null;
    if (!query) {
      state.searchMatches = null;
      renderNoteList();
      return;
    }

    state.searchTimer = setTimeout(async function () {
      state.searchTimer = null;
      const res = await callAppAsync("list_notes", { query });
      if (version !== state.searchVersion) return;
      if (!res.ok) {
        showToast("Search failed: " + res.error);
        return;
      }
      state.searchMatches = new Set(res.data.notes.map(note => note.id));
      renderNoteList();
    }, 180);
  }

  function updateEditorAvailability() {
    for (const id of ["input-title", "input-tags", "editor-textarea"]) {
      const field = byId(id);
      field.disabled = !state.activeId;
      field.readOnly = Boolean(state.deletingId);
    }
    for (const id of ["btn-export-menu", "btn-delete-note"]) {
      byId(id).disabled = !state.activeId || Boolean(state.deletingId);
    }
  }

  function applyNoteToEditor(note) {
    if (state.saveTimer) {
      clearTimeout(state.saveTimer);
      state.saveTimer = null;
    }
    state.activeId = note ? note.id : null;
    state.dirty = false;

    const content = (note && note.content) || "";
    const fields = {
      "input-title": (note && note.title) || "",
      "input-tags": note && Array.isArray(note.tags) ? note.tags.join(", ") : "",
      "editor-textarea": content,
    };
    for (const [id, value] of Object.entries(fields)) {
      const field = byId(id);
      if (field) {
        field.value = value;
      }
    }

    updateEditorAvailability();
    state.previewPending = true;
    updatePreview();
    setSaveState(state.activeId ? "Saved" : "", "");
    updateNoteSelection();
  }

  function flushSaveSync() {
    if (state.saveTimer) {
      clearTimeout(state.saveTimer);
      state.saveTimer = null;
    }
    if (state.activeId === null) return false;
    if (!state.dirty) return true;
    const titleEl = byId("input-title");
    const tagsEl = byId("input-tags");
    const areaEl = byId("editor-textarea");
    const payload = {
      id: state.activeId,
      title: titleEl ? titleEl.value : "",
      tags: tagsEl ? tagsEl.value : "",
      content: areaEl ? areaEl.value : "",
    };
    const res = callAppSync("save_note", payload);
    if (!res.ok) {
      setSaveState("Save failed: " + res.error, "is-error");
      return false;
    }
    state.dirty = false;
    setSaveState("Saved", "");
    const saved = res.data;
    for (let i = 0; i < state.notes.length; i++) {
      if (state.notes[i].id === saved.id) {
        state.notes[i].title = saved.title;
        state.notes[i].tags = saved.tags;
        state.notes[i].excerpt = saved.excerpt;
        state.notes[i].updatedAt = saved.updatedAt;
        break;
      }
    }
    renderTagStrip();
    renderNoteList();
    if (state.filterQuery) scheduleSearch();
    return true;
  }

  function scheduleAutosave() {
    if (!state.activeId || state.deletingId) return;
    if (!state.dirty) {
      state.dirty = true;
      setSaveState("Unsaved changes", "is-dirty");
    } else {
      syncWindowTitle();
    }
    if (state.saveTimer) clearTimeout(state.saveTimer);
    state.saveTimer = setTimeout(function () {
      flushSaveSync();
    }, 450);
  }

  async function selectNote(id) {
    if (state.deletingId) return false;
    const version = ++state.selectionVersion;
    if (id === state.activeId) return true;
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return false;
    }
    const res = await callAppAsync("load_note", { id: id });
    if (version !== state.selectionVersion) return false;
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return false;
    }
    // Edits can arrive while the selected note loads.
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return false;
    }
    applyNoteToEditor(res.data);
    return true;
  }

  function applyWorkspaceList(data) {
    state.notes = Array.isArray(data.notes) ? data.notes : [];
    state.maxNoteBytes = data.maxNoteBytes;
    const unreadableCount = data.unreadableCount || 0;
    if (unreadableCount && unreadableCount !== state.unreadableCount) {
      showToast(unreadableCount + (unreadableCount === 1 ? " note could" : " notes could") + " not be read");
    }
    state.unreadableCount = unreadableCount;
    renderTagStrip();
    scheduleSearch();
    if (state.filterQuery) renderNoteList();
  }

  function firstAvailableNote(preferredId) {
    if (preferredId && state.notes.some(note => note.id === preferredId)) return preferredId;
    return state.notes.length ? state.notes[0].id : null;
  }

  async function refreshWorkspace(preferredId, selection = state.selectionVersion) {
    const version = ++state.workspaceVersion;
    const res = await callAppAsync("list_notes", {});
    if (version !== state.workspaceVersion) return false;
    if (!res.ok) {
      showToast("Could not refresh notes: " + res.error);
      return false;
    }
    applyWorkspaceList(res.data);
    if (selection !== state.selectionVersion || state.deletingId) return false;

    const targetId = firstAvailableNote(preferredId);
    if (targetId) return selectNote(targetId);
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return false;
    }
    applyNoteToEditor(null);
    return true;
  }

  async function createNewNote() {
    if (state.deletingId) return;
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return;
    }
    const selection = state.selectionVersion;
    const res = await callAppAsync("create_note", {
      title: "Untitled note",
      tags: "",
      content: "# Untitled note\n\n",
    });
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    const selected = await refreshWorkspace(res.data.id, selection);
    const titleEl = byId("input-title");
    if (selected && state.activeId === res.data.id) {
      titleEl.focus();
      titleEl.select();
    }
    showToast("Created note");
  }

  async function deleteActiveNote() {
    if (!state.activeId || state.deletingId) return;
    const idToDelete = state.activeId;
    const title = byId("input-title").value || "Untitled note";
    if (!window.confirm('Move "' + title + '" to the workspace trash?')) return;
    if (!flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return;
    }

    state.selectionVersion++;
    state.deletingId = idToDelete;
    updateEditorAvailability();
    setSaveState("Moving to trash…", "is-dirty");
    const res = await callAppAsync("delete_note", { id: idToDelete });
    state.deletingId = null;
    updateEditorAvailability();
    if (!res.ok) {
      setSaveState("Could not move note to trash: " + res.error, "is-error");
      return;
    }
    if (state.activeId === idToDelete) applyNoteToEditor(null);
    state.notes = state.notes.filter(note => note.id !== idToDelete);
    renderTagStrip();
    renderNoteList();
    showToast("Moved note to workspace trash", res.data.trashPath);
    await refreshWorkspace(null);
  }

  async function importFileTokens(files, extraFiles = 0) {
    if (!Array.isArray(files) || files.length === 0) return;
    if (!window.Draconium || !window.Draconium.files) return;

    const selection = state.selectionVersion;
    let importedCount = 0;
    let lastImportedId = null;
    const failures = [];
    for (const entry of files) {
      const fileName = entry.name || "imported.md";
      try {
        if (state.maxNoteBytes && entry.size > state.maxNoteBytes) {
          throw new Error("File exceeds " + state.maxNoteBytes / (1024 * 1024) + " MiB limit");
        }
        const readRes = await window.Draconium.files.readTextAsync(entry.token);
        if (!readRes || !readRes.ok) {
          throw new Error((readRes && readRes.error) || "Could not read file");
        }
        const impRes = await callAppAsync("import_note", { fileName, content: readRes.data });
        if (!impRes.ok) throw new Error(impRes.error);
        lastImportedId = impRes.data.id;
        importedCount++;
      } catch (error) {
        failures.push(fileName + " — " + error.message);
      }
    }
    if (lastImportedId) await refreshWorkspace(lastImportedId, selection);
    const imported = "Imported " + importedCount + (importedCount === 1 ? " file" : " files");
    const summary = [imported];
    if (failures.length) summary.push(failures.length + " failed");
    if (extraFiles > 0) summary.push(extraFiles + " skipped by the file drop limit");
    const message = summary.join(", ");
    showToast(failures.length ? message + ". " + failures[0] : message, [message, ...failures].join("\n"));
  }

  async function openNativeFilePicker() {
    if (!window.Draconium || !window.Draconium.dialog) {
      showToast("File picker unavailable");
      return;
    }
    try {
      const picked = await window.Draconium.dialog.openFileAsync({
        extensions: ["md", "markdown", "txt"],
        multiple: true,
      });
      if (!picked || !picked.ok) {
        const error = picked && picked.error;
        if (error !== "cancelled") showToast("File picker failed: " + (error || "No response"));
        return;
      }
      await importFileTokens(picked.data);
    } catch (error) {
      showToast("File picker failed: " + error);
    }
  }

  function hideExportMenu() {
    const pop = byId("export-popover");
    if (pop && typeof pop.hidePopover === "function") {
      pop.hidePopover();
    }
    byId("btn-export-menu").focus();
  }

  function drawLabScope() {
    const canvas = byId("lab-canvas");
    if (!canvas || typeof canvas.getContext !== "function") return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const w = canvas.width;
    const h = canvas.height;
    const isDark = state.theme === "dark";

    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = isDark ? "#111821" : "#f6f7f9";
    ctx.fillRect(0, 0, w, h);

    // 16.7 ms (60 fps) reference guide line
    const targetY = h - Math.round((16.7 / 36) * (h - 14)) - 6;
    ctx.strokeStyle = isDark ? "rgba(45, 212, 191, 0.45)" : "rgba(15, 118, 110, 0.4)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, targetY);
    ctx.lineTo(w, targetY);
    ctx.stroke();

    const samples = state.frameSamples;
    const count = samples.length;
    const barGap = 4;
    const barW = Math.max(4, Math.floor((w - 16) / Math.max(1, count)) - barGap);

    for (let i = 0; i < count; i++) {
      const ms = Math.max(2, Math.min(36, samples[i]));
      const barH = Math.max(4, Math.round((ms / 36) * (h - 14)));
      const x = 8 + i * (barW + barGap);
      const y = h - 6 - barH;
      ctx.fillStyle =
        ms <= 20
          ? isDark
            ? "#60a5fa"
            : "#1d4ed8"
          : isDark
            ? "#f59e0b"
            : "#b45309";
      ctx.fillRect(x, y, barW, barH);
    }
  }

  function toggleLabDrawer(forceOpen) {
    const drawer = byId("lab-drawer");
    const btn = byId("btn-toggle-lab");
    if (!drawer) return;
    const open = forceOpen === undefined ? !state.labOpen : Boolean(forceOpen);
    state.labOpen = open;
    if (open) {
      drawer.classList.remove("is-hidden");
      byId("lab-dom-nodes").textContent = String(document.querySelectorAll("*").length);
      drawLabScope();
    } else {
      drawer.classList.add("is-hidden");
    }
    if (btn) btn.setAttribute("aria-expanded", open ? "true" : "false");
  }

  function runRenderBenchmark(targetFrames) {
    if (state.benchPromise) return state.benchPromise;
    if (typeof window.requestAnimationFrame !== "function") {
      const error = "Frame timing is unavailable";
      showToast(error);
      return Promise.resolve({ error });
    }

    const totalFrames = Math.max(4, Math.min(180, Math.round(Number(targetFrames) || 60)));
    toggleLabDrawer(true);
    state.frameSamples = [];
    const button = byId("btn-run-bench");
    button.textContent = "Measuring...";
    button.disabled = true;

    state.benchPromise = new Promise(function (resolve) {
      const deltas = [];
      let lastFrame;
      let frameRequest;
      const timeout = setTimeout(function () {
        const error = "Render check timed out waiting for frames";
        showToast(error);
        finish({ error });
      }, Math.max(10000, totalFrames * 500));

      function finish(result) {
        clearTimeout(timeout);
        window.cancelAnimationFrame(frameRequest);
        button.textContent = "Run render check";
        button.disabled = false;
        state.benchPromise = null;
        resolve(result);
      }

      function sample(timestamp) {
        if (lastFrame !== undefined && timestamp > lastFrame) {
          const elapsed = timestamp - lastFrame;
          deltas.push(elapsed);
          state.frameSamples.push(elapsed);
          if (state.frameSamples.length > 48) state.frameSamples.shift();
          drawLabScope();
        }
        lastFrame = timestamp;
        if (deltas.length < totalFrames) {
          frameRequest = window.requestAnimationFrame(sample);
          return;
        }

        const avgMs = deltas.reduce((sum, delta) => sum + delta, 0) / deltas.length;
        const sorted = deltas.slice().sort((a, b) => a - b);
        const p95Ms = sorted[Math.ceil(sorted.length * 0.95) - 1];
        const fps = 1000 / avgMs;
        const domNodes = document.querySelectorAll("*").length;

        byId("lab-fps").textContent = fps.toFixed(1) + " fps";
        byId("lab-avg-ms").textContent = avgMs.toFixed(1) + " ms";
        byId("lab-p95-ms").textContent = p95Ms.toFixed(1) + " ms";
        byId("lab-dom-nodes").textContent = String(domNodes);

        const report = {
          frames: totalFrames,
          fps: Number(fps.toFixed(2)),
          avgFrameMs: Number(avgMs.toFixed(2)),
          p95FrameMs: Number(p95Ms.toFixed(2)),
          domNodes,
        };
        callAppAsync("record_benchmark", report);
        showToast("Render check complete");
        finish(report);
      }

      frameRequest = window.requestAnimationFrame(sample);
    });
    return state.benchPromise;
  }

  window.__folioRunBenchmark = runRenderBenchmark;
  window.__draconiumBeforeClose = function () {
    return !state.activeId || flushSaveSync();
  };

  function wireWindowChrome() {
    const shell = byId("window-shell");
    const hostWindow = window.Draconium && window.Draconium.window;
    if (!hostWindow) return;

    function syncMaximizedClass() {
      const res = hostWindow.isMaximized();
      if (shell && res && res.ok) {
        if (res.data === "true") shell.classList.add("is-maximized");
        else shell.classList.remove("is-maximized");
      }
    }

    function syncFullscreenClass(enabled) {
      if (shell) shell.classList.toggle("is-fullscreen", enabled === true || enabled === "true");
    }

    if (typeof hostWindow.isFullscreen === "function") {
      const res = hostWindow.isFullscreen();
      if (res && res.ok) syncFullscreenClass(res.data);
    }
    if (typeof window.Draconium.on === "function") {
      window.Draconium.on("fullscreen-changed", syncFullscreenClass);
    }

    const minBtn = byId("btn-win-min");
    if (minBtn) minBtn.addEventListener("click", function () { hostWindow.minimize(); });

    const maxBtn = byId("btn-win-max");
    if (maxBtn) {
      maxBtn.addEventListener("click", function () {
        hostWindow.toggleMaximize();
        setTimeout(syncMaximizedClass, 60);
      });
    }

    const closeBtn = byId("btn-win-close");
    if (closeBtn) closeBtn.addEventListener("click", function () { hostWindow.close(); });

    const titlebar = byId("titlebar");
    if (titlebar) {
      titlebar.addEventListener("dblclick", function (ev) {
        if (ev.target && ev.target.closest && ev.target.closest("button, [data-dn-no-drag]")) return;
        hostWindow.toggleMaximize();
        setTimeout(syncMaximizedClass, 60);
      });
    }

    const handles = document.querySelectorAll(".resize-edge[data-resize]");
    for (let i = 0; i < handles.length; i++) {
      handles[i].addEventListener("mousedown", function (ev) {
        if (ev.button !== 0) return;
        ev.preventDefault();
        hostWindow.dragResize(this.getAttribute("data-resize"));
      });
    }

    syncMaximizedClass();
  }

  function bindEvents() {
    const modeButtons = document.querySelectorAll(".mode-btn[data-mode]");
    for (let i = 0; i < modeButtons.length; i++) {
      modeButtons[i].addEventListener("click", function () {
        setMode(this.getAttribute("data-mode"));
      });
    }

    const searchInput = byId("input-search");
    if (searchInput) {
      searchInput.addEventListener("input", function () {
        state.filterQuery = this.value.trim();
        scheduleSearch();
      });
    }

    if (byId("btn-new-note")) byId("btn-new-note").addEventListener("click", createNewNote);
    if (byId("btn-import-file")) byId("btn-import-file").addEventListener("click", openNativeFilePicker);
    if (byId("btn-delete-note")) byId("btn-delete-note").addEventListener("click", deleteActiveNote);

    if (byId("btn-toggle-lab")) {
      byId("btn-toggle-lab").addEventListener("click", function () { toggleLabDrawer(); });
    }
    if (byId("btn-close-lab")) {
      byId("btn-close-lab").addEventListener("click", function () { toggleLabDrawer(false); });
    }
    if (byId("btn-run-bench")) {
      byId("btn-run-bench").addEventListener("click", function () { runRenderBenchmark(60); });
    }

    if (byId("btn-theme-toggle")) {
      byId("btn-theme-toggle").addEventListener("click", function () {
        applyTheme(state.theme === "dark" ? "light" : "dark");
      });
    }

    const titleEl = byId("input-title");
    const tagsEl = byId("input-tags");
    const areaEl = byId("editor-textarea");

    if (titleEl) titleEl.addEventListener("input", scheduleAutosave);
    if (tagsEl) tagsEl.addEventListener("input", scheduleAutosave);
    if (areaEl) {
      areaEl.addEventListener("input", function () {
        state.previewPending = true;
        clearTimeout(state.previewTimer);
        state.previewTimer = setTimeout(updatePreview, 140);
        scheduleAutosave();
      });
    }

    const exportMenu = byId("export-popover");
    const exportTrigger = byId("btn-export-menu");
    let focusWasInMenu = false;
    exportMenu.addEventListener("beforetoggle", function (event) {
      focusWasInMenu = event.newState === "closed" && exportMenu.contains(document.activeElement);
    });
    exportMenu.addEventListener("toggle", function (event) {
      const open = event.newState === "open";
      exportTrigger.setAttribute("aria-expanded", String(open));
      if (open) {
        byId("act-copy-md").focus();
      } else if (focusWasInMenu && (document.activeElement === document.body || exportMenu.contains(document.activeElement))) {
        exportTrigger.focus();
      }
    });

    if (byId("act-copy-md")) {
      byId("act-copy-md").addEventListener("click", function () {
        hideExportMenu();
        const text = areaEl ? areaEl.value : "";
        if (window.Draconium && window.Draconium.clipboard) {
          const res = window.Draconium.clipboard.writeText(text);
          showToast(res && res.ok ? "Copied Markdown" : "Clipboard copy failed");
        }
      });
    }

    if (byId("act-copy-html")) {
      byId("act-copy-html").addEventListener("click", function () {
        hideExportMenu();
        if (window.Draconium && window.Draconium.clipboard) {
          const res = window.Draconium.clipboard.writeHtml(renderNoteArticle().innerHTML);
          showToast(res && res.ok ? "Copied formatted HTML" : "Clipboard copy failed");
        }
      });
    }

    if (byId("act-save-md")) {
      byId("act-save-md").addEventListener("click", async function () {
        hideExportMenu();
        if (!window.Draconium || !window.Draconium.dialog) return;
        const name = (state.activeId || "note") + ".md";
        const res = await window.Draconium.dialog.saveTextFileAsync({
          name: name,
          extensions: ["md", "txt"],
          text: areaEl ? areaEl.value : "",
        });
        if (res && res.ok) showToast("Saved Markdown copy");
      });
    }

    if (byId("act-export-html")) {
      byId("act-export-html").addEventListener("click", async function () {
        hideExportMenu();
        if (!state.activeId || !window.Draconium || !window.Draconium.dialog) return;
        try {
          const res = await window.Draconium.dialog.saveTextFileAsync({
            name: state.activeId + ".html",
            extensions: ["html"],
            text: buildNoteHtml(),
          });
          if (res && res.ok) {
            showToast("Exported HTML");
          } else if (!res || res.error !== "cancelled") {
            showToast("HTML export failed: " + ((res && res.error) || "No response"));
          }
        } catch (error) {
          showToast("HTML export failed: " + error);
        }
      });
    }

    if (byId("act-export-pdf")) {
      byId("act-export-pdf").addEventListener("click", async function () {
        hideExportMenu();
        if (!window.Draconium || !window.Draconium.window) return;
        const res = await window.Draconium.window.exportPdfAsync((state.activeId || "note") + ".pdf");
        if (res && res.ok) showToast("Exported PDF");
      });
    }

    if (byId("act-capture-png")) {
      byId("act-capture-png").addEventListener("click", async function () {
        hideExportMenu();
        if (!window.Draconium || !window.Draconium.window) return;
        const res = await window.Draconium.window.capturePngAsync((state.activeId || "note") + ".png");
        if (res && res.ok) showToast("Saved PNG snapshot");
      });
    }

    document.addEventListener("keydown", function (ev) {
      if (!ev.ctrlKey) return;
      const key = (ev.key || "").toLowerCase();
      if (key === "n") {
        ev.preventDefault();
        createNewNote();
      } else if (key === "s") {
        ev.preventDefault();
        if (flushSaveSync()) showToast("Saved");
      } else if (key === "1") {
        ev.preventDefault();
        setMode("write");
      } else if (key === "2") {
        ev.preventDefault();
        setMode("split");
      } else if (key === "3") {
        ev.preventDefault();
        setMode("preview");
      } else if (ev.shiftKey && key === "l") {
        ev.preventDefault();
        toggleLabDrawer();
      }
    });

    const dropOverlay = byId("drop-overlay");
    document.addEventListener("dn:file-drop-hover", function () {
      if (dropOverlay) dropOverlay.classList.remove("is-hidden");
    });
    document.addEventListener("dn:file-drop-cancel", function () {
      if (dropOverlay) dropOverlay.classList.add("is-hidden");
    });
    document.addEventListener("dn:file-drop", function (ev) {
      if (dropOverlay) dropOverlay.classList.add("is-hidden");
      if (ev.detail) importFileTokens(ev.detail.files, ev.detail.extraFiles);
    });

    if (window.Draconium && typeof window.Draconium.on === "function") {
      window.Draconium.on("theme-changed", function (payload) {
        const theme = typeof payload === "string" ? payload : payload && payload.theme;
        if (theme === "light" || theme === "dark") applyTheme(theme);
      });
    }
  }

  function hydrateInitialWorkspaceSync(preferredId) {
    const res = callAppSync("list_notes", {});
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    applyWorkspaceList(res.data);
    const targetId = firstAvailableNote(preferredId);

    if (targetId) {
      const loaded = callAppSync("load_note", { id: targetId });
      if (loaded.ok) {
        applyNoteToEditor(loaded.data);
      } else {
        setSaveState(loaded.error, "is-error");
      }
    }
  }

  function bootstrap() {
    wireWindowChrome();
    applyNoteToEditor(null);
    bindEvents();

    if (window.Draconium && window.Draconium.window) {
      const themeRes = window.Draconium.window.getTheme();
      if (themeRes && themeRes.ok && (themeRes.data === "dark" || themeRes.data === "light")) {
        applyTheme(themeRes.data);
      }
    }

    hydrateInitialWorkspaceSync("welcome");
    drawLabScope();
    window.__folioReady = true;
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", bootstrap);
  } else {
    bootstrap();
  }
})();

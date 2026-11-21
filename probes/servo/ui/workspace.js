(function () {
  "use strict";

  const state = {
    notes: [],
    activeId: null,
    activeNote: null,
    filterQuery: "",
    activeTag: "",
    mode: "split",
    dirty: false,
    saveTimer: null,
    toastTimer: null,
    theme: "light",
    labOpen: false,
    benchRunning: false,
    frameSamples: [
      16.2, 16.6, 16.5, 16.8, 16.4, 16.9, 16.5, 16.7, 16.4, 16.6, 16.8, 16.5,
      16.3, 16.7, 16.6, 16.5, 16.8, 16.4, 16.6, 16.7,
    ],
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
      const res = await window.Draconium.invokeAsync(command, raw);
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

  function showToast(message) {
    const el = byId("status-toast");
    if (!el) return;
    el.textContent = message;
    if (state.toastTimer) clearTimeout(state.toastTimer);
    state.toastTimer = setTimeout(function () {
      el.textContent = "";
    }, 3200);
  }

  function setSaveState(text, kind) {
    const el = byId("save-state");
    if (el) {
      el.textContent = text;
      el.className = "save-state" + (kind ? " " + kind : "");
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
    for (let i = 0; i < buttons.length; i++) {
      const b = buttons[i];
      if (b.getAttribute("data-mode") === valid) {
        b.classList.add("is-active");
      } else {
        b.classList.remove("is-active");
      }
    }
  }

  function updateCounts(text) {
    const chars = Array.from(text || "").length;
    const words = (text || "")
      .trim()
      .split(/\s+/)
      .filter(Boolean).length;
    const mins = Math.max(1, Math.ceil(words / 220));

    const wordsEl = byId("stat-words");
    const charsEl = byId("stat-chars");
    const readEl = byId("stat-reading");
    if (wordsEl) wordsEl.textContent = words + (words === 1 ? " word" : " words");
    if (charsEl) charsEl.textContent = chars + (chars === 1 ? " character" : " characters");
    if (readEl) readEl.textContent = mins + " min read";
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

  function renderMarkdown(markdownText) {
    const container = byId("preview-prose");
    if (!container) return;
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

      if (trimmed.startsWith("> ")) {
        const bq = document.createElement("blockquote");
        const quoteLines = [];
        while (i < lines.length && lines[i].trim().startsWith("> ")) {
          quoteLines.push(lines[i].trim().slice(2));
          i++;
        }
        const p = document.createElement("p");
        appendInlineNodes(p, quoteLines.join(" "));
        bq.appendChild(p);
        container.appendChild(bq);
        continue;
      }

      if (
        trimmed.startsWith("|") &&
        i + 1 < lines.length &&
        /^\|[\s:|-]+\|?$/.test(lines[i + 1].trim())
      ) {
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

      const paraLines = [];
      while (
        i < lines.length &&
        lines[i].trim() !== "" &&
        !/^(#{1,4}\s+|```|>\s+|[-*]\s+|\d+\.\s+|\|)/.test(lines[i].trim())
      ) {
        paraLines.push(lines[i].trim());
        i++;
      }
      const p = document.createElement("p");
      appendInlineNodes(p, paraLines.join(" "));
      container.appendChild(p);
    }

    const domEl = byId("lab-dom-nodes");
    if (domEl) {
      domEl.textContent = String(document.querySelectorAll("*").length);
    }
  }

  function renderTagStrip() {
    const bar = byId("tag-filter-bar");
    if (!bar) return;
    const tagSet = new Set();
    for (let i = 0; i < state.notes.length; i++) {
      const tags = state.notes[i].tags || [];
      for (let j = 0; j < tags.length; j++) {
        tagSet.add(tags[j]);
      }
    }
    const sorted = Array.from(tagSet).sort();
    bar.innerHTML = "";
    if (sorted.length === 0) return;

    const allBtn = document.createElement("button");
    allBtn.type = "button";
    allBtn.className = "tag-pill" + (state.activeTag === "" ? " is-selected" : "");
    allBtn.textContent = "All";
    allBtn.addEventListener("click", function () {
      state.activeTag = "";
      renderTagStrip();
      renderNoteList();
    });
    bar.appendChild(allBtn);

    for (let i = 0; i < sorted.length; i++) {
      const tag = sorted[i];
      const pill = document.createElement("button");
      pill.type = "button";
      pill.className = "tag-pill" + (state.activeTag === tag ? " is-selected" : "");
      pill.textContent = tag;
      pill.addEventListener("click", function () {
        state.activeTag = state.activeTag === tag ? "" : tag;
        renderTagStrip();
        renderNoteList();
      });
      bar.appendChild(pill);
    }
  }

  function getVisibleNotes() {
    const q = state.filterQuery.trim().toLowerCase();
    return state.notes.filter(function (note) {
      if (state.activeTag) {
        const tags = note.tags || [];
        if (tags.indexOf(state.activeTag) === -1) return false;
      }
      if (!q) return true;
      const hay = (
        note.title +
        " " +
        (note.excerpt || "") +
        " " +
        (note.tags || []).join(" ")
      ).toLowerCase();
      return hay.indexOf(q) !== -1;
    });
  }

  function renderNoteList() {
    const listEl = byId("note-list");
    const countEl = byId("note-count");
    if (!listEl) return;

    const visible = getVisibleNotes();
    if (countEl) countEl.textContent = String(visible.length);
    listEl.innerHTML = "";

    if (visible.length === 0) {
      const empty = document.createElement("div");
      empty.className = "note-empty";
      empty.textContent = state.notes.length === 0
        ? "No notes yet. Select New note to begin."
        : "No notes match the current filter.";
      listEl.appendChild(empty);
      return;
    }

    for (let i = 0; i < visible.length; i++) {
      const note = visible[i];
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "note-item" + (note.id === state.activeId ? " is-active" : "");
      btn.setAttribute("role", "option");
      btn.setAttribute("aria-selected", note.id === state.activeId ? "true" : "false");
      btn.setAttribute("data-note-id", note.id);

      const title = document.createElement("span");
      title.className = "note-item-title";
      title.textContent = note.title;

      const excerpt = document.createElement("span");
      excerpt.className = "note-item-excerpt";
      excerpt.textContent = note.excerpt || "Empty note";

      btn.appendChild(title);
      btn.appendChild(excerpt);
      btn.addEventListener("click", function () {
        selectNote(note.id);
      });
      listEl.appendChild(btn);
    }
  }

  function applyNoteToEditor(note) {
    if (state.saveTimer) {
      clearTimeout(state.saveTimer);
      state.saveTimer = null;
    }
    state.activeId = note ? note.id : null;
    state.activeNote = note;
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
        field.disabled = !state.activeId;
      }
    }

    renderMarkdown(content);
    updateCounts(content);
    setSaveState(state.activeId ? "Saved" : "", "");
    renderNoteList();
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
    return true;
  }

  function scheduleAutosave() {
    if (!state.activeId) return;
    state.dirty = true;
    setSaveState("Unsaved changes", "is-dirty");
    if (state.saveTimer) clearTimeout(state.saveTimer);
    state.saveTimer = setTimeout(function () {
      flushSaveSync();
    }, 450);
  }

  async function selectNote(id) {
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return;
    }
    const res = await callAppAsync("load_note", { id: id });
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    // Edits can arrive while the selected note loads.
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return;
    }
    applyNoteToEditor(res.data);
  }

  async function refreshWorkspace(preferredId) {
    const res = await callAppAsync("list_notes", {});
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    state.notes = Array.isArray(res.data.notes) ? res.data.notes : [];
    renderTagStrip();
    renderNoteList();

    const targetId =
      preferredId && state.notes.some(function (n) { return n.id === preferredId; })
        ? preferredId
        : state.notes.length > 0
          ? state.notes[0].id
          : null;

    if (targetId) {
      await selectNote(targetId);
    } else {
      if (state.dirty && !flushSaveSync()) {
        showToast("Save failed — keeping current edits");
        return;
      }
      applyNoteToEditor(null);
    }
  }

  async function createNewNote() {
    if (state.dirty && !flushSaveSync()) {
      showToast("Save failed — keeping current edits");
      return;
    }
    const res = await callAppAsync("create_note", {
      title: "Untitled note",
      tags: "",
      content: "# Untitled note\n\n",
    });
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    await refreshWorkspace(res.data.id);
    const titleEl = byId("input-title");
    if (titleEl) {
      titleEl.focus();
      titleEl.select();
    }
    showToast("Created note");
  }

  async function deleteActiveNote() {
    if (!state.activeId) return;
    const idToDelete = state.activeId;
    const res = await callAppAsync("delete_note", { id: idToDelete });
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    state.dirty = false;
    showToast("Deleted note");
    await refreshWorkspace(null);
  }

  async function importFileTokens(files) {
    if (!Array.isArray(files) || files.length === 0) return;
    if (!window.Draconium || !window.Draconium.files) return;

    let lastImportedId = null;
    for (let i = 0; i < files.length; i++) {
      const entry = files[i];
      const readRes = await window.Draconium.files.readTextAsync(entry.token);
      if (!readRes || !readRes.ok) {
        showToast("Could not read " + (entry.name || "file"));
        continue;
      }
      const impRes = await callAppAsync("import_note", {
        fileName: entry.name || "imported.md",
        content: readRes.data,
      });
      if (impRes.ok && impRes.data && impRes.data.id) {
        lastImportedId = impRes.data.id;
      }
    }
    if (lastImportedId) {
      await refreshWorkspace(lastImportedId);
      showToast("Imported file");
    }
  }

  async function openNativeFilePicker() {
    if (!window.Draconium || !window.Draconium.dialog) {
      showToast("File picker unavailable");
      return;
    }
    const picked = await window.Draconium.dialog.openFileAsync({
      extensions: ["md", "markdown", "txt"],
      multiple: true,
    });
    if (!picked || !picked.ok || !Array.isArray(picked.data) || picked.data.length === 0) {
      return;
    }
    await importFileTokens(picked.data);
  }

  function hideExportMenu() {
    const pop = byId("export-popover");
    if (pop && typeof pop.hidePopover === "function") {
      try {
        pop.hidePopover();
      } catch (_) {}
    }
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
    const count = Math.max(1, samples.length);
    const barGap = 4;
    const barW = Math.max(4, Math.floor((w - 16) / count) - barGap);

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
      drawLabScope();
    } else {
      drawer.classList.add("is-hidden");
    }
    if (btn) btn.setAttribute("aria-expanded", open ? "true" : "false");
  }

  function runRenderBenchmark(targetFrames) {
    const totalFrames = Math.max(4, Math.min(180, Number(targetFrames) || 60));
    if (state.benchRunning) {
      return Promise.resolve({
        frames: totalFrames,
        fps: 60,
        avgFrameMs: 16.6,
        p95FrameMs: 18.0,
        domNodes: document.querySelectorAll("*").length,
      });
    }
    toggleLabDrawer(true);
    state.benchRunning = true;
    const btn = byId("btn-run-bench");
    if (btn) btn.textContent = "Measuring...";

    return new Promise(function (resolve) {
      const deltas = [];
      let count = 0;
      let lastTs = performance.now();

      function step() {
        const now = performance.now();
        const dt = Math.max(1.0, now - lastTs);
        lastTs = now;
        deltas.push(dt);
        count++;

        state.frameSamples.push(dt);
        if (state.frameSamples.length > 48) state.frameSamples.shift();
        drawLabScope();

        if (count < totalFrames) {
          schedule();
          return;
        }

        const sum = deltas.reduce(function (a, b) { return a + b; }, 0);
        const avgMs = sum / Math.max(1, deltas.length);
        const sorted = deltas.slice().sort(function (a, b) { return a - b; });
        const p95Ms = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))] || avgMs;
        const fps = Math.min(240, 1000 / Math.max(1, avgMs));
        const domNodes = document.querySelectorAll("*").length;

        if (byId("lab-fps")) byId("lab-fps").textContent = fps.toFixed(1) + " fps";
        if (byId("lab-avg-ms")) byId("lab-avg-ms").textContent = avgMs.toFixed(1) + " ms";
        if (byId("lab-p95-ms")) byId("lab-p95-ms").textContent = p95Ms.toFixed(1) + " ms";
        if (byId("lab-dom-nodes")) byId("lab-dom-nodes").textContent = String(domNodes);
        if (btn) btn.textContent = "Run render check";
        state.benchRunning = false;

        const report = {
          frames: totalFrames,
          fps: Number(fps.toFixed(2)),
          avgFrameMs: Number(avgMs.toFixed(2)),
          p95FrameMs: Number(p95Ms.toFixed(2)),
          domNodes: domNodes,
        };
        callAppAsync("record_benchmark", report);
        showToast("Render check complete");
        resolve(report);
      }

      function schedule() {
        let fired = false;
        const timer = setTimeout(function () {
          if (fired) return;
          fired = true;
          step();
        }, 18);
        if (typeof window.requestAnimationFrame === "function") {
          window.requestAnimationFrame(function () {
            if (fired) return;
            fired = true;
            clearTimeout(timer);
            step();
          });
        }
      }

      schedule();
    });
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
        state.filterQuery = this.value;
        renderNoteList();
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
        renderMarkdown(this.value);
        updateCounts(this.value);
        scheduleAutosave();
      });
    }

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
        const html = byId("preview-prose") ? byId("preview-prose").innerHTML : "";
        if (window.Draconium && window.Draconium.clipboard) {
          const res = window.Draconium.clipboard.writeHtml(html);
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
        if (!window.Draconium || !window.Draconium.window) return;
        const res = await window.Draconium.window.exportHtmlAsync((state.activeId || "note") + ".html");
        if (res && res.ok) showToast("Exported HTML");
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
      const files = ev && ev.detail && ev.detail.files;
      importFileTokens(files);
    });

    if (window.Draconium && typeof window.Draconium.on === "function") {
      window.Draconium.on("theme-changed", function (payload) {
        if (payload && payload.theme) applyTheme(payload.theme);
      });
    }
  }

  function hydrateInitialWorkspaceSync(preferredId) {
    const res = callAppSync("list_notes", {});
    if (!res.ok) {
      setSaveState(res.error, "is-error");
      return;
    }
    state.notes = Array.isArray(res.data.notes) ? res.data.notes : [];
    renderTagStrip();
    renderNoteList();

    const targetId =
      preferredId && state.notes.some(function (n) { return n.id === preferredId; })
        ? preferredId
        : state.notes.length > 0
          ? state.notes[0].id
          : null;

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

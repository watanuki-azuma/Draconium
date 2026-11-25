"use strict";

/* Draco Shot: hotkey -> region select -> minimal annotations -> copy / save.
   All annotation coordinates are stored in full-capture-image pixels; the
   visible "view" is either the whole capture (select phase) or the selected
   region (edit phase). */

const D = window.Draconium;

const COLORS = ["#ff3b30", "#ff9500", "#ffd60a", "#34c759", "#0a84ff", "#ffffff"];
const WIDTHS = [2, 4, 8];
const TEXT_SIZES = [16, 24, 36];
const STEP_RADII = [11, 14, 18];

const els = {};
let settings = { hotkey: "Ctrl+Shift+S", saveDir: "", filenamePattern: "", autoCopy: true };

const session = {
  active: false,
  busy: false,
  phase: "select", // "select" | "edit"
  img: null,
  width: 0,
  height: 0,
  region: null, // {x, y, w, h} in image pixels
  annotations: [],
  redoStack: [],
  tool: "arrow",
  color: COLORS[0],
  widthIdx: 1,
  draft: null, // annotation being dragged, or {crop:true, ...}
};

document.addEventListener("DOMContentLoaded", init);

function init() {
  for (const id of [
    "idle-panel", "stage", "canvas-wrap", "base-canvas", "anno-canvas",
    "select-rect", "text-input", "toolbar", "stage-hint", "toast",
    "hotkey-label", "btn-capture", "settings-form", "set-hotkey",
    "set-save-dir", "btn-pick-dir", "set-pattern", "set-autocopy",
    "tool-group", "color-group", "width-group", "btn-undo", "btn-redo",
    "btn-copy", "btn-save", "btn-cancel",
  ]) {
    els[id] = document.getElementById(id);
  }
  if (!D) {
    els["hotkey-label"].textContent = "(Draconium ホスト外では動作しません)";
    return;
  }
  buildColorSwatches();
  wireEvents();
  loadSettings();
}

function wireEvents() {
  els["btn-capture"].addEventListener("click", startCapture);
  els["settings-form"].addEventListener("submit", (ev) => {
    ev.preventDefault();
    saveSettings();
  });
  els["btn-pick-dir"].addEventListener("click", pickSaveDir);
  els["tool-group"].addEventListener("click", (ev) => {
    const btn = ev.target.closest("button[data-tool]");
    if (btn) selectTool(btn.dataset.tool);
  });
  els["width-group"].addEventListener("click", (ev) => {
    const btn = ev.target.closest("button[data-width]");
    if (!btn) return;
    session.widthIdx = Number(btn.dataset.width);
    for (const b of els["width-group"].querySelectorAll("button")) {
      b.classList.toggle("active", b === btn);
    }
  });
  els["btn-undo"].addEventListener("click", undo);
  els["btn-redo"].addEventListener("click", redo);
  els["btn-copy"].addEventListener("click", doCopy);
  els["btn-save"].addEventListener("click", doSave);
  els["btn-cancel"].addEventListener("click", () => endSession());
  const canvas = els["anno-canvas"];
  canvas.addEventListener("pointerdown", onPointerDown);
  canvas.addEventListener("pointermove", onPointerMove);
  canvas.addEventListener("pointerup", onPointerUp);
  canvas.addEventListener("pointercancel", onPointerUp);
  document.addEventListener("keydown", onKeyDown);
  window.addEventListener("resize", () => {
    if (session.active) layoutStage();
  });
  if (typeof D.on === "function") {
    D.on("global-hotkey", () => {
      if (session.active) {
        endSession();
      } else {
        startCapture();
      }
    });
  }
}

/* ---------------- settings ---------------- */

async function invokeAsync(command, payload) {
  const raw = payload === undefined ? "" : JSON.stringify(payload);
  const res = await D.invokeAsync(command, raw);
  if (!res || !res.ok) {
    throw new Error((res && res.error) || "ホストコマンドが失敗しました");
  }
  if (res.data == null || res.data === "") return null;
  try {
    return JSON.parse(res.data);
  } catch {
    return res.data;
  }
}

async function loadSettings() {
  try {
    const loaded = await invokeAsync("get_settings");
    if (loaded) settings = loaded;
  } catch {
    /* keep defaults */
  }
  els["hotkey-label"].textContent = settings.hotkey || "Ctrl+Shift+S";
  els["set-hotkey"].value = settings.hotkey || "";
  els["set-save-dir"].value = settings.saveDir || "";
  els["set-pattern"].value = settings.filenamePattern || "";
  els["set-autocopy"].checked = !!settings.autoCopy;
}

async function saveSettings() {
  try {
    const saved = await invokeAsync("save_settings", {
      hotkey: els["set-hotkey"].value,
      saveDir: els["set-save-dir"].value,
      filenamePattern: els["set-pattern"].value,
      autoCopy: els["set-autocopy"].checked,
    });
    if (saved) settings = saved;
    els["hotkey-label"].textContent = settings.hotkey || "";
    toast("設定を保存しました（ホットキーは次回起動時に反映）");
  } catch (err) {
    toast("設定を保存できませんでした: " + err.message);
  }
}

async function pickSaveDir() {
  try {
    const res = await D.dialog.selectFolderAsync({ title: "保存先フォルダ" });
    if (!res || !res.ok || res.data == null) return;
    const path = Array.isArray(res.data) ? res.data[0] : res.data;
    if (typeof path === "string" && path) {
      els["set-save-dir"].value = path;
    }
  } catch {
    /* dialog unavailable: keep manual entry */
  }
}

/* ---------------- session lifecycle ---------------- */

async function startCapture() {
  if (session.active || session.busy || !D) return;
  session.busy = true;
  try {
    // Hide first so this window never appears inside its own screenshot.
    D.window.hide();
    const cap = await invokeAsync("capture_screen");
    const img = new Image();
    await new Promise((resolve, reject) => {
      img.onload = resolve;
      img.onerror = () => reject(new Error("キャプチャ画像を読み込めませんでした"));
      img.src = "data:image/png;base64," + cap.pngBase64;
    });
    session.img = img;
    session.width = cap.width;
    session.height = cap.height;
    session.region = null;
    session.annotations = [];
    session.redoStack = [];
    session.draft = null;
    session.phase = "select";
    session.active = true;
    els["idle-panel"].hidden = true;
    els["stage"].hidden = false;
    els["toolbar"].hidden = true;
    els["stage-hint"].textContent = "ドラッグで範囲を選択　Esc: キャンセル";
    layoutStage();
    D.window.setFullscreen(true);
    D.window.show();
    D.window.focus();
  } catch (err) {
    session.active = false;
    els["stage"].hidden = true;
    els["idle-panel"].hidden = false;
    D.window.show();
    toast("キャプチャに失敗しました: " + err.message);
  } finally {
    session.busy = false;
  }
}

function endSession() {
  session.active = false;
  session.draft = null;
  closeTextInput(false);
  els["stage"].hidden = true;
  els["idle-panel"].hidden = false;
  if (D) {
    D.window.setFullscreen(false);
    D.window.hide();
  }
}

function enterEditPhase(region) {
  session.region = region;
  session.phase = "edit";
  els["select-rect"].hidden = true;
  els["toolbar"].hidden = false;
  els["stage-hint"].textContent = "注釈を入れて コピー / 保存　Esc: キャンセル";
  selectTool(session.tool);
  layoutStage();
  updateUndoButtons();
}

/* ---------------- layout & coordinate mapping ---------------- */

function viewRect() {
  if (session.phase === "edit" && session.region) return session.region;
  return { x: 0, y: 0, w: session.width, h: session.height };
}

function layoutStage() {
  if (!session.img) return;
  const view = viewRect();
  const topReserve = session.phase === "edit" ? 60 : 0;
  const availW = window.innerWidth;
  const availH = window.innerHeight - topReserve;
  const scale = Math.min(availW / view.w, availH / view.h);
  const wrapW = Math.max(1, Math.round(view.w * scale));
  const wrapH = Math.max(1, Math.round(view.h * scale));
  const wrap = els["canvas-wrap"];
  wrap.style.left = Math.round((availW - wrapW) / 2) + "px";
  wrap.style.top = Math.round(topReserve + (availH - wrapH) / 2) + "px";
  wrap.style.width = wrapW + "px";
  wrap.style.height = wrapH + "px";
  for (const canvas of [els["base-canvas"], els["anno-canvas"]]) {
    canvas.width = view.w;
    canvas.height = view.h;
  }
  drawBase();
  drawAnnotations();
}

function displayScale() {
  const view = viewRect();
  const rect = els["canvas-wrap"].getBoundingClientRect();
  return rect.width > 0 ? view.w / rect.width : 1;
}

function pointerToImage(ev) {
  const view = viewRect();
  const rect = els["canvas-wrap"].getBoundingClientRect();
  const x = view.x + (ev.clientX - rect.left) * (view.w / rect.width);
  const y = view.y + (ev.clientY - rect.top) * (view.h / rect.height);
  return {
    x: Math.min(Math.max(x, 0), session.width),
    y: Math.min(Math.max(y, 0), session.height),
  };
}

function imageToWrapPx(x, y) {
  const view = viewRect();
  const rect = els["canvas-wrap"].getBoundingClientRect();
  return {
    x: (x - view.x) * (rect.width / view.w),
    y: (y - view.y) * (rect.height / view.h),
  };
}

/* ---------------- drawing ---------------- */

function drawBase() {
  const view = viewRect();
  const ctx = els["base-canvas"].getContext("2d");
  ctx.clearRect(0, 0, view.w, view.h);
  ctx.drawImage(session.img, view.x, view.y, view.w, view.h, 0, 0, view.w, view.h);
}

function drawAnnotations() {
  const view = viewRect();
  const ctx = els["anno-canvas"].getContext("2d");
  ctx.clearRect(0, 0, view.w, view.h);
  ctx.save();
  ctx.translate(-view.x, -view.y);
  for (const a of session.annotations) drawAnnotation(ctx, a);
  if (session.draft && typeof session.draft.type === "string") {
    drawAnnotation(ctx, session.draft);
  }
  ctx.restore();
  if (session.draft && session.draft.crop) {
    drawCropOutline(ctx, normalizeRect(session.draft));
  }
}

function drawCropOutline(ctx, r) {
  const view = viewRect();
  ctx.save();
  ctx.strokeStyle = "#ffd60a";
  ctx.lineWidth = Math.max(1, displayScale() * 1.5);
  ctx.setLineDash([6, 4]);
  ctx.strokeRect(r.x - view.x, r.y - view.y, r.w, r.h);
  ctx.restore();
}

function drawAnnotation(ctx, a) {
  ctx.save();
  ctx.strokeStyle = a.color;
  ctx.fillStyle = a.color;
  ctx.lineWidth = a.width || 4;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  switch (a.type) {
    case "line":
      ctx.beginPath();
      ctx.moveTo(a.x1, a.y1);
      ctx.lineTo(a.x2, a.y2);
      ctx.stroke();
      break;
    case "arrow": {
      ctx.beginPath();
      ctx.moveTo(a.x1, a.y1);
      ctx.lineTo(a.x2, a.y2);
      ctx.stroke();
      const angle = Math.atan2(a.y2 - a.y1, a.x2 - a.x1);
      const head = Math.max(10, (a.width || 4) * 3.5);
      for (const sign of [-1, 1]) {
        const t = angle + sign * (Math.PI - Math.PI / 7);
        ctx.beginPath();
        ctx.moveTo(a.x2, a.y2);
        ctx.lineTo(a.x2 + head * Math.cos(t), a.y2 + head * Math.sin(t));
        ctx.stroke();
      }
      break;
    }
    case "rect":
      ctx.strokeRect(a.x, a.y, a.w, a.h);
      break;
    case "ellipse":
      ctx.beginPath();
      ctx.ellipse(
        a.x + a.w / 2, a.y + a.h / 2,
        Math.abs(a.w / 2), Math.abs(a.h / 2),
        0, 0, Math.PI * 2
      );
      ctx.stroke();
      break;
    case "text": {
      ctx.font = "700 " + a.size + "px 'Segoe UI', sans-serif";
      ctx.textBaseline = "top";
      const lines = String(a.text).split("\n");
      lines.forEach((line, i) => {
        ctx.fillText(line, a.x, a.y + i * a.size * 1.25);
      });
      break;
    }
    case "step": {
      ctx.beginPath();
      ctx.arc(a.x, a.y, a.r, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = "#ffffff";
      ctx.font = "700 " + Math.round(a.r * 1.15) + "px 'Segoe UI', sans-serif";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(String(a.n), a.x, a.y + 1);
      break;
    }
    case "highlight": {
      if (a.points.length < 2) break;
      ctx.globalAlpha = 0.4;
      ctx.lineWidth = (a.width || 4) * 3;
      ctx.beginPath();
      ctx.moveTo(a.points[0].x, a.points[0].y);
      for (const p of a.points.slice(1)) ctx.lineTo(p.x, p.y);
      ctx.stroke();
      break;
    }
    case "mosaic": {
      const cols = Math.max(1, Math.round(a.w / a.block));
      const rows = Math.max(1, Math.round(a.h / a.block));
      const tmp = drawAnnotation.tmpCanvas ||
        (drawAnnotation.tmpCanvas = document.createElement("canvas"));
      tmp.width = cols;
      tmp.height = rows;
      const tctx = tmp.getContext("2d");
      tctx.drawImage(session.img, a.x, a.y, a.w, a.h, 0, 0, cols, rows);
      ctx.imageSmoothingEnabled = false;
      ctx.drawImage(tmp, 0, 0, cols, rows, a.x, a.y, a.w, a.h);
      break;
    }
  }
  ctx.restore();
}

/* ---------------- pointer handling ---------------- */

function normalizeRect(d) {
  const x = Math.min(d.x1, d.x2);
  const y = Math.min(d.y1, d.y2);
  return { x, y, w: Math.abs(d.x2 - d.x1), h: Math.abs(d.y2 - d.y1) };
}

function onPointerDown(ev) {
  if (!session.active || ev.button !== 0) return;
  els["anno-canvas"].setPointerCapture(ev.pointerId);
  const p = pointerToImage(ev);
  if (session.phase === "select") {
    session.draft = { crop: false, select: true, x1: p.x, y1: p.y, x2: p.x, y2: p.y };
    updateSelectRect();
    return;
  }
  const tool = session.tool;
  const base = { color: session.color, width: WIDTHS[session.widthIdx] };
  if (tool === "text") {
    openTextInput(p);
  } else if (tool === "step") {
    const n = session.annotations.filter((a) => a.type === "step").length + 1;
    commit({ type: "step", x: p.x, y: p.y, n, r: STEP_RADII[session.widthIdx], ...base });
  } else if (tool === "crop") {
    session.draft = { crop: true, x1: p.x, y1: p.y, x2: p.x, y2: p.y };
  } else if (tool === "highlight") {
    session.draft = { type: "highlight", points: [p], ...base };
  } else if (tool === "mosaic" || tool === "rect" || tool === "ellipse") {
    session.draft = { type: tool, x1: p.x, y1: p.y, x2: p.x, y2: p.y, ...base };
  } else {
    // arrow / line
    session.draft = { type: tool, x1: p.x, y1: p.y, x2: p.x, y2: p.y, ...base };
  }
}

function onPointerMove(ev) {
  if (!session.draft) return;
  const p = pointerToImage(ev);
  const d = session.draft;
  if (session.phase === "select") {
    d.x2 = p.x;
    d.y2 = p.y;
    updateSelectRect();
    return;
  }
  if (d.type === "highlight") {
    d.points.push(p);
  } else {
    d.x2 = p.x;
    d.y2 = p.y;
  }
  drawAnnotations();
}

function onPointerUp(ev) {
  const d = session.draft;
  if (!d) return;
  session.draft = null;
  if (session.phase === "select") {
    els["select-rect"].hidden = true;
    const r = normalizeRect(d);
    if (r.w >= 8 && r.h >= 8) {
      enterEditPhase(clampRectToImage(r));
    } else {
      drawAnnotations();
    }
    return;
  }
  if (d.crop) {
    const r = clampRectToImage(normalizeRect(d));
    if (r.w >= 8 && r.h >= 8) {
      session.region = r;
      layoutStage();
    } else {
      drawAnnotations();
    }
    return;
  }
  if (d.type === "highlight") {
    if (d.points.length >= 2) commit(d);
    else drawAnnotations();
    return;
  }
  if (d.type === "mosaic" || d.type === "rect" || d.type === "ellipse") {
    const r = normalizeRect(d);
    if (r.w >= 4 && r.h >= 4) {
      const a = { type: d.type, x: r.x, y: r.y, w: r.w, h: r.h, color: d.color, width: d.width };
      if (d.type === "mosaic") a.block = Math.max(6, WIDTHS[session.widthIdx] * 3);
      commit(a);
    } else {
      drawAnnotations();
    }
    return;
  }
  // arrow / line
  const dist = Math.hypot(d.x2 - d.x1, d.y2 - d.y1);
  if (dist >= 3) commit(d);
  else drawAnnotations();
}

function clampRectToImage(r) {
  const x = Math.min(Math.max(r.x, 0), session.width);
  const y = Math.min(Math.max(r.y, 0), session.height);
  const w = Math.min(r.w, session.width - x);
  const h = Math.min(r.h, session.height - y);
  return { x, y, w, h };
}

function updateSelectRect() {
  const d = session.draft;
  if (!d) return;
  const r = normalizeRect(d);
  const tl = imageToWrapPx(r.x, r.y);
  const br = imageToWrapPx(r.x + r.w, r.y + r.h);
  const el = els["select-rect"];
  el.hidden = false;
  el.style.left = tl.x + "px";
  el.style.top = tl.y + "px";
  el.style.width = Math.max(0, br.x - tl.x) + "px";
  el.style.height = Math.max(0, br.y - tl.y) + "px";
}

function commit(annotation) {
  session.annotations.push(annotation);
  session.redoStack = [];
  drawAnnotations();
  updateUndoButtons();
}

function undo() {
  const a = session.annotations.pop();
  if (a) session.redoStack.push(a);
  drawAnnotations();
  updateUndoButtons();
}

function redo() {
  const a = session.redoStack.pop();
  if (a) session.annotations.push(a);
  drawAnnotations();
  updateUndoButtons();
}

function updateUndoButtons() {
  els["btn-undo"].disabled = session.annotations.length === 0;
  els["btn-redo"].disabled = session.redoStack.length === 0;
}

function selectTool(tool) {
  session.tool = tool;
  for (const b of els["tool-group"].querySelectorAll("button[data-tool]")) {
    b.classList.toggle("active", b.dataset.tool === tool);
  }
}

function buildColorSwatches() {
  for (const color of COLORS) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "swatch" + (color === session.color ? " active" : "");
    btn.style.background = color;
    btn.title = color;
    btn.addEventListener("click", () => {
      session.color = color;
      for (const b of els["color-group"].querySelectorAll(".swatch")) {
        b.classList.toggle("active", b === btn);
      }
    });
    els["color-group"].appendChild(btn);
  }
}

/* ---------------- text input ---------------- */

let textPoint = null;

function openTextInput(p) {
  textPoint = p;
  const input = els["text-input"];
  const wrapPx = imageToWrapPx(p.x, p.y);
  const scale = 1 / displayScale();
  input.style.left = wrapPx.x + "px";
  input.style.top = wrapPx.y + "px";
  input.style.fontSize = Math.max(10, TEXT_SIZES[session.widthIdx] * scale) + "px";
  input.style.color = session.color;
  input.value = "";
  input.hidden = false;
  input.focus();
}

function closeTextInput(commitText) {
  const input = els["text-input"];
  if (input.hidden) return;
  const text = input.value.trim();
  input.hidden = true;
  if (commitText && text && textPoint) {
    commit({
      type: "text",
      x: textPoint.x,
      y: textPoint.y,
      text,
      color: session.color,
      width: WIDTHS[session.widthIdx],
      size: TEXT_SIZES[session.widthIdx],
    });
  }
  textPoint = null;
}

/* ---------------- keyboard ---------------- */

function onKeyDown(ev) {
  if (!els["text-input"].hidden) {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeTextInput(false);
    } else if (ev.key === "Enter" && (ev.ctrlKey || ev.metaKey)) {
      ev.preventDefault();
      closeTextInput(true);
    }
    return;
  }
  if (!session.active) return;
  if (ev.key === "Escape") {
    ev.preventDefault();
    endSession();
  } else if (session.phase === "edit" && ev.ctrlKey && !ev.shiftKey && ev.key.toLowerCase() === "z") {
    ev.preventDefault();
    undo();
  } else if (
    session.phase === "edit" && ev.ctrlKey &&
    (ev.key.toLowerCase() === "y" || (ev.shiftKey && ev.key.toLowerCase() === "z"))
  ) {
    ev.preventDefault();
    redo();
  }
}

/* ---------------- export ---------------- */

function renderExport() {
  const region = session.region;
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(region.w);
  canvas.height = Math.round(region.h);
  const ctx = canvas.getContext("2d");
  ctx.drawImage(
    session.img,
    region.x, region.y, region.w, region.h,
    0, 0, canvas.width, canvas.height
  );
  ctx.save();
  ctx.translate(-region.x, -region.y);
  for (const a of session.annotations) drawAnnotation(ctx, a);
  ctx.restore();
  return canvas;
}

function bytesToBase64(bytes) {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

async function copyCanvasToClipboard(canvas) {
  const ctx = canvas.getContext("2d");
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const rgbaBase64 = bytesToBase64(new Uint8Array(data.data.buffer));
  const res = await D.clipboard.writeImageAsync({
    width: canvas.width,
    height: canvas.height,
    rgbaBase64,
  });
  if (!res || !res.ok) {
    throw new Error((res && res.error) || "クリップボードへコピーできませんでした");
  }
}

async function doCopy() {
  if (!session.active || session.phase !== "edit") return;
  try {
    await copyCanvasToClipboard(renderExport());
    endSession();
  } catch (err) {
    toast("コピーに失敗しました: " + err.message);
  }
}

async function doSave() {
  if (!session.active || session.phase !== "edit") return;
  try {
    const canvas = renderExport();
    const dataUrl = canvas.toDataURL("image/png");
    const pngBase64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
    const now = new Date();
    const saved = await invokeAsync("save_shot", {
      pngBase64,
      localTime: {
        year: now.getFullYear(),
        month: now.getMonth() + 1,
        day: now.getDate(),
        hour: now.getHours(),
        minute: now.getMinutes(),
        second: now.getSeconds(),
      },
    });
    if (settings.autoCopy) {
      await copyCanvasToClipboard(canvas);
    }
    endSession();
    if (D.notify && typeof D.notify.notify === "function") {
      D.notify.notify({
        title: "Draco Shot",
        body: "保存しました: " + (saved && saved.fileName ? saved.fileName : "shot.png"),
      });
    }
  } catch (err) {
    toast("保存に失敗しました: " + err.message);
  }
}

/* ---------------- toast ---------------- */

let toastTimer = null;

function toast(message) {
  const el = els["toast"];
  el.textContent = message;
  el.hidden = false;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    el.hidden = true;
  }, 3200);
}

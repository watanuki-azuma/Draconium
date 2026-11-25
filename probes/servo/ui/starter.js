(function () {
  "use strict";

  const inputEl = document.getElementById("starter-input");
  const replyEl = document.getElementById("starter-reply");
  const hostMetaEl = document.getElementById("host-meta");
  const themeMetaEl = document.getElementById("theme-meta");

  function formatReply(res) {
    if (!res || !res.ok) {
      return "Error: " + ((res && res.error) || "No response");
    }
    try {
      const data = JSON.parse(res.data);
      return (
        "Response #" +
        data.count +
        " (" +
        data.chars +
        " chars) on " +
        data.engine +
        ": " +
        data.message
      );
    } catch (_) {
      return String(res.data);
    }
  }

  function applyTheme(theme) {
    const mode = theme === "dark" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", mode);
    if (themeMetaEl) themeMetaEl.textContent = "Theme: " + mode;
  }

  function sendSync() {
    if (!window.Draconium) return;
    const res = window.Draconium.invoke("echo", inputEl ? inputEl.value : "");
    if (replyEl) replyEl.textContent = formatReply(res);
  }

  async function sendAsync() {
    if (!window.Draconium) return;
    const res = await window.Draconium.invokeAsync("echo", inputEl ? inputEl.value : "");
    if (replyEl) replyEl.textContent = formatReply(res);
  }

  function bootstrap() {
    const syncBtn = document.getElementById("btn-echo-sync");
    const asyncBtn = document.getElementById("btn-echo-async");
    if (syncBtn) syncBtn.addEventListener("click", sendSync);
    if (asyncBtn) asyncBtn.addEventListener("click", sendAsync);

    if (window.Draconium) {
      const themeRes = window.Draconium.window.getTheme();
      if (themeRes && themeRes.ok) applyTheme(themeRes.data);
      window.Draconium.on("theme-changed", function (payload) {
        const theme = typeof payload === "string" ? payload : payload && payload.theme;
        if (theme === "light" || theme === "dark") applyTheme(theme);
      });

      const info = window.Draconium.invoke("host_info", "");
      if (info && info.ok && hostMetaEl) {
        try {
          const parsed = JSON.parse(info.data);
          hostMetaEl.textContent = parsed.host + " on " + parsed.engine + " (" + parsed.protocol + ")";
        } catch (_) {}
      }
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", bootstrap);
  } else {
    bootstrap();
  }
})();

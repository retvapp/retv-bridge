// The ReTV Bridge window: polls the app's state and renders it. Text only
// goes in through textContent; the window never sees the session token.
"use strict";

const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);

// Language and strings: read once (get_strings); the poll carries state only.
let ui = null;
let state = null;
let tab = "code";
let seenSeq = -1;
let shownLogLen = -1;

function s(key, vars) {
  let text = (ui && ui.strings[key]) || key;
  for (const [name, value] of Object.entries(vars || {})) {
    text = text.split("{" + name + "}").join(value);
  }
  return text;
}

function setTab(next) {
  tab = next;
  $("view-code").hidden = tab !== "code";
  $("view-log").hidden = tab !== "log";
  for (const b of document.querySelectorAll(".tab")) {
    b.classList.toggle("active", b.dataset.tab === tab);
  }
  if (tab === "log") {
    shownLogLen = -1;
    renderLog();
  }
}

function render() {
  document.documentElement.lang = ui.lang;
  $("title").textContent = s("title");
  $("tab-code").textContent = s("tab.code");
  $("tab-log").textContent = s("tab.log");

  const notices = $("notices");
  notices.replaceChildren();
  for (const key of state.notices) {
    const p = document.createElement("p");
    p.className = "notice";
    p.textContent = s(key);
    notices.append(p);
    if (key === "lan.denied") {
      const hint = document.createElement("p");
      hint.className = "notice hint";
      hint.textContent = s("lan.hint");
      notices.append(hint);
    }
  }

  // With the port taken there is no bridge: only the notice matters.
  const portBusy = state.notices.includes("port.busy");
  const adb = portBusy ? "none" : state.adb.kind;
  $("adb-consent").hidden = adb !== "needsConsent";
  $("adb-consent-text").textContent = s("adb.consent", { size: String(state.adb.sizeMb) });
  $("adb-terms").textContent = s("adb.terms");
  $("adb-accept").textContent = s("adb.accept");

  $("adb-downloading").hidden = adb !== "downloading";
  $("adb-downloading-text").textContent = s("adb.downloading");
  const total = state.adb.total || 1;
  $("adb-progress").value = Math.min(1, state.adb.done / total);

  $("adb-failed").hidden = adb !== "failed";
  $("adb-failed-text").textContent = s("adb.failed");
  $("adb-retry").textContent = s("adb.retry");
  $("adb-pick").textContent = s("adb.pick");

  // The installed tools did not start: retry the start, never the download.
  $("adb-start-failed").hidden = adb !== "startFailed";
  $("adb-start-failed-text").textContent = s("adb.startFailed");
  $("adb-start-retry").textContent = s("adb.retry");

  $("pair").hidden = adb !== "ready";
  $("code-label").textContent = s("code.label");
  $("code").textContent = state.code || "";
  $("code").hidden = !state.code;
  $("code-none").textContent = s("code.none");
  $("code-none").hidden = !!state.code;
  $("tv").textContent = state.tv;
  $("tv").classList.toggle("on", state.tvConnected);
  $("open").textContent = s("open");

  $("close-note").textContent = s(ui.windows ? "closeNote.win" : "closeNote");
  $("log-empty").textContent = s("log.empty");
}

async function renderLog() {
  if (!state || state.logLen === shownLogLen) return;
  shownLogLen = state.logLen;
  const lines = await invoke("show_log");
  const pre = $("log");
  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
  pre.textContent = lines.join("\n");
  $("log-empty").hidden = lines.length > 0;
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

async function tick() {
  try {
    if (!ui) ui = await invoke("get_strings");
    state = await invoke("get_state");
  } catch (e) {
    return;
  }
  render();
  if (state.viewSeq !== seenSeq) {
    seenSeq = state.viewSeq;
    setTab(state.view);
  } else if (tab === "log") {
    renderLog();
  }
}

for (const b of document.querySelectorAll(".tab")) {
  b.addEventListener("click", () => setTab(b.dataset.tab));
}
$("adb-accept").addEventListener("click", () => invoke("accept_adb_download"));
$("adb-retry").addEventListener("click", () => invoke("retry_adb"));
$("adb-start-retry").addEventListener("click", () => invoke("retry_adb"));
$("adb-pick").addEventListener("click", () => invoke("pick_adb_zip"));
$("open").addEventListener("click", () => invoke("open_site"));
$("adb-terms").addEventListener("click", (e) => {
  e.preventDefault();
  invoke("open_terms");
});

setTab("code");
tick();
setInterval(tick, 700);

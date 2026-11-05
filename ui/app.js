// The ReTV Bridge window: polls the app's state and renders it. Text only goes in through
// textContent (and DOM nodes built here); the window never sees the session token. Every view is
// rebuilt only when what it shows changes, so a poll never steals a click or the keyboard focus.
"use strict";

const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);

const POLL_MS = 700;
/// While the wizard waits for the TV (restart, Developer Mode, TV off), it reads the TV this often.
const RECHECK_MS = 5000;
const TV_DOWN = new Set(["samsung.err.tv", "samsung.err.devOff", "samsung.err.sdb", "samsung.err.devClosed"]);
const REMOTE_ERR = new Set(["samsung.err.remoteDenied", "samsung.err.remoteTimeout", "samsung.err.remoteDown"]);

// Language and strings: read once (get_strings); the poll carries state only.
let ui = null;
let state = null;
let sam = null;
let tab = "code";
let seenSeq = -1;
let shownLogLen = -1;
let logText = "";

// --- Strings ---

function s(key, vars) {
  let text = (ui && ui.strings[key]) || key;
  for (const [name, value] of Object.entries(vars || {})) {
    text = text.split("{" + name + "}").join(String(value));
  }
  return text;
}

function has(key) {
  return !!(ui && ui.strings[key]);
}

/// `key.one / .few / .many / .other` by the language's plural rules.
function plural(key, n, vars) {
  let form = "other";
  try {
    form = new Intl.PluralRules(ui.lang).select(n);
  } catch (e) {
    form = n === 1 ? "one" : "other";
  }
  const k = has(key + "." + form) ? key + "." + form : key + ".other";
  return s(k, Object.assign({ n: String(n) }, vars || {}));
}

// --- DOM helpers ---

function append(el, kids) {
  for (const k of kids.flat(Infinity)) {
    if (k === null || k === undefined || k === false) continue;
    el.append(typeof k === "string" || typeof k === "number" ? document.createTextNode(String(k)) : k);
  }
}

function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "key") el.dataset.key = v;
    else if (k === "value") el.value = v;
    else if (k === "checked") el.checked = !!v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  append(el, kids);
  return el;
}

const SVGNS = "http://www.w3.org/2000/svg";
const ICONS = {
  ok: [["circle", { cx: 10, cy: 10, r: 8 }], ["path", { d: "m6.5 10 2.5 2.5 4.5-5" }]],
  warn: [["path", { d: "M10 2.5 18 17H2z", "stroke-linejoin": "round" }], ["path", { d: "M10 8v4M10 14.5v.2" }]],
  err: [["circle", { cx: 10, cy: 10, r: 8 }], ["path", { d: "M10 6v5M10 13.8v.2" }]],
  check: [["path", { d: "m5 10.5 3.5 3.5 6.5-7.5" }]],
  skip: [["circle", { cx: 10, cy: 10, r: 8 }], ["path", { d: "M6.5 10h7" }]],
  home: [["path", { d: "M3.5 9.5 10 4l6.5 5.5", "stroke-linejoin": "round" }], ["path", { d: "M5.5 8.5V16h9V8.5" }]],
  power: [["path", { d: "M10 3v6.5" }], ["path", { d: "M6.2 5.6a6 6 0 1 0 7.6 0" }]],
  x: [["path", { d: "M5.5 5.5l9 9M14.5 5.5l-9 9" }]],
  user: [["circle", { cx: 10, cy: 7, r: 3.2 }], ["path", { d: "M3.8 16.5c.8-3 3.3-4.6 6.2-4.6s5.4 1.6 6.2 4.6" }]],
  tv: [["rect", { x: 2.5, y: 4, width: 15, height: 10, rx: 1.8 }], ["path", { d: "M7 17h6" }]],
};

function icon(name, size) {
  const svg = document.createElementNS(SVGNS, "svg");
  const px = String(size || 20);
  for (const [k, v] of Object.entries({
    width: px,
    height: px,
    viewBox: "0 0 20 20",
    fill: "none",
    stroke: "currentColor",
    "stroke-width": "1.8",
    "stroke-linecap": "round",
    "aria-hidden": "true",
  })) {
    svg.setAttribute(k, v);
  }
  for (const [tag, attrs] of ICONS[name]) {
    const el = document.createElementNS(SVGNS, tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
    svg.append(el);
  }
  return svg;
}

const KEY_ICONS = { "⌂": ["home", "Home"], "⏻": ["power", "Power"] };

/// A string with the window's small markup: [[TV term]] (a word to look for on the TV), ((key))
/// (a remote key) and `value` (an address, in mono).
function rich(text) {
  const out = [];
  const re = /\[\[(.+?)\]\]|\(\((.+?)\)\)|`([^`]+)`/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1] !== undefined) out.push(h("span", { class: "tvterm" }, m[1]));
    else if (m[2] !== undefined) {
      const ic = KEY_ICONS[m[2]];
      out.push(ic ? h("kbd", { "aria-label": ic[1], role: "img" }, icon(ic[0], 14)) : h("kbd", {}, m[2]));
    } else out.push(h("span", { class: "mono sel" }, m[3]));
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

const R = (key, vars) => rich(s(key, vars));

function btn(label, onClick, kind, opts) {
  const o = opts || {};
  return h(
    "button",
    {
      type: "button",
      class: "btn " + (kind || "sec") + (o.cls ? " " + o.cls : ""),
      key: o.key || label,
      onclick: onClick,
      disabled: !!o.disabled,
    },
    label,
  );
}

function banner(kind, title, text, acts, extra) {
  return h(
    "div",
    { class: "ban " + kind },
    icon(kind === "ok" ? "ok" : kind === "err" ? "err" : "warn"),
    title ? h("b", {}, title) : null,
    text ? h("p", {}, text) : null,
    extra || null,
    acts && acts.length ? h("div", { class: "acts" }, acts) : null,
  );
}

function errBanner(key, acts, extra) {
  const title = has(key + ".title") ? s(key + ".title") : null;
  const tech = has(key + ".tech") ? h("p", { class: "tech" }, s(key + ".tech")) : null;
  // `{ip}`: this computer's address on the TV's network (samsung.err.devClosed).
  return banner("err", title, R(key, { ip: (sam && sam.macIp) || "…" }), acts, [extra || null, tech]);
}

function spinner(small) {
  return h("span", { class: "spin" + (small ? " s" : ""), "aria-hidden": "true" });
}

function wait(text, sub) {
  return h("div", { class: "wait" }, spinner(), h("div", {}, text, sub ? h("small", {}, sub) : null));
}

function bar(fraction) {
  const i = h("i", {});
  i.style.width = Math.round(Math.max(0, Math.min(1, fraction)) * 100) + "%";
  return h("div", { class: "bar", role: "progressbar", "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": String(Math.round(fraction * 100)) }, i);
}

/// A numbered list; each item `{ text: nodes, sub, now }`; numbering starts at `start`.
function howto(items, start) {
  return h(
    "ol",
    { class: "howto", start: String(start || 1) },
    items.map((it, i) =>
      h("li", { class: it.now ? "now" : null }, h("span", { class: "num", "aria-hidden": "true" }, String((start || 1) + i)), it.text, it.sub ? h("small", {}, it.sub) : null),
    ),
  );
}

/// Radio cards. `options`: [{ value, title, sub, tag, tagOk }].
function choice(options, selected, onPick, label, cls) {
  const group = h("div", { class: "choice" + (cls ? " " + cls : ""), role: "radiogroup", "aria-label": label || null });
  const buttons = options.map((o, i) => {
    const on = o.value === selected;
    const b = h(
      "button",
      {
        type: "button",
        class: "opt" + (on ? " sel" : ""),
        role: "radio",
        "aria-checked": on ? "true" : "false",
        tabindex: on || (selected == null && i === 0) ? "0" : "-1",
        key: "opt-" + o.value,
        onclick: () => onPick(o.value),
      },
      h("span", { class: "radio", "aria-hidden": "true" }),
      h("span", { class: "txt" }, h("b", {}, o.title), o.sub ? h("small", {}, o.sub) : null),
      o.tag ? h("span", { class: "tag" + (o.tagOk ? " ok" : "") }, o.tag) : null,
    );
    return b;
  });
  group.addEventListener("keydown", (e) => {
    const dir = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[e.key];
    if (!dir) return;
    e.preventDefault();
    const at = buttons.indexOf(document.activeElement);
    const next = buttons[(at + dir + buttons.length) % buttons.length];
    next.focus();
    next.click();
  });
  append(group, buttons);
  return group;
}

// Each painted area remembers what it last showed; it is rebuilt only when that changes.
const painted = {};
let focusAfterPaint = null;

function paint(id, sig, build) {
  const json = JSON.stringify(sig);
  const el = $(id);
  if (painted[id] === json) return false;
  painted[id] = json;
  const active = document.activeElement;
  const keep = active && el.contains(active) ? active.dataset.key : null;
  el.replaceChildren();
  append(el, [build()]);
  const want = focusAfterPaint || keep;
  if (want) {
    const f = el.querySelector('[data-key="' + CSS.escape(want) + '"]');
    if (f) {
      f.focus();
      if (f.tagName === "INPUT" && f.type === "text") f.setSelectionRange(f.value.length, f.value.length);
      if (focusAfterPaint === want) focusAfterPaint = null;
    }
  }
  return true;
}

function repaint() {
  for (const k of Object.keys(painted)) delete painted[k];
  render();
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (e) {
    const ta = h("textarea", { class: "sr-only", "aria-hidden": "true" });
    ta.value = text;
    document.body.append(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch (e2) {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

let copiedUntil = 0;
async function copyLog() {
  const lines = await invoke("show_log");
  if (await copyText(lines.join("\n"))) {
    copiedUntil = Date.now() + 2500;
    announce(s("log.copied"));
    renderLogLabels();
  }
}

let lastAnnounced = "";
function announce(text) {
  if (!text || text === lastAnnounced) return;
  lastAnnounced = text;
  $("live").textContent = text;
}

// --- Tabs ---

const TABS = ["code", "samsung", "log"];

function setTab(next, byPerson) {
  if (!TABS.includes(next)) return;
  tab = next;
  for (const b of document.querySelectorAll("#tabs [role=tab]")) {
    const on = b.dataset.tab === tab;
    b.setAttribute("aria-selected", on ? "true" : "false");
    b.tabIndex = on ? 0 : -1;
  }
  if (byPerson) invoke("set_view", { view: tab });
  if (tab === "log") shownLogLen = -1;
  if (ui && state) {
    render();
    if (tab === "samsung") tick();
  }
}

// --- First run: «Какой у вас телевизор?» ---

let firstPick = null;

function isFirstRun() {
  return state.tvKind == null && state.adb.kind === "needsConsent" && !state.notices.includes("port.busy");
}

function renderFirst() {
  paint("first-body", [ui.lang, firstPick], () => [
    h("h2", {}, s("first.title")),
    h("p", { class: "lead" }, s("first.lead")),
    choice(
      [
        { value: "android", title: s("first.android"), sub: s("first.android.sub") },
        { value: "samsung", title: s("first.samsung"), sub: s("first.samsung.sub") },
        { value: "unknown", title: s("first.unknown"), sub: s("first.unknown.sub") },
      ],
      firstPick,
      (v) => {
        firstPick = v;
        render();
      },
      s("first.title"),
      "big",
    ),
  ]);
  paint("first-foot", [ui.lang, firstPick], () => [
    h("span", { class: "hint" }, s("first.hint")),
    btn(s("first.next"), () => invoke("set_tv_kind", { kind: firstPick }), "pri", { disabled: !firstPick }),
  ]);
}

// --- The Android TV tab ---

function renderCode() {
  const a = acct();
  const sig = [ui.lang, state.adb, state.notices, state.code, state.tvModels, state.running, a && [a.signedIn, a.tvs]];
  paint("code-body", sig, () => {
    const out = [];
    if (state.notices.includes("port.busy")) {
      out.push(
        banner("err", s("port.busy.title"), s("port.busy"), [btn(s("port.restart"), () => invoke("restart_bridge"), "pri", { cls: "sm" })], h("p", { class: "tech" }, s("port.busy.tech"))),
      );
      return out;
    }
    if (state.notices.includes("lan.denied")) out.push(banner("warn", s("lan.title"), s("lan.denied"), null, h("p", { class: "tech" }, s("lan.hint"))));
    if (state.notices.includes("pairing.disabled")) out.push(banner("warn", s("pairing.title"), s("pairing.disabled")));
    if (state.notices.includes("firetv.found")) {
      out.push(banner("warn", s("firetv.title"), s("firetv.found"), null, h("ol", {}, ["firetv.s1", "firetv.s2", "firetv.s3", "firetv.s4", "firetv.s5"].map((k) => h("li", {}, s(k))))));
    }
    const a = state.adb;
    if (a.kind === "needsConsent") {
      out.push(h("h2", {}, s("adb.title")), h("p", { class: "lead" }, s("adb.consent", { size: a.sizeMb })), h("p", {}, btn(s("adb.terms"), () => invoke("open_terms"), "link")));
    } else if (a.kind === "downloading") {
      out.push(h("h2", {}, s("adb.title")), wait(s("adb.downloading")), bar(a.total ? a.done / a.total : 0));
    } else if (a.kind === "starting") {
      out.push(wait(s("adb.starting")));
    } else if (a.kind === "failed") {
      out.push(
        banner("err", s("adb.failed.title"), s("adb.failed"), [
          btn(s("adb.retry"), () => invoke("retry_adb"), "pri", { cls: "sm" }),
          btn(s("adb.pick"), () => invoke("pick_adb_zip"), "link"),
        ]),
      );
    } else if (a.kind === "startFailed") {
      out.push(banner("err", s("adb.startFailed.title"), s("adb.startFailed"), [btn(s("adb.retry"), () => invoke("retry_adb"), "pri", { cls: "sm" })]));
    } else {
      const models = state.tvModels || [];
      out.push(
        models.length
          ? banner("ok", s("tv.connected", { model: models.join(", ") }), s("tv.connected.sub"))
          : banner("warn", s("tv.none"), s("tv.none.sub")),
      );
      out.push(
        h(
          "div",
          { class: "codebox" },
          h("div", { class: "label" }, s("code.label")),
          state.code ? h("div", { class: "code" }, state.code) : h("p", { class: "lead" }, s("code.none")),
          state.code ? h("p", { class: "lead" }, s("code.lead")) : null,
        ),
      );
      out.push(accountTvs());
    }
    out.push(h("div", { class: "grow" }), h("p", { class: "small muted" }, s(ui.windows ? "closeNote.win" : "closeNote")));
    return out;
  });
  paint("code-foot", sig, () => {
    if (state.notices.includes("port.busy")) return [];
    if (state.adb.kind === "needsConsent") return [btn(s("adb.accept"), () => invoke("accept_adb_download"), "pri", { cls: "wide" })];
    if (state.adb.kind === "ready") return [btn(s("open"), () => invoke("open_site"), "pri", { cls: "wide" })];
    return [];
  });
}

// --- The Samsung tab: a six-step wizard, its step derived from the backend's state ---

// Window-only state (the window is made anew when opened; what matters lives in Rust).
const L = {
  viewStep: null,
  manual: false,
  ipDraft: "",
  confirm: false,
  choice: null,
  celebrateUntil: 0,
  prevStep: 0,
  autoScanned: false,
  lastRecheck: 0,
  autoBusy: false,
  stickyErr: null,
  appsAsked: false,
  installClickedAt: 0,
  ticks: {},
  conflictShown: false,
};

function tvOf(v) {
  return v.tvs.find((t) => t.ip === v.selected) || null;
}

function tvName(tv) {
  return tv.name || tv.model;
}

/// 1 find, 2 Developer Mode, 3 Samsung sign-in, 4 ReTV Home, 5 apps, 6 ads, 7 summary.
function stepOf(v) {
  const tv = tvOf(v);
  if (!tv) return 1;
  if (!(tv.devMode && tv.hostOk && v.duid)) return 2;
  if (!v.cert) return 3;
  if (!v.installed) return 4;
  if (!v.appsPassed) return 5;
  if (!v.privacyPassed) return 6;
  return 7;
}

/// The error to show: during the window's own re-check the last one stays (no flicker).
function errOf(v) {
  if (v.busy === "refresh" && L.autoBusy) return L.stickyErr;
  if (!v.busy) L.autoBusy = false;
  return v.error;
}

function samAuto(v, step, err) {
  if (document.visibilityState === "hidden") return;
  const now = Date.now();
  if (!v.scanned && !v.selected && !v.busy && !L.autoScanned) {
    L.autoScanned = true;
    invoke("samsung_scan");
    return;
  }
  const waiting = (step === 2 && tvOf(v) && now >= L.celebrateUntil) || (step >= 3 && step <= 6 && TV_DOWN.has(err));
  if (waiting && !v.busy && now - L.lastRecheck >= RECHECK_MS) {
    L.lastRecheck = now;
    L.autoBusy = true;
    L.stickyErr = v.error;
    invoke("samsung_refresh", { auto: true });
  }
  if (step === 5 && !v.appsLoaded && !v.busy && !L.appsAsked && !err) {
    L.appsAsked = true;
    invoke("samsung_apps");
  }
}

function pass(step, passed) {
  // An error belongs to the step that produced it: it goes with the step (the backend clears it too).
  if (sam && !sam.busy) {
    sam.error = null;
    sam.status = null;
    sam.refusedBuiltin = [];
  }
  L.stickyErr = null;
  invoke("samsung_pass", { step, passed });
  repaint();
}

function ipBox(mac, tvIp) {
  const parts = mac.split(".");
  return h(
    "div",
    { class: "box" },
    h("div", { class: "label" }, s("samsung.ipbox.label")),
    h("div", { class: "ip", "aria-label": mac }, parts.map((p, i) => (i ? [h("span", { class: "dot", "aria-hidden": "true" }, "."), p] : p))),
    h("div", { class: "ipsub" }, h("span", {}, s("samsung.ipbox.sub")), h("span", {}, R("samsung.ipbox.tv", { ip: tvIp }))),
  );
}

function ipField(v) {
  const bad = v.error === "samsung.err.badIp";
  const input = h("input", {
    type: "text",
    id: "sam-ip",
    key: "sam-ip",
    inputmode: "decimal",
    autocomplete: "off",
    spellcheck: "false",
    placeholder: s("samsung.ip.placeholder"),
    "aria-invalid": bad ? "true" : null,
    "aria-describedby": "sam-ip-help",
    value: L.ipDraft,
    oninput: (e) => {
      L.ipDraft = e.target.value;
    },
  });
  return h(
    "form",
    {
      class: "field",
      onsubmit: (e) => {
        e.preventDefault();
        invoke("samsung_add_ip", { ip: L.ipDraft });
        L.viewStep = null;
      },
    },
    h("label", { for: "sam-ip" }, s("samsung.ip.label")),
    h("div", { class: "in" }, input, h("button", { type: "submit", class: "btn sec", key: "sam-ip-find" }, s("samsung.ip.find"))),
    bad ? h("div", { class: "bad" }, s("samsung.err.badIp")) : null,
    h("div", { class: "help", id: "sam-ip-help" }, R("samsung.ip.help")),
  );
}

function tvCards(v) {
  return choice(
    v.tvs.map((tv) => ({
      value: tv.ip,
      title: tvName(tv),
      sub: [tv.model, tv.ip].filter(Boolean).join(" · "),
      tag: tv.devMode && tv.hostOk ? s("samsung.tag.ready") : tv.year ? String(tv.year) : null,
      tagOk: tv.devMode && tv.hostOk,
    })),
    v.selected,
    (ip) => {
      L.viewStep = null;
      if (ip !== v.selected) invoke("samsung_select", { ip });
      else repaint();
    },
    s("samsung.pick.title"),
  );
}

const copyLogBtn = () => btn(s("samsung.copyLog"), copyLog, "link");

/// The TV went quiet in a later step: what to do, and Bridge re-checks by itself.
function tvDown(err) {
  return [
    errBanner(err, [btn(s("samsung.retryNow"), () => invoke("samsung_refresh"), "sec", { cls: "sm" })]),
    wait(s("samsung.dev.wait"), s("samsung.recheckSub")),
  ];
}

function step1(v, cur) {
  const body = [];
  const foot = [];
  const scanning = v.busy === "scan" || (!v.scanned && !v.selected);
  if (scanning) {
    body.push(h("h2", {}, s("samsung.find.title")), h("p", { class: "lead" }, s("samsung.find.lead")), wait(s("samsung.find.wait"), s("samsung.find.waitSub")));
    if (L.manual) body.push(ipField(v));
    else foot.push(btn(s("samsung.find.manual"), () => ((L.manual = true), repaint()), "link"));
    foot.push(btn(s("samsung.next"), null, "pri", { cls: "right", disabled: true }));
  } else if (v.tvs.length === 0) {
    body.push(
      h("h2", {}, s("samsung.none.title")),
      banner("warn", s("samsung.none.check"), s("samsung.none.text")),
      ipField(v),
    );
    foot.push(btn(s("samsung.none.android"), () => setTab("code", true), "link"), btn(s("samsung.again"), () => invoke("samsung_scan"), "sec", { cls: "right" }));
  } else {
    body.push(
      h("h2", {}, s("samsung.pick.title")),
      h("p", { class: "lead" }, v.selected ? s("samsung.pick.current") : plural("samsung.pick.lead", v.tvs.length)),
      tvCards(v),
      h("div", {}, btn(s("samsung.again"), () => invoke("samsung_scan"), "link", { disabled: !!v.busy })),
    );
    if (L.manual) body.push(ipField(v));
    else foot.push(btn(s("samsung.find.manual"), () => ((L.manual = true), repaint()), "link"));
    if (v.selected && cur > 1) foot.push(btn(s("samsung.backToStep", { n: Math.min(cur, 6) }), () => ((L.viewStep = null), repaint()), "pri", { cls: "right" }));
  }
  if (v.error && v.error !== "samsung.err.badIp" && !v.busy) body.push(errBanner(v.error, [btn(s("samsung.again"), () => invoke("samsung_scan"), "sec", { cls: "sm" })]));
  return { body, foot };
}

function waitFoot() {
  return [
    h("span", { class: "hint" }, spinner(true), h("span", {}, h("b", {}, s("samsung.dev.wait")), s("samsung.dev.waitSub"))),
    btn(s("samsung.dev.checkNow"), () => invoke("samsung_refresh"), "sec"),
  ];
}

function step2(v, tv, err, celebrate) {
  const body = [];
  if (celebrate) {
    body.push(
      h("h2", {}, s("samsung.dev.ready.title")),
      banner("ok", s("samsung.dev.ready.ban"), s("samsung.dev.ready.text", { name: tvName(tv) })),
      h("p", { class: "lead" }, s("samsung.dev.ready.lead")),
    );
    return {
      body,
      foot: [h("span", { class: "hint" }, s("samsung.dev.ready.auto", { n: celebrate })), btn(s("samsung.next"), () => ((L.celebrateUntil = 0), repaint()), "pri")],
    };
  }
  const box = v.macIp ? ipBox(v.macIp, tv.ip) : banner("warn", null, s("samsung.noMacIp"));
  if (err === "samsung.err.devClosed") {
    // REST says on and trusts this computer, yet the TV shut sdb: its own switch, or quick standby.
    body.push(h("h2", {}, s("samsung.dev.title")), errBanner(err), box);
    return { body, foot: waitFoot() };
  }
  if (tv.devMode && !tv.hostOk && tv.devIp) {
    // 127.0.0.1 is what ReTV on the TV asks for to remove apps without a computer: say so, not «another computer».
    const loop = tv.devIp === "127.0.0.1" || tv.devIp === "1.0.0.127";
    body.push(
      h("h2", {}, s("samsung.dev.other.title")),
      loop
        ? banner("warn", s("samsung.dev.loop.ban"), R("samsung.dev.loop.text"))
        : banner("warn", s("samsung.dev.other.ban"), R("samsung.dev.other.text", { ip: tv.devIp })),
      howto([{ text: R("samsung.dev.other.1") }, { text: R("samsung.dev.other.2"), now: true }]),
      box,
      howto([{ text: R("samsung.dev.other.3") }], 3),
    );
  } else {
    body.push(
      h("h2", {}, s("samsung.dev.title")),
      howto([
        { text: R("samsung.dev.1") },
        { text: R("samsung.dev.2"), sub: R("samsung.dev.2sub") },
        { text: R("samsung.dev.3") },
        { text: R("samsung.dev.4"), sub: R("samsung.dev.4sub"), now: true },
      ]),
      box,
      howto([{ text: R("samsung.dev.5") }], 5),
    );
    if (err === "samsung.err.hostIp") body.push(errBanner(err));
  }
  if (err && !TV_DOWN.has(err) && err !== "samsung.err.hostIp") body.push(errBanner(err, [copyLogBtn()]));
  return { body, foot: waitFoot() };
}

function step3(v, err) {
  const body = [];
  if (v.busy === "signIn") {
    body.push(
      h("h2", {}, s("samsung.cert.waitTitle")),
      wait(s("samsung.cert.wait"), s("samsung.cert.waitSub")),
      h("p", { class: "lead" }, s("samsung.cert.waitLead")),
      h("p", { class: "small muted" }, s("samsung.cert.reopenHint")),
    );
    return {
      body,
      foot: [btn(s("samsung.cert.cancel"), () => invoke("samsung_sign_in_cancel"), "link"), btn(s("samsung.cert.reopen"), () => invoke("samsung_sign_in_reopen"), "sec", { cls: "right" })],
    };
  }
  body.push(h("h2", {}, s("samsung.cert.title")), h("p", { class: "lead" }, s("samsung.cert.lead")));
  if (err && TV_DOWN.has(err)) body.push(tvDown(err));
  else if (err) body.push(errBanner(err, err === "samsung.err.tooMany" ? [] : [btn(s("samsung.retry"), () => invoke("samsung_sign_in"), "sec", { cls: "sm" }), copyLogBtn()]));
  body.push(
    howto([{ text: s("samsung.cert.1") }, { text: s("samsung.cert.2") }, { text: s("samsung.cert.3") }]),
    h("div", { class: "box small" }, h("b", {}, s("samsung.cert.privacyB")), " ", s("samsung.cert.privacy")),
    h("div", {}, btn(s("samsung.cert.create"), () => invoke("samsung_open_account"), "link")),
  );
  return { body, foot: [btn(s("samsung.cert.signIn"), () => invoke("samsung_sign_in"), "pri", { cls: "wide", disabled: !!v.busy })] };
}

function startInstall(cmd) {
  L.installClickedAt = Date.now();
  invoke(cmd);
  repaint();
}

const STAGES = ["cert", "prepare", "remove", "send", "install", "start"];

function step4(v, err) {
  const body = [];
  const installing = v.busy === "install" || (Date.now() < L.installClickedAt + 2000 && !v.installed && !v.installConflict);
  if (installing) {
    const stages = STAGES.filter((st) => st !== "remove" || v.installReplace);
    const at = stages.indexOf(v.installStage || "prepare");
    body.push(
      h("h2", {}, s("samsung.install.workingTitle")),
      h("p", { class: "lead" }, s("samsung.install.workingLead")),
      h(
        "ul",
        { class: "prog" },
        stages.map((st, i) => {
          const state = i < at ? "done" : i === at ? "now" : "todo";
          const mark = state === "done" ? icon("check", 14) : state === "now" ? spinner(true) : null;
          return h("li", { class: state, "aria-current": state === "now" ? "step" : null }, h("span", { class: "st" }, mark), s("samsung.install.s." + st));
        }),
      ),
    );
    return { body, foot: [btn(s("samsung.next"), null, "pri", { cls: "right", disabled: true })] };
  }
  if (v.installConflict) {
    body.push(
      h("h2", {}, s("samsung.install.conflictTitle")),
      banner("warn", s("samsung.install.conflictBan"), s("samsung.install.conflict"), [
        btn(s("samsung.install.replace"), () => startInstall("samsung_reinstall"), "pri", { cls: "sm", disabled: !!v.busy }),
        btn(s("samsung.install.keep"), () => invoke("samsung_install_cancel"), "link", { key: "install-keep" }),
      ]),
      h("p", { class: "small muted" }, s("samsung.install.conflictOr")),
    );
    return { body, foot: [] };
  }
  if (v.retvOnTv && !err) {
    // ReTV Home is on the TV already (vd_applist): update it, or go on with the one there.
    body.push(h("h2", {}, s("samsung.install.already")), h("p", { class: "lead" }, s("samsung.install.alreadyText")));
    if (!v.wgt) body.push(banner("warn", null, s("samsung.err.noWgt")));
    return {
      body,
      foot: [
        btn(s("samsung.install.update"), () => startInstall("samsung_install"), "sec", { disabled: !v.wgt || !!v.busy }),
        btn(s("samsung.next"), () => invoke("samsung_keep_installed"), "pri", { cls: "right", disabled: !!v.busy }),
      ],
    };
  }
  body.push(h("h2", {}, s("samsung.install.title")), h("p", { class: "lead" }, s("samsung.install.lead")));
  if (err && TV_DOWN.has(err)) body.push(tvDown(err));
  else if (err === "samsung.err.wrongCert") body.push(errBanner(err, [btn(s("samsung.cert.signIn"), () => invoke("samsung_sign_in"), "sec", { cls: "sm" })]));
  else if (err && err !== "samsung.err.otherAuthor") body.push(errBanner(err, [btn(s("samsung.retry"), () => startInstall("samsung_install"), "sec", { cls: "sm" }), copyLogBtn()]));
  if (!v.wgt) body.push(banner("warn", null, s("samsung.err.noWgt")));
  return { body, foot: [btn(s("samsung.install.button"), () => startInstall("samsung_install"), "pri", { cls: "wide", disabled: !v.wgt || !!v.busy })] };
}

function ticked(v, a) {
  return Object.prototype.hasOwnProperty.call(L.ticks, a.appId) ? L.ticks[a.appId] : a.ticked;
}

function usesText(n) {
  if (n === null || n === undefined) return s("samsung.apps.unknown");
  if (n === 0) return s("samsung.apps.never");
  return plural("samsung.apps.uses", n);
}

/// An app Samsung does not let remove: greyed, no checkbox.
function builtinRow(a) {
  return h("div", { class: "app off" }, h("span", { class: "cb", "aria-hidden": "true" }), h("span", { class: "n" }, a.title), h("span", { class: "u" }, s("samsung.apps.builtin")));
}

function step5(v, err) {
  const body = [h("h2", {}, s("samsung.apps.title"))];
  const skip = btn(s("samsung.skip"), () => pass(5, true), "link");
  const next = (disabled) => btn(s("samsung.next"), () => pass(5, true), "pri", { cls: "right", disabled });
  if (err && TV_DOWN.has(err)) return { body: body.concat(tvDown(err)), foot: [skip, next(true)] };
  if (!v.appsLoaded) {
    if (err && v.busy !== "apps") body.push(errBanner(err, [btn(s("samsung.retry"), () => invoke("samsung_apps"), "sec", { cls: "sm" }), copyLogBtn()]));
    else body.push(wait(s("samsung.apps.loading")));
    return { body, foot: [skip, next(true)] };
  }
  if (v.busy === "remove") {
    body.push(wait(s("samsung.apps.removing")));
    return { body, foot: [next(true)] };
  }
  const offered = v.apps.filter((a) => !a.builtin);
  const n = offered.filter((a) => ticked(v, a)).length;
  if (v.refusedBuiltin && v.refusedBuiltin.length) {
    body.push(banner("warn", s("samsung.apps.builtinRefused", { names: v.refusedBuiltin.join(", ") }), s("samsung.apps.builtinSub")));
  }
  if (v.removed > 0 && n === 0) {
    body.push(
      v.status === "samsung.status.removedSome"
        ? banner("warn", plural("samsung.apps.removed", v.removed), s("samsung.status.removedSome"))
        : banner("ok", plural("samsung.apps.removed", v.removed), s("samsung.apps.before")),
    );
  } else {
    body.push(h("p", { class: "lead" }, s("samsung.apps.lead")));
  }
  if (err && err !== "samsung.err.nothingTicked") body.push(errBanner(err, [copyLogBtn()]));
  if (offered.length === 0) {
    body.push(h("p", {}, s("samsung.apps.empty")));
    if (v.apps.length) body.push(h("div", { class: "apps" }, v.apps.map(builtinRow)));
    return { body, foot: [next(false)] };
  }
  body.push(
    h(
      "div",
      { class: "selbar" },
      h("span", {}, s("samsung.apps.ticked", { n, total: offered.length })),
      n > 0 && !L.confirm
        ? btn(
            s("samsung.apps.untickAll"),
            () => {
              for (const a of offered) {
                if (ticked(v, a)) {
                  L.ticks[a.appId] = false;
                  invoke("samsung_tick", { appId: a.appId, on: false });
                }
              }
              repaint();
            },
            "link",
          )
        : null,
    ),
    h(
      "div",
      { class: "apps" + (L.confirm ? " dim" : "") },
      v.apps.map((a) => {
        if (a.builtin) return builtinRow(a);
        const on = ticked(v, a);
        return h(
          "label",
          { class: "app" },
          h("input", {
            type: "checkbox",
            key: "app-" + a.appId,
            checked: on,
            disabled: L.confirm || !!v.busy,
            onchange: (e) => {
              L.ticks[a.appId] = e.target.checked;
              invoke("samsung_tick", { appId: a.appId, on: e.target.checked });
              repaint();
            },
          }),
          h("span", { class: "cb", "aria-hidden": "true" }, on ? icon("check", 14) : null),
          h("span", { class: "n" }, a.title),
          h("span", { class: "u" }, usesText(a.useCount)),
        );
      }),
    ),
  );
  if (L.confirm && n > 0) {
    // The question goes above the (dimmed) list, where it is seen without scrolling.
    body.splice(2, 0, h("div", { class: "box", role: "alertdialog", "aria-label": plural("samsung.apps.confirm", n) }, h("b", {}, plural("samsung.apps.confirm", n)), h("p", { class: "small muted" }, s("samsung.apps.confirmSub"))));
    return {
      body,
      foot: [
        btn(s("samsung.apps.cancel"), () => ((L.confirm = false), repaint()), "sec", { cls: "right", key: "apps-cancel" }),
        btn(
          s("samsung.apps.confirmYes"),
          () => {
            L.confirm = false;
            invoke("samsung_remove");
            repaint();
          },
          "dan",
        ),
      ],
    };
  }
  return {
    body,
    foot: [
      skip,
      n > 0
        ? btn(
            plural("samsung.apps.remove", n),
            () => {
              L.confirm = true;
              // Removing asks first; Cancel is the focused choice.
              focusAfterPaint = "apps-cancel";
              repaint();
            },
            "pri",
            { cls: "right", disabled: !!v.busy },
          )
        : next(false),
    ],
  };
}

function remoteErr(err) {
  if (err === "samsung.err.remoteDenied") {
    return errBanner(err, null, howtoSmall(["samsung.denied.1", "samsung.denied.2", "samsung.denied.3"]));
  }
  return errBanner(err);
}

function howtoSmall(keys) {
  return h("ol", {}, keys.map((k) => h("li", {}, R(k))));
}

function step6(v, err) {
  const p = v.privacy ? v.privacy.step : "ready";
  const round = (v.privacy && v.privacy.round) || 1;
  const running = v.busy === "privacy";
  const answer = (a) => () => {
    L.choice = null;
    invoke("samsung_privacy", { answer: a });
  };
  const start = (label) => btn(s(label), () => ((L.choice = null), invoke("samsung_privacy_start")), label === "samsung.privacy.start" ? "pri" : "link", { cls: label === "samsung.privacy.start" ? "right" : null, disabled: !!v.busy });
  const skip = btn(s("samsung.skip"), () => pass(6, true), "link");
  const stop = btn(s("samsung.privacy.stop"), () => invoke("samsung_privacy_stop"), "sec", { cls: "right" });
  const second = round === 2 ? h("p", { class: "small muted" }, s("samsung.privacy.secondRound")) : null;
  if (running && p === "pressing") {
    if (v.pressingLeftMs === null || v.pressingLeftMs === undefined) {
      return { body: [h("h2", {}, s("samsung.privacy.pressTitle")), second, wait(s("samsung.privacy.connecting"))], foot: [stop] };
    }
    const left = Math.ceil(v.pressingLeftMs / 1000);
    const total = Math.max(left, L.pressTotal || 15);
    L.pressTotal = total;
    return {
      body: [
        h("h2", {}, s("samsung.privacy.pressTitle")),
        h("p", { class: "lead" }, s("samsung.privacy.pressing")),
        second,
        h("div", { class: "count", "aria-hidden": "true" }, String(left)),
        h("p", { class: "center muted small" }, plural("samsung.privacy.left", left)),
        bar(1 - left / total),
      ],
      foot: [stop],
    };
  }
  L.pressTotal = 0;
  // Several Samsung TVs in the account: the person says which one this is.
  if (running && v.privacyPick) {
    const title = s("samsung.privacy.pickTitle");
    const options = v.privacyPick
      .map((t) => ({ value: t.id, title: t.name || t.model, sub: t.name && t.model && !t.name.includes(t.model) ? t.model : null }))
      .concat([{ value: "none", title: s("samsung.privacy.pickNone"), sub: s("samsung.privacy.pickNone.sub") }]);
    const pick = () => {
      const tv = L.choice === "none" ? null : L.choice;
      L.choice = null;
      invoke("samsung_privacy_pick", { tv });
    };
    return {
      body: [h("h2", {}, title), h("p", { class: "lead" }, s("samsung.privacy.pickLead")), choice(options, L.choice, (c) => ((L.choice = c), repaint()), title)],
      foot: [stop, btn(s("samsung.next"), L.choice ? pick : null, "pri", { cls: "right", disabled: !L.choice })],
    };
  }
  if (running && (p === "reading" || p === "checking")) {
    const reading = p === "reading";
    return {
      body: [h("h2", {}, s(reading ? "samsung.privacy.readingTitle" : "samsung.privacy.checkingTitle")), second, wait(s(reading ? "samsung.privacy.reading" : "samsung.privacy.checking"))],
      foot: [stop],
    };
  }
  if (running && (p === "askScreen" || p === "askResult")) {
    const screen = p === "askScreen";
    const title = s(screen ? "samsung.privacy.askScreenTitle" : "samsung.privacy.askResultTitle");
    const opt = (value, sub) => ({ value, title: s("samsung.privacy.a." + value), sub: s(sub || "samsung.privacy.a." + value + ".sub") });
    const options = screen
      ? [opt("screenOpen"), opt("off"), opt("on"), opt("unknown"), opt("other")]
      : [opt("off"), opt("on", round < 2 ? "samsung.privacy.a.onAfter.sub" : "samsung.privacy.a.unknown.sub"), opt("unknown"), opt("other")];
    // The request could not reach ReTV Home through the account: why, and (the TV not signed in)
    // how to open Samsung's screen by hand meanwhile.
    const hint = v.privacyHint;
    const lead = hint
      ? [banner("warn", null, s(hint)), hint === "samsung.privacy.tvSignIn" && screen ? h("p", { class: "small" }, s("samsung.privacy.openYourself")) : null]
      : [h("p", { class: "lead" }, s("samsung.privacy.noReport"))];
    return {
      body: [h("h2", {}, title)].concat(lead, [second, choice(options, L.choice, (c) => ((L.choice = c), repaint()), title)]),
      foot: [stop, btn(s("samsung.next"), L.choice ? answer(L.choice) : null, "pri", { cls: "right", disabled: !L.choice })],
    };
  }
  if (err && TV_DOWN.has(err)) return { body: [h("h2", {}, s("samsung.privacy.pairTitle"))].concat(tvDown(err)), foot: [skip] };
  if (!v.remote) {
    const body = [h("h2", {}, s("samsung.privacy.pairTitle")), h("p", { class: "lead" }, s("samsung.privacy.pairText"))];
    if (v.busy === "remote") {
      body.push(wait(R("samsung.privacy.pairing"), s("samsung.privacy.pairingSub")));
      return { body, foot: [skip, btn(s("samsung.next"), null, "pri", { cls: "right", disabled: true })] };
    }
    if (err) body.push(REMOTE_ERR.has(err) ? remoteErr(err) : errBanner(err, [copyLogBtn()]));
    return { body, foot: [skip, btn(s(err ? "samsung.tryAgain" : "samsung.privacy.pair"), () => invoke("samsung_remote_pair"), "pri", { cls: "right", disabled: !!v.busy })] };
  }
  const errs = err ? [REMOTE_ERR.has(err) ? remoteErr(err) : errBanner(err)] : [];
  if (p === "stopped") {
    const why = (v.privacy && v.privacy.why) || "unexpected";
    return {
      body: [h("h2", {}, s("samsung.privacy.stoppedTitle")), banner("warn", null, s("samsung.privacy.why." + why))].concat(errs, [h("p", { class: "small" }, R("samsung.privacy.manual"))]),
      foot: [skip, btn(s("samsung.tryAgain"), () => ((L.choice = null), invoke("samsung_privacy_start")), "pri", { cls: "right", disabled: !!v.busy })],
    };
  }
  if (p === "done" || p === "manual") {
    const body = p === "done"
      ? [h("h2", {}, s("samsung.step.6")), banner("ok", s(v.privacy.pressed ? "samsung.privacy.doneOff" : "samsung.privacy.doneAlready"))]
      : [h("h2", {}, s("samsung.privacy.manualTitle")), h("p", {}, R("samsung.privacy.manual"))];
    return { body, foot: [start("samsung.privacy.restart"), btn(s("samsung.next"), () => pass(6, true), "pri", { cls: "right" })] };
  }
  // Ready.
  const a = acct();
  const body = [h("h2", {}, s("samsung.privacy.readyTitle")), h("p", { class: "lead" }, s("samsung.privacy.ready")), h("p", { class: "small muted" }, s("samsung.privacy.readyNote"))];
  if (!(a && a.signedIn)) body.push(h("p", { class: "small" }, s("samsung.privacy.accountHint")));
  return { body: body.concat(errs), foot: [skip, start("samsung.privacy.start")] };
}

function step7(v, tv) {
  const p = v.privacy ? v.privacy.step : null;
  const row = (ok, title, sub) => h("li", { class: ok ? null : "skip" }, icon(ok ? "ok" : "skip"), h("div", {}, title, sub ? h("small", {}, sub) : null));
  const ads = p === "done" ? row(true, s("samsung.done.adsOff")) : p === "manual" ? row(false, s("samsung.done.adsManual")) : row(false, s("samsung.done.adsKept"));
  return {
    body: [
      h("h2", {}, s("samsung.done.title")),
      h("p", { class: "lead" }, [tvName(tv), tv.model && tv.name ? tv.model : null].filter(Boolean).join(", ")),
      h(
        "ul",
        { class: "summary" },
        row(true, s("samsung.done.installed")),
        v.removed > 0 ? row(true, plural("samsung.done.removed", v.removed), s("samsung.done.removedSub")) : row(false, s("samsung.done.appsKept")),
        ads,
      ),
      h("p", { class: "small muted" }, s("samsung.done.note")),
    ],
    foot: [
      btn(
        s("samsung.done.another"),
        () => {
          L.autoScanned = false;
          L.appsAsked = false;
          L.viewStep = null;
          invoke("samsung_forget");
        },
        "sec",
        { cls: "right" },
      ),
    ],
  };
}

/// A finished step, opened again from the stepper.
function revisit(n, v, tv, cur) {
  const back = btn(s("samsung.backToStep", { n: Math.min(cur, 6) }), () => ((L.viewStep = null), repaint()), "pri", { cls: "right" });
  if (n === 2) {
    return {
      body: [h("h2", {}, s("samsung.dev.title")), banner("ok", s("samsung.dev.ready.ban"), s("samsung.dev.ready.text", { name: tvName(tv) })), v.macIp ? ipBox(v.macIp, tv.ip) : null],
      foot: [back],
    };
  }
  if (n === 3) return { body: [h("h2", {}, s("samsung.cert.title")), banner("ok", s("samsung.cert.ok"))], foot: [back] };
  return { body: [h("h2", {}, s("samsung.install.title")), banner("ok", s("samsung.install.ok"))], foot: [back] };
}

function stepper(cur, shown) {
  const items = [1, 2, 3, 4, 5, 6].map((i) => {
    const done = i < cur;
    const cls = i === shown ? "cur" : done ? "done" : "";
    const name = s("samsung.step." + i);
    const label = s("samsung.goStep", { n: i, name });
    const inner =
      done && i !== shown
        ? h("button", {
            type: "button",
            "aria-label": label,
            title: name,
            key: "step-" + i,
            onclick: () => {
              if (i >= 5) {
                L.viewStep = null;
                pass(i, false);
              } else L.viewStep = i;
              repaint();
            },
          })
        : h("span", { "aria-label": label, role: "img" });
    return h("li", { class: cls, "aria-current": i === shown ? "step" : null }, inner);
  });
  return [
    h("ol", { "aria-label": s("samsung.stepsLabel") }, items),
    h("div", { class: "cap" }, shown <= 6 ? s("samsung.step", { n: shown, name: s("samsung.step." + shown) }) : s("samsung.done.title")),
  ];
}

function renderSamsung() {
  const v = sam;
  const cur = stepOf(v);
  const now = Date.now();
  if (L.prevStep === 2 && cur === 3) L.celebrateUntil = now + 3000;
  if (cur !== 5) L.appsAsked = cur > 5 ? L.appsAsked : false;
  if (!v.apps.some((a) => Object.prototype.hasOwnProperty.call(L.ticks, a.appId) && L.ticks[a.appId] !== a.ticked)) L.ticks = {};
  L.prevStep = cur;
  if (L.viewStep !== null && (L.viewStep >= cur || (L.viewStep > 1 && !tvOf(v)))) L.viewStep = null;
  const err = errOf(v);
  const shown = L.viewStep || cur;
  const tv = tvOf(v);
  const celebrate = cur === 3 && now < L.celebrateUntil && !L.viewStep ? Math.ceil((L.celebrateUntil - now) / 1000) : 0;
  const installing = Date.now() < L.installClickedAt + 2000;

  const model = [ui.lang, v, err, cur, shown, celebrate, installing, L.manual, L.confirm, L.choice, L.ticks, state.notices];
  paint("sam-steps", [ui.lang, cur, celebrate ? 2 : shown], () => stepper(cur, celebrate ? 2 : shown));
  let view = null;
  const get = () => (view = view || (celebrate ? step2(v, tv, err, celebrate) : build(shown, v, tv, err, cur)));
  paint("sam-body", model, () => get().body);
  paint("sam-foot", model, () => get().foot);
  {
    const cap = $("sam-steps").querySelector(".cap");
    const title = $("sam-body").querySelector("h2");
    const extra = $("sam-body").querySelector(".ban b, .wait");
    announce([cap && cap.textContent, title && title.textContent, extra && extra.textContent].filter(Boolean).join(". "));
  }
  // Another ReTV Home on the TV: the choice gets the focus once, on Cancel.
  if (v.installConflict && !L.conflictShown) {
    const keep = document.querySelector('[data-key="install-keep"]');
    if (keep) keep.focus();
  }
  L.conflictShown = !!v.installConflict;
  samAuto(v, cur, err);
}

function build(shown, v, tv, err, cur) {
  if (shown === 1 || !tv) return step1(v, cur);
  if (shown < cur && shown <= 4) return revisit(shown, v, tv, cur);
  if (shown === 2) return step2(v, tv, err, 0);
  if (shown === 3) return step3(v, err);
  if (shown === 4) return step4(v, err);
  if (shown === 5) return step5(v, err);
  if (shown === 6) return step6(v, err);
  return step7(v, tv);
}

// --- «Войти в ReTV»: the header's account, its menu, the sign-in sheet ---

// Window-only state: what lives in Rust (the attempt, the code, the account) comes with the poll.
const A = { open: false, cellOpen: false, menu: false, focused: false, lastTvsAsk: 0 };

function acct() {
  return state && state.account ? state.account : null;
}

const HOSTS = { world: "retv.app", ru: "ru.retv.app" };

function planOf(a) {
  const k = "acct.plan." + (a.tier || "free");
  return has(k) ? s(k) : s("acct.plan.free");
}

function sheetShown() {
  const a = acct();
  if (!a) return false;
  if (a.phase.kind !== "idle") return true;
  if (a.signedIn) A.open = false;
  return A.open;
}

function openSignIn() {
  A.open = true;
  A.menu = false;
  A.focused = false;
  invoke("account_sign_in");
  render();
}

function closeSheet() {
  const a = acct();
  const k = a ? a.phase.kind : "idle";
  if (k === "starting" || k === "waiting") invoke("account_cancel");
  else if (k === "done" || k === "failed") invoke("account_dismiss");
  if (a) a.phase = { kind: "idle" };
  A.open = false;
  A.cellOpen = false;
  A.focused = false;
  render();
  const back = document.querySelector('[data-key="acct-sign-in"], [data-key="acct-chip"]');
  if (back) back.focus();
}

function retrySignIn() {
  A.open = true;
  A.focused = false;
  invoke("account_dismiss").then(() => invoke("account_sign_in"));
}

function toggleMenu(e) {
  // The chip is painted again below: the document's click handler must not see this click.
  if (e) e.stopPropagation();
  A.menu = !A.menu;
  render();
  if (A.menu) {
    const first = $("acct-menu").querySelector("button");
    if (first) first.focus();
  }
}

function renderAcctHeader() {
  const a = acct();
  const shown = !!(A.menu && a && a.signedIn);
  paint("acct", [ui.lang, a && [a.signedIn, a.label, a.pro], shown], () => {
    if (!a) return [];
    if (!a.signedIn) return [h("button", { type: "button", class: "acct-btn", key: "acct-sign-in", onclick: openSignIn }, s("acct.signIn"))];
    return [
      h(
        "button",
        { type: "button", class: "acct-btn in", key: "acct-chip", "aria-haspopup": "menu", "aria-expanded": shown ? "true" : "false", title: s("acct.menu"), onclick: toggleMenu },
        icon("user", 16),
        h("span", { class: "lbl" }, a.label || s("acct.signedIn")),
        a.pro ? h("span", { class: "badge" }, "Pro") : null,
      ),
    ];
  });
  $("acct-menu").hidden = !shown;
  $("acct-menu").setAttribute("aria-label", s("acct.menu"));
  paint("acct-menu", [ui.lang, shown && [a.label, a.tier, a.host, a.offline]], () =>
    !shown
      ? []
      : [
          h(
            "div",
            { class: "who" },
            h("b", { class: "sel" }, a.label || s("acct.signedIn")),
            h("small", {}, planOf(a)),
            h("small", {}, s("acct.on", { host: a.host || "" })),
            a.offline ? h("small", { class: "warnline" }, s("acct.offline", { host: a.host || "" })) : null,
          ),
          h("hr", {}),
          h(
            "button",
            {
              type: "button",
              role: "menuitem",
              key: "acct-out",
              onclick: () => {
                A.menu = false;
                invoke("account_sign_out");
                render();
              },
            },
            s("acct.signOut"),
          ),
        ],
  );
}

/// «Аккаунт на retv.app · Сменить», and the two sites when open.
function cellLine(a) {
  const line = h(
    "div",
    { class: "cellline" },
    h("span", {}, s("acct.on", { host: a.choiceHost })),
    btn(
      s("acct.change"),
      () => {
        A.cellOpen = !A.cellOpen;
        render();
        const box = $("sheet-body").querySelector(".cellbox");
        if (box) box.scrollIntoView({ block: "nearest" });
      },
      "link",
      { key: "cell-change" },
    ),
  );
  if (!A.cellOpen) return line;
  return h(
    "div",
    { class: "cellbox" },
    line,
    choice(
      [
        { value: "world", title: HOSTS.world },
        { value: "ru", title: HOSTS.ru },
      ],
      a.choice,
      (v) => {
        A.cellOpen = false;
        if (v !== a.choice) {
          A.focused = false;
          invoke("account_set_cell", { cell: v });
        }
        render();
      },
      s("acct.cellMenu.label"),
    ),
    h("p", { class: "small muted" }, s("acct.cell.hint")),
  );
}

function sheetBody(a) {
  const k = a.phase.kind;
  const out = [];
  if (k === "idle" || k === "starting") {
    out.push(h("p", { class: "lead" }, s("acct.sheet.lead")), wait(s("acct.starting")));
  } else if (k === "waiting") {
    out.push(
      howto([
        { text: s("acct.step.opened"), sub: s("acct.step.opened.sub") },
        { text: s("acct.step.allow"), sub: s("acct.step.allow.sub"), now: true },
      ]),
      wait(s("acct.waiting"), s("acct.waiting.sub")),
      h(
        "div",
        { class: "codebox acctcode" },
        h("div", { class: "label" }, s("acct.code.label")),
        h("div", { class: "code" }, a.code || ""),
        h("p", { class: "small muted" }, R("acct.code.go", { go: a.go || "" })),
        h("p", { class: "small muted" }, s("acct.code.never")),
      ),
    );
  } else if (k === "done") {
    out.push(banner("ok", s("acct.done.title"), (a.label || s("acct.signedIn")) + " · " + planOf(a)), h("p", { class: "lead" }, s("acct.done")));
    return out;
  } else if (k === "failed") {
    const key = "acct.err." + a.phase.fail;
    out.push(banner("err", s(key + ".title"), s(key, { host: HOSTS[a.phase.cell] || a.choiceHost })));
  }
  out.push(h("div", { class: "grow" }), cellLine(a));
  return out;
}

function sheetFoot(a) {
  const k = a.phase.kind;
  if (k === "done") return [btn(s("acct.close"), closeSheet, "pri", { cls: "wide", key: "acct-close" })];
  if (k === "failed") return [btn(s("acct.dismiss"), closeSheet, "sec", { key: "acct-dismiss" }), btn(s("acct.retry"), retrySignIn, "pri", { cls: "right", key: "acct-retry" })];
  if (k === "waiting")
    return [btn(s("acct.cancel"), closeSheet, "sec", { key: "acct-cancel" }), btn(s("acct.reopen"), () => invoke("account_reopen"), "pri", { cls: "right", key: "acct-reopen" })];
  return [btn(s("acct.cancel"), closeSheet, "sec", { key: "acct-cancel" })];
}

const BEHIND_SHEET = ["acct", "tabs", "view-first", "view-code", "view-samsung", "view-log"];

function renderSheet() {
  const a = acct();
  const shown = sheetShown();
  $("sheet").hidden = !shown;
  $("scrim").hidden = !shown;
  for (const id of BEHIND_SHEET) {
    if (shown) $(id).setAttribute("inert", "");
    else $(id).removeAttribute("inert");
  }
  if (!shown) {
    A.cellOpen = false;
    A.focused = false;
    return;
  }
  A.menu = false;
  $("sheet-title").textContent = s("acct.sheet.title");
  const x = $("sheet-x");
  x.setAttribute("aria-label", s("acct.dismiss"));
  if (!x.firstChild) x.append(icon("x", 20));
  const sig = [ui.lang, a.phase, a.code, a.go, a.choice, A.cellOpen, a.label, a.pro, a.tier];
  paint("sheet-body", sig, () => sheetBody(a));
  paint("sheet-foot", sig, () => sheetFoot(a));
  if (!A.focused) {
    A.focused = true;
    const f = $("sheet-foot").querySelector(".btn.pri") || $("sheet-foot").querySelector(".btn") || x;
    f.focus();
  }
  const title = $("sheet-body").querySelector(".ban b, .wait");
  announce([s("acct.sheet.title"), title && title.textContent].filter(Boolean).join(". "));
}

/// The account's TVs on the Android TV tab (signed in), or a way to sign in.
function accountTvs() {
  const a = acct();
  if (!a) return null;
  if (!a.signedIn) return h("p", { class: "small" }, btn(s("acct.tvs.signIn"), openSignIn, "link", { key: "tvs-sign-in" }));
  if (!a.tvs) return null;
  return h(
    "div",
    { class: "acct-tvs" },
    h("div", { class: "label" }, s("acct.tvs.title")),
    a.tvs.length
      ? h(
          "ul",
          { class: "tvlist" },
          a.tvs.map((tv) =>
            h(
              "li",
              {},
              icon("tv"),
              h("div", { class: "txt" }, h("div", { class: "n" }, tv.name || tv.model), h("small", {}, [tv.model, tv.kindLine].filter(Boolean).join(" · "))),
              tv.homeVersion ? h("span", { class: "tag" }, s("acct.tvs.home", { v: tv.homeVersion })) : null,
            ),
          ),
        )
      : h("p", { class: "small muted" }, s("acct.tvs.none")),
  );
}

function renderAccount() {
  renderAcctHeader();
  renderSheet();
  const a = acct();
  if (a && a.signedIn && tab === "code" && Date.now() - A.lastTvsAsk > 30000) {
    A.lastTvsAsk = Date.now();
    invoke("account_tvs");
  }
}

document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (sheetShown()) {
    e.preventDefault();
    closeSheet();
  } else if (A.menu) {
    A.menu = false;
    render();
    const chip = document.querySelector('[data-key="acct-chip"]');
    if (chip) chip.focus();
  }
});
document.addEventListener("click", (e) => {
  if (!A.menu) return;
  if ($("acct").contains(e.target) || $("acct-menu").contains(e.target)) return;
  A.menu = false;
  render();
});
$("sheet-x").addEventListener("click", closeSheet);
$("scrim").addEventListener("click", () => {
  const a = acct();
  // A click beside the sheet closes it only when nothing is running.
  if (a && (a.phase.kind === "done" || a.phase.kind === "failed")) closeSheet();
});

// --- Pill, log, static labels ---

function renderPill() {
  let cls = "";
  let key = "pill.on";
  if (state.notices.includes("port.busy")) {
    cls = "off";
    key = "pill.off";
  } else if (sam && stepOf(sam) >= 3 && TV_DOWN.has(errOf(sam))) {
    cls = "warn";
    key = "pill.tvDown";
  }
  const pill = $("pill");
  pill.className = "pill " + cls;
  if (pill.textContent !== s(key)) pill.textContent = s(key);
}

const LANG_CHOICES = ["system", "ru", "uk", "en"];

/// «Язык»: rebuilt only when the language or the choice changes, so an open list is not closed.
function renderLangPick() {
  const pick = $("lang-pick");
  const sig = ui.lang + ":" + ui.langChoice;
  if (pick.dataset.sig === sig) return;
  pick.dataset.sig = sig;
  $("lang-label").textContent = s("lang.label");
  pick.replaceChildren(...LANG_CHOICES.map((c) => h("option", { value: c }, s("lang." + c))));
  pick.value = ui.langChoice || "system";
}

async function chooseLang(choice) {
  try {
    ui = await invoke("set_lang", { choice });
  } catch (e) {
    return;
  }
  repaint();
}

function renderLogLabels() {
  renderLangPick();
  $("log-lead").textContent = s("log.lead");
  const copied = Date.now() < copiedUntil;
  $("log-copy").textContent = s(copied ? "log.copied" : "log.copy");
  $("log-hint").textContent = s("log.hint");
}

async function renderLog() {
  renderLogLabels();
  if (!state || state.logLen === shownLogLen) return;
  shownLogLen = state.logLen;
  const lines = await invoke("show_log");
  const pre = $("log");
  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
  logText = lines.length ? lines.join("\n") : s("log.empty");
  pre.textContent = logText;
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

function render() {
  document.documentElement.lang = ui.lang;
  $("tabs").setAttribute("aria-label", s("tabs.label"));
  for (const t of TABS) $("tab-" + t).textContent = s("tab." + t);
  const first = isFirstRun();
  $("tabs").hidden = first;
  $("view-first").hidden = !first;
  $("view-code").hidden = first || tab !== "code";
  $("view-samsung").hidden = first || tab !== "samsung";
  $("view-log").hidden = first || tab !== "log";
  if (first) renderFirst();
  else if (tab === "code") renderCode();
  else if (tab === "samsung" && sam) renderSamsung();
  else if (tab === "log") renderLog();
  renderPill();
  renderAccount();
}

async function tick() {
  try {
    if (!ui) ui = await invoke("get_strings");
    state = await invoke("get_state");
    // The language changed in the tray's «Язык» menu: the strings again.
    if (state.lang && state.lang !== ui.lang) {
      ui = await invoke("get_strings");
      for (const k of Object.keys(painted)) delete painted[k];
    }
    if (tab === "samsung" || state.view === "samsung") sam = await invoke("samsung_view");
  } catch (e) {
    return;
  }
  if (state.viewSeq !== seenSeq) {
    seenSeq = state.viewSeq;
    tab = TABS.includes(state.view) ? state.view : "code";
    setTab(tab, false);
    return;
  }
  render();
}

for (const b of document.querySelectorAll("#tabs [role=tab]")) {
  b.addEventListener("click", () => setTab(b.dataset.tab, true));
}
$("tabs").addEventListener("keydown", (e) => {
  const dir = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
  if (!dir) return;
  e.preventDefault();
  const next = TABS[(TABS.indexOf(tab) + dir + TABS.length) % TABS.length];
  setTab(next, true);
  $("tab-" + next).focus();
});
$("log-copy").addEventListener("click", copyLog);
$("lang-pick").addEventListener("change", (e) => chooseLang(e.target.value));

setTab("code", false);
tick();
setInterval(tick, POLL_MS);

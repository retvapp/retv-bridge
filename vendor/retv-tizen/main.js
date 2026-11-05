// ReTV Home for Samsung (Tizen web app): the profile chip and the gear (Settings) on the top line, a card there when
// DNS must go back to automatic, the clock, a hero for the focused app, the dock, «Все приложения» (a button under
// the dock), the app menu (OK held or Tools: open, move, add to or remove from the dock, into a folder, hide;
// «Изменить док» in «Все приложения»), and a small settings panel with the ReTV sign-in by code. Folders and the
// hidden apps are in folders.js, «DNS без рекламы» in dns.js. Smart Hub cannot be replaced on a retail Samsung; with
// «Autorun Last App» on, the TV reopens this app at power-on.
//
// Speed rules (2021 TVs, Chromium 76 era, armv7): the last app list is painted from localStorage before the TV answers;
// a key press moves one ring with a transform and toggles a class on two tiles, nothing is rebuilt; the hero changes
// once per frame at most; DOM is rebuilt only when the app list itself changes. ES2017 at most, no framework.
(function () {
  "use strict";
  var L = window.RetvLogic;
  var VERSION = "0.2.0";
  // What the server stores as this TV's ReTV Home version: the platform in it, so the cabinet can tell it apart.
  var WIRE_VERSION = VERSION + "-tizen";
  var lang = L.langOf(navigator.language);
  var S = L.STRINGS[lang], O = S.opt;
  var hasTizen = typeof tizen !== "undefined";
  var K = { apps: "retv.apps", recent: "retv.recent", key: "retv.key", token: "retv.token", cell: "retv.cell", ent: "retv.ent", label: "retv.label",
    dns: "retv.dns", ret: "retv.return", privacyAt: "retv.privacyAt", before: "retv.before",
    dnsAt: "retv.dnsAt", dnsLast: "retv.dnsLast", dock: "retv.dock", noLaunch: "retv.noLaunch",
    folders: "retv.folders", hidden: "retv.hidden", menuUses: "retv.menuUses", coach: "retv.coach", dnsServers: "retv.dnsServers" };

  function $(id) { return document.getElementById(id); }
  function read(k, def) { try { var v = localStorage.getItem(k); return v == null ? def : JSON.parse(v); } catch (e) { return def; } }
  function write(k, v) { try { if (v == null) localStorage.removeItem(k); else localStorage.setItem(k, JSON.stringify(v)); } catch (e) {} }
  function el(tag, cls, text) { var e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
  function cssUrl(p) { return p ? "url('" + String(p).replace(/'/g, "%27") + "')" : "none"; }

  var chip = $("chip"), chipPro = $("chipPro"), dock = $("dock"), dockRing = $("dockRing"), statusEl = $("status");
  var heroArt = $("heroArt"), heroLabel = $("heroLabel"), heroIcon = $("heroIcon"), heroName = $("heroName");
  var all = $("all"), grid = $("grid"), gridRing = $("gridRing"), panel = $("panel"), panelBody = $("panelBody");
  var allEdit = $("allEdit"), allHint = $("allHint"), menuEl = $("menu"), menuBody = $("menuBody");
  var allBtn = $("allLine"), hintEl = $("hint"), foldHint = $("foldHint"), heroMinis = $("heroMinis");
  var gear = $("gear"), warn = $("warn"), coachEl = $("coach");
  var M = S.menu, F = S.folder;

  // --- Static text ---
  $("chipDisc").textContent = S.profile.charAt(0);
  gear.setAttribute("aria-label", S.settings);
  warn.firstChild.textContent = O.dnsRevertCard;
  warn.lastChild.textContent = O.dnsRevertHow;
  coachEl.textContent = M.coach;
  $("chipName").textContent = S.profile;
  allBtn.firstChild.textContent = M.allBtn;
  hintEl.textContent = M.homeHint;
  $("allTitle").textContent = S.allApps;
  $("panelTitle").textContent = S.settings;
  $("menuHint").textContent = M.close;
  document.documentElement.lang = lang;

  // --- Clock ---
  var h24 = true;
  try { h24 = !/a/i.test(tizen.time.getTimeFormat()); } catch (e) {}
  var clockEl = $("clock"), dateEl = $("date"), clockTimer = 0;
  function tick() {
    var d = new Date(), c = L.clockText(d, h24), t = L.dateText(d, lang);
    if (clockEl.textContent !== c) clockEl.textContent = c;
    if (dateEl.textContent !== t) dateEl.textContent = t;
    clearTimeout(clockTimer);
    clockTimer = setTimeout(tick, 60050 - (d.getTime() % 60000));
  }

  var statusTimer = 0;
  // The hint line under the dock («Удерживайте OK…») stands where the status line does, while it has nothing to say;
  // it goes for good once the app menu has been opened HINT_USES times.
  var menuUses = read(K.menuUses, 0);
  if (typeof menuUses !== "number") menuUses = 0;
  function showHint() { hintEl.style.visibility = statusEl.textContent || menuUses >= L.HINT_USES ? "hidden" : ""; }
  function setStatus(text, sticky) {
    statusEl.textContent = text || "";
    showHint();
    clearTimeout(statusTimer);
    if (text && !sticky) statusTimer = setTimeout(function () { statusEl.textContent = ""; showHint(); }, 6000);
  }

  // --- Apps ---
  var recent = read(K.recent, []);
  if (!Array.isArray(recent)) recent = [];
  // Samsung apps this TV will not open (not every TV lets a web app open them): hidden after two failures unless one
  // ever opened, afresh with a new firmware or ReTV Home (checked again once Samsung's webapis answer), and listed in
  // «Скрытые приложения», where OK brings one back.
  var book = L.launchBook(read(K.noLaunch, null), null, VERSION), blocked = L.blockedIds(book);
  function setBook(b) {
    if (b === book) return;
    book = b;
    write(K.noLaunch, b);
    var was = blocked.join(), now = L.blockedIds(b);
    blocked = now;
    if (now.join() !== was) setApps(raw, false);
  }
  function firmware() { try { return String(webapis.productinfo.getFirmware() || ""); } catch (e) { return ""; } }
  function checkFirmware() {
    var fw = firmware();
    if (fw && fw !== book.fw) setBook(L.launchBook(book, fw, VERSION));
  }
  /** The installed apps hidden because this TV will not open them, in home order. */
  function blockedApps() {
    if (!blocked.length) return [];
    var list = raw.filter(function (a) { return blocked.indexOf(a.id) >= 0; });
    return L.order(list, recent).map(function (a) { return { id: a.id, name: a.name, iconPath: a.iconPath, blocked: true }; });
  }
  /** What «Скрытые приложения» lists: the person's hidden apps, then those this TV would not open. */
  function hiddenAll() { return L.hiddenApps(apps, hidden).concat(blockedApps()); }
  var saved = L.dockIds(read(K.dock, null));   // null: the dock was never edited, it is the first apps in home order
  var folders = L.folderList(read(K.folders, null)), hidden = L.hiddenList(read(K.hidden, null));
  var raw = L.expand(read(K.apps, []));
  // apps: every app shown on this TV, in home order; items: what «Все приложения» shows (apps on their own and folders);
  // gl: the grid's tiles (items, then «Скрытые» when apps are hidden); docked: the dock's items.
  var apps = [], items = [], gl = [], docked = [], shownSig = null, gridSig = null, layoutGen = 0;
  var mode = "home", zone = "dock", dockI = 0, gridI = 0, gridTop = 0, allZone = "grid", editing = false, moving = null;
  var lastOpened = null;
  var TW = L.TILE_W, TH = L.TILE_H, PITCH = L.TILE_W + L.TILE_GAP, INSET = L.DOCK_INSET, PAD_V = 30;
  var COLS = 9, GX = 188, GY = 130, ROWS_SHOWN = 6;

  /** A small square for an app inside a folder tile or the hero: its drawn colours and first letter, or its icon. */
  function mini(a, cls) {
    var m = el("i", cls), b = L.brandOf(a, lang);
    if (b) { m.style.background = b[0]; m.style.color = b[1]; m.textContent = b[2].charAt(0).toUpperCase(); }
    else m.style.backgroundImage = cssUrl(a.iconPath);
    return m;
  }
  function tile(a, x, y) {
    var t = el("div", "tile"), b = a.folder || a.more ? null : L.brandOf(a, lang);
    if (a.folder) {
      // Up to 4 of its apps in a row, the name under them.
      t.className += " folder";
      var row = el("div", "minis");
      for (var k = 0; k < a.apps.length && k < 4; k++) row.appendChild(mini(a.apps[k], "m"));
      t.appendChild(row); t.appendChild(el("span", "", a.name));
    } else if (a.more) {
      t.className += " more"; t.textContent = a.name;
    } else if (b) {
      t.className += " brand"; t.style.background = b[0]; t.style.color = b[1]; t.textContent = b[2];
      if (b[3]) t.style.fontSize = b[3] + "px";
      if (b[4]) t.style.boxShadow = "inset 0 0 0 3px " + b[4];
    } else {
      // Samsung's own icons are small and often blank on white: the icon small, the name under it.
      t.className += " plain";
      var i = el("i"); i.style.backgroundImage = cssUrl(a.iconPath);
      t.appendChild(i); t.appendChild(el("span", "", a.name));
    }
    t.style.left = x + "px"; t.style.top = y + "px";
    return t;
  }
  function clearTiles(box, keep) { for (var c = box.lastChild; c; ) { var p = c.previousSibling; if (c !== keep) box.removeChild(c); c = p; } }
  function dockCount() { return docked.length; }
  function dockList() { return docked.map(function (a) { return a.id; }); }
  function inDock(id) { for (var i = 0; i < docked.length; i++) if (docked[i].id === id) return true; return false; }
  /** The dock place of [id], or of the folder in the dock holding it; -1 when it is not on the home screen. */
  function dockIndexOf(id) {
    for (var i = 0; i < docked.length; i++) {
      var d = docked[i];
      if (d.id === id) return i;
      if (d.folder) for (var j = 0; j < d.apps.length; j++) if (d.apps[j].id === id) return i;
    }
    return -1;
  }

  function renderDock() {
    var n = dockCount(), f = document.createDocumentFragment(), w = L.dockWidth(n);
    for (var i = 0; i < n; i++) f.appendChild(tile(docked[i], INSET + i * PITCH, PAD_V));
    clearTiles(dock, dockRing);
    dock.insertBefore(f, dockRing);
    dock.style.width = w + "px";
    dock.style.left = (1920 - w) / 2 + "px";
    dock.style.visibility = n ? "" : "hidden";
  }

  function setApps(list, fromTv) {
    raw = list;
    var shown = blocked.length ? list.filter(function (a) { return blocked.indexOf(a.id) < 0; }) : list;
    var ordered = L.order(shown, recent), sig = L.signature(ordered) + "\u0003" + blocked.join();
    if (fromTv) setStatus(ordered.length ? "" : S.noApps, true);
    if (sig === shownSig) return;
    shownSig = sig;
    var hadDock = dockCount() > 0;
    apps = ordered;
    relayout();
    // The first list on an empty cache: focus goes to the first tile, as it would have from the cache.
    if (mode === "home" && (!dockCount() || zone === "dock" || !hadDock)) refocusHome();
    coachStart();
  }
  /** The home screen's focus after the dock changed: the same dock place (or the last), the chip when it is empty. */
  function refocusHome() {
    if (dockCount()) focusDock(Math.min(dockI, dockCount() - 1), true);
    else { focusChip(); showHero(null, true); }
  }

  /** Items, grid and dock from the apps and the saved layout; the open grid or folder is redrawn. */
  function relayout() {
    items = L.layoutItems(apps, folders, hidden, lang);
    var n = hiddenAll().length;
    gl = n ? items.concat([{ id: "hidden:", more: true, name: L.fmt(F.hiddenTile, n) }]) : items;
    docked = L.dockApps(items, saved);
    renderDock();
    layoutGen++;
    gridSig = null;
    if (mode === "all") { renderGrid(); focusGrid(Math.max(0, Math.min(gridI, gl.length - 1))); }
    if (fold.state.on) fold.refresh();
  }
  /** The dock after an edit ([ids]: the dock as it is to be shown), saved at once. */
  function setDock(ids) {
    saved = ids;
    write(K.dock, ids);
    docked = L.dockApps(items, saved);
    renderDock();
    markGrid();
  }
  /** The layout now ({ dock as shown, folders, hidden }) and after an edit, saved at once. */
  function layoutNow() { return { dock: dockList(), folders: folders, hidden: hidden }; }
  function setLayout(l) {
    saved = l.dock; folders = l.folders; hidden = l.hidden;
    write(K.dock, saved); write(K.folders, folders); write(K.hidden, hidden);
    relayout();
    if (mode === "home") refocusHome();
  }
  function pin(a) {
    var ids = L.dockPin(dockList(), a.id);
    if (!ids) { note(L.plural(L.DOCK_MAX, M.dockFull, lang)); return false; }
    setDock(ids);
    note(L.fmt(M.pinned, a.name));
    return true;
  }
  function unpin(a) {
    setDock(L.dockUnpin(dockList(), a.id));
    note(L.fmt(M.unpinned, a.name));
    if (mode === "home") refocusHome();
  }
  /** One line for the person: under the title in «Все приложения» or a folder, else the status line. */
  var hintTimer = 0;
  function note(text) {
    if (mode === "folder") {
      foldHint.textContent = text;
      clearTimeout(hintTimer);
      hintTimer = setTimeout(function () { foldHint.textContent = fold.hintText(); }, 4000);
      return;
    }
    if (mode !== "all") { setStatus(text); return; }
    allHint.textContent = text;
    clearTimeout(hintTimer);
    hintTimer = setTimeout(allHintText, 4000);
  }
  function allHintText() { allHint.textContent = editing ? M.editHint : M.holdHint; }

  function loadApps() {
    if (!hasTizen) return;
    try {
      tizen.application.getAppsInfo(function (list) {
        devSend(list);
        var got = [];
        for (var i = 0; i < list.length; i++) {
          var a = list[i];
          if (L.wanted(a)) got.push({ id: a.id, name: L.appName(a, lang), iconPath: a.iconPath || "" });
        }
        write(K.apps, L.compact(got));
        setApps(got, true);
      }, function (e) { if (!apps.length) setStatus(L.fmt(S.appsFailed, e.message), true); });
    } catch (e) { if (!apps.length) setStatus(L.fmt(S.appsFailed, e.message), true); }
  }

  function launch(a) {
    if (!a) return;
    if (a.folder) { fold.open(a); return; }
    if (a.more) { fold.openHidden(); return; }
    coachDone();
    // Samsung's Settings open over ReTV: no return to watch, not a recent app.
    var overlay = L.overlayApp(a.id);
    // Sent before the launch: the page may be paused as soon as the app is in front. The service waits up to 20 s
    // for the app to show; its errors (no service yet, an old firmware) are not the person's business here.
    if (returnOn() && !overlay) svcCall("watchReturn", { appId: a.id }, 15000).then(null, quiet);
    function failed(e) {
      setStatus(L.fmt(S.openFailed, (e && e.message) || a.name));
      // A Samsung app that never opened on this TV: counted; the second failure hides it («Скрытые» brings it back).
      setBook(L.bookFail(book, a.id));
    }
    try {
      tizen.application.launch(a.id, function () {
        setBook(L.bookOk(book, a.id));
        if (overlay) return;
        lastOpened = a.id;
        recent = L.pushRecent(recent, a.id, 30);
        write(K.recent, recent);
      }, failed);
    } catch (e) { failed(e); }
  }

  // --- Hero: the focused app, large; at most one change per frame however fast the keys repeat ---
  var heroWant = null, heroShown, heroRaf = 0;
  function showHero(a, now) {
    heroWant = a;
    if (now) { drawHero(); return; }
    if (!heroRaf) heroRaf = requestAnimationFrame(drawHero);
  }
  function drawHero() {
    heroRaf = 0;
    var a = heroWant;
    if (a === heroShown) return;
    heroShown = a;
    if (!a) { heroArt.style.visibility = "hidden"; heroName.textContent = ""; return; }
    heroMinis.textContent = "";
    if (a.folder) {
      // A folder: up to 4 of its apps large, its name under them.
      for (var k = 0; k < a.apps.length && k < 4; k++) heroMinis.appendChild(mini(a.apps[k], "hm"));
      heroArt.style.visibility = ""; heroArt.style.background = ""; heroArt.style.color = ""; heroArt.style.boxShadow = "";
      heroLabel.textContent = ""; heroIcon.style.backgroundImage = "none";
      heroName.textContent = a.name;
      return;
    }
    var b = L.brandOf(a, lang);
    heroArt.style.visibility = "";
    heroArt.style.background = b ? b[0] : "";
    heroArt.style.color = b ? b[1] : "";
    heroArt.style.boxShadow = b && b[4] ? "inset 0 0 0 6px " + b[4] : "";
    heroLabel.textContent = b ? b[2] : "";
    heroIcon.style.backgroundImage = b ? "none" : cssUrl(a.iconPath);
    heroName.textContent = a.name;
  }

  // --- Focus: one ring per surface, moved with a transform; the focused tile scales by a class ---
  function ringAt(ring, x, y) { ring.style.transform = "translate3d(" + x + "px," + y + "px,0) scale(1.1)"; }
  // The home screen's places besides the dock: the top line (chip, the DNS card when it shows, the gear, as ReTV
  // Home on Android: chip → banner → gear) and «Все приложения» under the dock.
  var ZONES = { chip: chip, warn: warn, gear: gear, allBtn: allBtn };
  function unfocusHome() {
    var t = dock.children[dockI];
    if (t && t !== dockRing) t.classList.remove("f");
    for (var z in ZONES) ZONES[z].classList.remove("f");
  }
  function focusDock(i, now) {
    unfocusHome();
    var tiles = dock.children;
    zone = "dock"; dockI = i;
    if (tiles[i]) tiles[i].classList.add("f");
    dockRing.style.visibility = "";
    ringAt(dockRing, INSET + i * PITCH, PAD_V);
    showHero(docked[i], now);
    if (coachOn) coachAt(i);
  }
  function focusZone(z) {
    unfocusHome();
    zone = z;
    dockRing.style.visibility = "hidden";
    ZONES[z].classList.add("f");
  }
  function focusChip() { focusZone("chip"); }
  /** «Все приложения ▼» under the dock: a button of its own, so the list can be found by moving down. */
  function focusAllBtn() { focusZone("allBtn"); }
  /** The top line from left to right, the DNS card only while it shows. */
  function topLine() { return warnOn ? ["chip", "warn", "gear"] : ["chip", "gear"]; }
  function stepTop(delta) {
    var line = topLine(), i = line.indexOf(zone) + delta;
    if (i >= 0 && i < line.length) focusZone(line[i]);
  }

  // «Верните DNS на автоматический» on the home screen too (dns.js decides when): one press opens the steps.
  var warnOn = false;
  function showWarn() {
    var on = dns.revertNow();
    if (on === warnOn) return;
    warnOn = on;
    warn.style.visibility = on ? "" : "hidden";
    if (!on && zone === "warn" && mode === "home") focusChip();
  }

  // The coach mark over the focused dock tile at the first start: «Удерживайте OK на приложении: меню». It goes for
  // good when the app menu opens, an app opens, or after COACH_MS; never shown to someone who has used the menu.
  var coachOn = false, coachChecked = false, coachTimer = 0;
  function coachAt(i) { coachEl.style.transform = "translate3d(" + (parseFloat(dock.style.left) + INSET + i * PITCH + TW / 2) + "px,0,0) translateX(-50%)"; }
  /** Once per start, as soon as the dock has tiles (from the cache, or the TV's first list on an empty cache). */
  function coachStart() {
    if (coachChecked || mode !== "home" || !dockCount()) return;
    coachChecked = true;
    if (read(K.coach, false) || menuUses > 0) return;
    coachOn = true;
    coachEl.style.visibility = "";
    coachAt(zone === "dock" ? dockI : 0);
    coachTimer = setTimeout(coachDone, L.COACH_MS);
  }
  function coachDone() {
    if (!coachOn) return;
    coachOn = false;
    clearTimeout(coachTimer);
    coachEl.style.visibility = "hidden";
    write(K.coach, true);
  }

  // Moving a dock tile («Переместить»): ← → swap it with its neighbour, OK or Back ends; the ring turns blue.
  function startMove(a) {
    var i = dockList().indexOf(a.id);
    if (i < 0) return;
    moving = a.id;
    dockRing.classList.add("mv");
    focusDock(i, true);
    setStatus(M.moving, true);
  }
  function stepMove(delta) {
    var ids = L.dockMove(dockList(), moving, delta), i = ids.indexOf(moving);
    if (i === dockI) return;
    setDock(ids);
    focusDock(i, true);
  }
  function endMove() {
    moving = null;
    dockRing.classList.remove("mv");
    setStatus("");
  }

  function renderGrid() {
    if (gridSig === layoutGen) return;
    gridSig = layoutGen;
    var f = document.createDocumentFragment();
    for (var i = 0; i < gl.length; i++) {
      var t = tile(gl[i], (i % COLS) * GX, Math.floor(i / COLS) * GY);
      if (inDock(gl[i].id)) t.className += " in";
      f.appendChild(t);
    }
    clearTiles(grid, gridRing);
    grid.insertBefore(f, gridRing);
  }
  /** The «in the dock» marks after an edit (the grid itself is not rebuilt). */
  function markGrid() {
    if (gridSig === null) return;
    var tiles = grid.children;
    for (var i = 0; i < gl.length; i++) if (tiles[i]) tiles[i].classList.toggle("in", inDock(gl[i].id));
  }
  function focusGrid(i) {
    if (i < 0) return;
    var tiles = grid.children;
    if (tiles[gridI]) tiles[gridI].classList.remove("f");
    allEdit.classList.remove("f");
    allZone = "grid";
    gridRing.style.visibility = "";
    gridI = i;
    if (tiles[i]) tiles[i].classList.add("f");
    ringAt(gridRing, (i % COLS) * GX, Math.floor(i / COLS) * GY);
    var row = Math.floor(i / COLS), top = gridTop;
    if (row < top) top = row; else if (row >= top + ROWS_SHOWN) top = row - ROWS_SHOWN + 1;
    if (top !== gridTop) { gridTop = top; grid.style.transform = "translate3d(0," + (-top * GY) + "px,0)"; }
  }
  function focusEdit() {
    var t = grid.children[gridI];
    if (t && t !== gridRing) t.classList.remove("f");
    allZone = "edit";
    gridRing.style.visibility = "hidden";
    allEdit.classList.add("f");
  }
  /** «Изменить док» in «Все приложения»: OK adds the focused app to the dock or removes it. */
  function setEditing(on) {
    editing = on;
    all.classList.toggle("edit", on);
    allEdit.textContent = on ? M.editDone : M.edit;
    clearTimeout(hintTimer);
    allHintText();
  }
  function openAll(edit) {
    if (!gl.length) return;
    renderGrid();
    mode = "all";
    setEditing(!!edit);
    all.classList.add("on");
    var i = zone === "dock" && docked[dockI] ? gl.indexOf(docked[dockI]) : 0;
    focusGrid(Math.max(0, Math.min(i, gl.length - 1)));
  }
  function closeAll() {
    mode = "home";
    setEditing(false);
    all.classList.remove("on");
    // The dock may have changed meanwhile.
    if (zone === "dock") refocusHome();
  }
  function gridOk() {
    var a = gl[gridI];
    if (!a) return;
    if (!editing || a.more) { launch(a); return; }
    if (inDock(a.id)) unpin(a); else pin(a);
  }

  // --- An open folder, or «Скрытые приложения» (folders.js) ---
  var fold = window.RetvFolders({
    $: $, F: F, tile: tile, clearTiles: clearTiles, ringAt: ringAt, GX: GX, GY: GY,
    mode: function () { return mode; }, setMode: function (m) { mode = m; },
    menuBack: function () { return menu.back; }, setMenuBack: function (m) { menu.back = m; },
    items: function () { return items; }, hiddenAll: hiddenAll, launch: launch,
    openMenu: function (a, from) { openMenu(a, from); }, openPanel: function () { openPanel(); },
    // OK in «Скрытые»: the app is back on the home screen; one this TV would not open gets another try.
    unhide: function (a) {
      if (a.blocked) setBook(L.bookForgive(book, a.id));
      else setLayout(L.unhideApp(layoutNow(), a.id));
      note(L.fmt(M.unhidden, a.name));
    },
    afterClose: function () { if (mode === "all") { renderGrid(); focusGrid(Math.max(0, Math.min(gridI, gl.length - 1))); } }
  });
  // --- The app menu: OK held on a tile (or the remote's Tools key), as ReTV Home on Android ---
  var menu = { app: null, from: "dock", back: "home", items: [], i: 0, top: 0 };
  var HINTS = { move: M.moveHint, unpin: M.unpinHint, edit: M.editDockHint, hide: M.hideHint, dissolve: M.dissolveHint };
  /** The menu sheet: [title] and its rows ({ key, label, sub, value }). */
  function showMenu(title, rows) {
    var f = document.createDocumentFragment();
    menu.items = []; menu.i = 0; menu.top = 0;
    for (var k = 0; k < rows.length; k++) {
      var r = rows[k], e = el("div", "item");
      e.appendChild(el("b", "", r.label));
      if (r.sub) e.appendChild(el("span", "", r.sub));
      menu.items.push({ el: e, key: r.key, value: r.value });
      f.appendChild(e);
    }
    $("menuTitle").textContent = title;
    menuBody.textContent = "";
    menuBody.style.transform = "";
    menuBody.appendChild(f);
    focusMenu(0);
    mode = "menu";
    menuEl.classList.add("on");
  }
  function openMenu(a, from) {
    if (!a || a.more) return;
    coachDone();
    menu.app = a; menu.from = from; menu.back = mode;
    var keys = L.menuKeys(from, inDock(a.id), dockCount(), !!a.folder);
    showMenu(a.name, keys.map(function (key) {
      return { key: key, label: M[key === "edit" ? "editDock" : key], sub: HINTS[key] || "" };
    }));
    if (menuUses < L.HINT_USES) { menuUses++; write(K.menuUses, menuUses); showHint(); }
  }
  /** «В папку»: «Новая папка» or one of the folders. */
  function folderPicker(a) {
    var rows = [{ key: "newFolder", label: M.folderNew, sub: "" }];
    items.forEach(function (it) {
      if (it.folder) rows.push({ key: "intoFolder", value: it.id, label: it.name, sub: L.plural(it.apps.length, F.n, lang) });
    });
    menu.app = a;
    showMenu(M.folderPick + ": " + a.name, rows);
  }
  /** «Переименовать»: «Папка» and the preset names, in the TV's language (no on-screen keyboard); ✓ the current one. */
  function renamePicker(folder) {
    var cur = folder.stored, rows = [{ key: "name", value: null, label: F.defaultName, sub: cur == null ? "✓" : "" }];
    L.FOLDER_PRESETS.forEach(function (k) {
      var v = L.PRESET_PREFIX + k;
      rows.push({ key: "name", value: v, label: F.presets[k], sub: cur === v ? "✓" : "" });
    });
    menu.app = folder;
    showMenu(M.renameTitle, rows);
  }
  function focusMenu(i) {
    var its = menu.items;
    if (its[menu.i]) its[menu.i].el.classList.remove("f");
    menu.i = Math.max(0, Math.min(its.length - 1, i));
    var e = its[menu.i] && its[menu.i].el;
    if (!e) return;
    e.classList.add("f");
    // A long list (the folder names) scrolls by a transform within its box.
    var top = menu.top, view = MENU_VIEW_H;
    if (e.offsetTop < top) top = e.offsetTop;
    else if (e.offsetTop + e.offsetHeight > top + view) top = e.offsetTop + e.offsetHeight - view;
    if (top !== menu.top) { menu.top = top; menuBody.style.transform = "translate3d(0," + (-top) + "px,0)"; }
  }
  var MENU_VIEW_H = 640;
  function closeMenu() { mode = menu.back; menuEl.classList.remove("on"); }
  function menuAct() {
    var it = menu.items[menu.i], a = menu.app;
    if (!it) return;
    // The pickers replace the menu's rows; everything else closes it first.
    if (it.key === "toFolder") { folderPicker(a); return; }
    if (it.key === "rename") { renamePicker(a); return; }
    closeMenu();
    var l = layoutNow();
    if (it.key === "open") launch(a);
    else if (it.key === "move") startMove(a);
    else if (it.key === "pin") pin(a);
    else if (it.key === "unpin") unpin(a);
    else if (it.key === "edit") openAll(true);
    else if (it.key === "hide") { setLayout(L.hideApp(l, a.id)); note(L.fmt(M.hidden, a.name)); }
    else if (it.key === "outFolder") { setLayout(L.removeFromFolder(l, a.id)); note(L.fmt(M.outOf, a.name)); }
    else if (it.key === "dissolve") setLayout(L.dissolveFolder(l, a.id));
    else if (it.key === "name") setLayout(L.renameFolder(l, a.id, it.value));
    else if (it.key === "newFolder") {
      var made = L.newFolder(l, a.id);
      setLayout(made.layout);
      note(L.fmt(M.movedTo, F.defaultName, a.name));
    } else if (it.key === "intoFolder") {
      setLayout(L.addToFolder(l, it.value, a.id));
      note(L.fmt(M.movedTo, folderName(it.value), a.name));
    }
  }
  function folderName(id) { for (var i = 0; i < items.length; i++) if (items[i].id === id) return items[i].name; return F.defaultName; }

  // --- Account: the server's TV sign-in, as ReTV Home on Android (TvAccount.kt) ---
  var acct = { token: read(K.token, null), cell: L.cellOf(read(K.cell, null)), ent: read(K.ent, null), label: read(K.label, null), dns: read(K.dns, null) };
  if (!acct.token || !acct.cell) acct = { token: null, cell: null, ent: null, label: null, dns: null };
  function claims() { return acct.ent ? L.claimsOf(acct.ent) : null; }
  function nowSec() { return Math.floor(Date.now() / 1000); }
  function plan() { return L.planOf(claims(), nowSec()); }

  function deviceKey() {
    var k = read(K.key, null);
    if (L.isDeviceKey(k)) return k;
    var b = new Uint8Array(32);
    try { crypto.getRandomValues(b); } catch (e) { for (var i = 0; i < 32; i++) b[i] = Math.floor(Math.random() * 256); }
    k = L.hex(b);
    write(K.key, k);
    return k;
  }
  var dev = null;
  function device() {
    if (dev) return dev;
    var model = "";
    try { model = webapis.productinfo.getRealModel(); } catch (e) {}
    if (!model) try { model = webapis.productinfo.getModel(); } catch (e) {}
    if (!model) try { model = tizen.systeminfo.getCapability("http://tizen.org/system/model_name"); } catch (e) {}
    model = String(model || "").trim().slice(0, 80) || "Samsung TV";
    var d = { model: model, name: (/^samsung/i.test(model) ? model : "Samsung " + model).slice(0, 80) };
    // Kept once Samsung's webapis answered (they load after the first paint).
    if (typeof webapis !== "undefined") dev = d;
    return d;
  }
  function timeZone() {
    try { return tizen.time.getLocalTimezone(); } catch (e) {}
    try { return Intl.DateTimeFormat().resolvedOptions().timeZone || ""; } catch (e) { return ""; }
  }

  /** POST to a cell's API; [cb] gets the status (0 offline or timeout) and the parsed JSON body (or null). */
  function api(cell, path, body, bearer, cb) { send("POST", cell, path, body, bearer, cb); }
  function apiGet(cell, path, bearer, cb) { send("GET", cell, path, null, bearer, cb); }
  function send(method, cell, path, body, bearer, cb) {
    var x = new XMLHttpRequest(), done = false;
    function fin() {
      if (done) return;
      done = true;
      var data = null;
      try { data = JSON.parse(x.responseText); } catch (e) {}
      if (cb) cb(x.status, data);
    }
    try {
      x.open(method, L.SITES[cell] + "/v1" + path);
      x.timeout = 15000;
      x.setRequestHeader("Accept", "application/json");
      if (body) x.setRequestHeader("Content-Type", "application/json");
      if (bearer) x.setRequestHeader("Authorization", "Bearer " + bearer);
      x.onload = fin; x.onerror = fin; x.ontimeout = fin;
      x.send(body ? JSON.stringify(body) : null);
    } catch (e) { fin(); }
  }

  var sign = null; // the sign-in under way: { cell, code, deviceCode, interval, until, msg, ended, timer }
  var notice = "";  // one line after a sign-in, until the panel closes
  var lastRefresh = 0, armedOut = 0;

  function failText(o) { return o === "offline" ? S.offline : o === "rate" ? S.rateLimited : o === "expired" ? S.expired : S.serverError; }
  function stopSignIn() { if (sign) clearTimeout(sign.timer); sign = null; }

  function startSignIn() {
    stopSignIn();
    var dv = device(), s = { cell: L.marketOf(navigator.language, timeZone()), code: "", msg: "", ended: false, timer: 0 };
    sign = s;
    renderPanel();
    api(s.cell, "/tv/codes", { app: "home", name: dv.name, model: dv.model, appVersion: WIRE_VERSION, deviceKey: deviceKey() }, null, function (st, d) {
      if (sign !== s) return;
      if (st === 200 && d && d.userCode && d.deviceCode) {
        s.code = d.userCode; s.deviceCode = d.deviceCode;
        s.interval = Math.max(5, d.interval || 5);
        s.until = Date.now() + (d.expiresIn || 600) * 1000;
        s.msg = S.wait;
        schedulePoll(s, s.interval);
      } else { s.msg = failText(L.pollOutcome(st, d && d.error)); s.ended = true; }
      renderPanel();
    });
  }
  function schedulePoll(s, sec) { s.timer = setTimeout(function () { poll(s); }, sec * 1000); }
  function signMsg(s, text, ended) {
    s.msg = text; s.ended = !!ended;
    if (ended) renderPanel(); else { var m = $("signMsg"); if (m) m.textContent = text; }
  }
  function poll(s) {
    if (sign !== s) return;
    if (Date.now() > s.until) { signMsg(s, S.expired, true); return; }
    api(s.cell, "/tv/token", { deviceCode: s.deviceCode, deviceKey: deviceKey() }, null, function (st, d) {
      if (sign !== s) return;
      var o = L.pollOutcome(st, d && d.error);
      if (o === "done") {
        if (d && d.deviceToken && d.entitlementToken) { signedIn(s.cell, d); return; }
        o = "server";
      }
      if (o === "expired") { signMsg(s, S.expired, true); return; }
      if (o === "slow") s.interval += 5;
      signMsg(s, o === "pending" || o === "slow" ? S.wait : failText(o));
      schedulePoll(s, o === "rate" ? Math.max(60, s.interval) : s.interval);
    });
  }
  function signedIn(cell, d) {
    var c = L.claimsOf(d.entitlementToken);
    acct = { token: d.deviceToken, cell: L.cellOf(c && c.cell) || cell, ent: d.entitlementToken, label: d.accountLabel || null, dns: null };
    saveAcct();
    stopSignIn();
    var p = plan();
    notice = p === "plus" ? S.donePlus : p === "pro" ? S.done : S.doneFree;
    lastRefresh = Date.now();
    accountChanged();
  }
  function saveAcct() { write(K.token, acct.token); write(K.cell, acct.cell); write(K.ent, acct.ent); write(K.label, acct.label); write(K.dns, acct.dns); }
  function forget() { acct = { token: null, cell: null, ent: null, label: null, dns: null }; saveAcct(); dns.forgotten(); accountChanged(); }

  /** A fresh entitlement when the token is due (past `rfa`) or [force]d; at most once a minute. */
  function refresh(force) {
    if (!acct.token) return;
    var c = claims(), now = Date.now();
    if (!force && c && c.rfa && c.rfa * 1000 > now) return;
    if (now - lastRefresh < 60000) return;
    lastRefresh = now;
    var token = acct.token;
    api(acct.cell, "/tv/entitlement", { appVersion: WIRE_VERSION, deviceKey: deviceKey() }, token, function (st, d) {
      if (acct.token !== token) return;
      if (st === 200 && d && d.entitlementToken) {
        acct.ent = d.entitlementToken;
        if ("accountLabel" in d) acct.label = d.accountLabel || null;
        // «DNS без рекламы»: { host, on, proEnded? } once the account has a DNS hostname (absent before that).
        var n = d.dns;
        acct.dns = n && typeof n === "object" && typeof n.host === "string" ? { host: n.host, on: n.on === true, proEnded: n.proEnded === true } : null;
        saveAcct();
        accountChanged();
        dns.report(false);
      } else if (st === 401 && d && d.error === "revoked") forget();
    });
  }
  function signOut() {
    if (acct.token) api(acct.cell, "/tv/sign-out", null, acct.token, null);
    notice = "";
    forget();
  }
  function accountChanged() {
    chipPro.textContent = plan() === "plus" ? S.plus : S.pro;
    chipPro.style.display = plan() === "free" ? "none" : "";
    if (mode === "panel") renderPanel();
    if (mode === "opt") optAccountChanged();
    showWarn();
  }

  // --- Settings panel ---
  var rows = [], itemI = 0;   // the panel's rows (not the layout's items)
  function item(title, sub, act) {
    var e = el("div", "item");
    e.appendChild(el("b", "", title));
    if (sub) e.appendChild(el("span", "", sub));
    rows.push({ el: e, act: act });
    return e;
  }
  function renderPanel() {
    var f = document.createDocumentFragment(), site = L.SITES[sign ? sign.cell : L.marketOf(navigator.language, timeZone())].replace(/^https:\/\//, "") + "/go";
    rows = [];
    var acc = el("div", "acc");
    if (acct.token) {
      acc.appendChild(el("b", "", L.fmt(S.account, L.labelText(acct.label, lang) || "ReTV")));
      acc.appendChild(el("span", "", L.planText(claims(), nowSec(), lang)));
    } else acc.appendChild(el("b", "", S.notSignedIn));
    f.appendChild(acc);
    if (sign) {
      var box = el("div", "sign");
      box.appendChild(el("p", "", L.fmt(S.signInBody, site)));
      box.appendChild(el("div", "code", sign.code ? L.codeText(sign.code) : "····-····"));
      var m = el("p", "msg", sign.msg); m.id = "signMsg";
      box.appendChild(m);
      f.appendChild(box);
      f.appendChild(item(S.newCode, "", startSignIn));
    } else if (acct.token) {
      if (notice) f.appendChild(el("p", "msg", notice));
      f.appendChild(item(S.signOut, S.signOutSub, function (e) {
        if (Date.now() - armedOut < 5000) { armedOut = 0; signOut(); return; }
        armedOut = Date.now();
        e.lastChild.textContent = S.signOutAgain;
      }));
    } else {
      f.appendChild(item(S.signIn, L.fmt(S.signInSub, site), startSignIn));
    }
    f.appendChild(item(O.item, O.itemSub, openOpt));
    f.appendChild(item(O.returnItem, returnOn() ? O.on : O.off, function (e) {
      var on = !returnOn();
      write(K.ret, on);
      e.lastChild.textContent = on ? O.on : O.off;
      svcCall("setReturnToReTV", { on: on }, 15000).then(null, quiet);
    }));
    f.appendChild(el("p", "ver", L.fmt(S.version, VERSION) + " · " + device().model));
    panelBody.textContent = "";
    panelBody.appendChild(f);
    focusItem(Math.min(itemI, rows.length - 1));
  }
  function focusItem(i) {
    if (rows[itemI]) rows[itemI].el.classList.remove("f");
    itemI = Math.max(0, i);
    if (rows[itemI]) rows[itemI].el.classList.add("f");
  }
  function openPanel() {
    if (mode === "folder") fold.close();
    if (mode === "all") closeAll();
    mode = "panel";
    itemI = 0;
    renderPanel();
    panel.classList.add("on");
    refresh(true);
  }
  function closePanel() {
    stopSignIn();
    notice = ""; armedOut = 0;
    mode = "home";
    panel.classList.remove("on");
  }

  // --- The on-TV service (service/dist/client.js, PROTOCOL.md): loaded only when something needs it ---
  var svcP = null;
  function quiet() {}
  function service() {
    if (svcP) return svcP;
    svcP = new Promise(function (resolve, reject) {
      function make() {
        try {
          var c = window.RetvService.create();
          // The page's switch is the truth: pushed on every (re)connection (the service keeps its own across restarts).
          if (c.on) c.on("connect", function () { c.call("setReturnToReTV", { on: returnOn() }, 15000).then(null, quiet); });
          resolve(c);
        } catch (e) { reject(e); }
      }
      if (window.RetvService) { make(); return; }
      if (!hasTizen) { reject(new Error("not a Samsung TV")); return; }
      var s = document.createElement("script");
      s.src = "service/dist/client.js";
      s.onload = function () { if (window.RetvService) make(); else reject(new Error("no client")); };
      s.onerror = function () { reject(new Error("no client")); };
      document.body.appendChild(s);
    });
    svcP.then(null, function () { svcP = null; });
    return svcP;
  }
  function svcCall(cmd, args, ms) { return service().then(function (s) { return s.call(cmd, args, ms); }); }
  function returnOn() { return read(K.ret, true) !== false; }

  // --- «Ускорить и очистить ТВ»: a full-screen layer with three cards; OK on a card opens its page ---
  var PRIVACY_APP = "tv6pbVMQFF.PrivacyChoices", PAGE_H = 840, VIEWS = ["privacy", "apps", "dns"];
  var optEl = $("opt"), optCards = $("optCards"), optPage = $("optPage"), optIn = $("optIn");
  // home: the DNS page was opened from the home screen's card, so Back goes home.
  var opt = { view: "cards", card: 0, items: [], i: 0, top: 0, busy: false, gen: 0, panelI: 0, calmUntil: 0, home: false };
  var pv = { step: "intro", lat: null, error: "" };                 // privacy page
  var av = { step: "idle", rows: [], guide: null, error: "", progress: "", result: null, btn: null }; // apps page
  var beforeN = read(K.before, null);                               // how many apps the «как было» list holds
  $("optTitle").textContent = O.title;

  function readLat() { try { return webapis.adinfo.isLATEnabled(); } catch (e) { return null; } }
  function siteOf() { return L.SITES[acct.cell || L.marketOf(navigator.language, timeZone())].replace(/^https:\/\//, ""); }
  function titleOf(view) { return view === "privacy" ? O.privacy : view === "apps" ? O.apps : O.dns; }
  function rerender(view) { if (mode === "opt" && opt.view === view) renderOpt(); }
  function steps(list) {
    var ol = el("ol", "steps");
    for (var i = 0; i < list.length; i++) ol.appendChild(el("li", "", list[i]));
    return ol;
  }
  function optItem(title, sub, act) {
    var e = el("div", "item");
    e.appendChild(el("b", "", title));
    if (sub) e.appendChild(el("span", "", sub));
    opt.items.push({ el: e, act: act });
    return e;
  }

  function cardLines(k) {
    if (k === 0) {
      var at = read(K.privacyAt, 0), out = [L.adsText(readLat(), lang), O.viewing];
      if (at) out.push(L.fmt(O.doneAt, L.dayText(new Date(at), lang)));
      return out;
    }
    if (k === 1) return typeof beforeN === "number" ? [O.appsSub, L.fmt(O.beforeSaved, L.appsText(beforeN, lang))] : [O.appsSub];
    return [dns.cardLine()];
  }
  function renderCards() {
    var f = document.createDocumentFragment();
    for (var k = 0; k < 3; k++) {
      var c = el("div", "card" + (k === opt.card ? " f" : ""));
      c.appendChild(el("h3", "", titleOf(VIEWS[k])));
      var lines = cardLines(k);
      for (var j = 0; j < lines.length; j++) c.appendChild(el("p", "", lines[j]));
      c.appendChild(el("p", "go", O.open));
      f.appendChild(c);
    }
    optCards.textContent = "";
    optCards.appendChild(f);
  }
  function focusCard(k) {
    var cs = optCards.children;
    if (cs[opt.card]) cs[opt.card].classList.remove("f");
    opt.card = k;
    if (cs[k]) cs[k].classList.add("f");
  }

  function renderOpt() {
    if (opt.view === "cards") {
      optPage.style.visibility = "hidden";
      optCards.style.visibility = "";
      renderCards();
      return;
    }
    opt.items = [];
    var f = document.createDocumentFragment();
    f.appendChild(el("h3", "", titleOf(opt.view)));
    if (opt.view === "privacy") privacyPage(f); else if (opt.view === "apps") appsPage(f); else dns.page(f);
    optIn.textContent = "";
    optIn.appendChild(f);
    optCards.style.visibility = "hidden";
    optPage.style.visibility = "";
    opt.top = -1;
    focusOptItem(Math.min(opt.i, opt.items.length - 1));
  }
  /** Focus an item; the page scrolls (a transform) so that it is in view. */
  function focusOptItem(i) {
    var its = opt.items;
    if (its[opt.i]) its[opt.i].el.classList.remove("f");
    opt.i = Math.max(0, i);
    var it = its[opt.i], top = Math.max(0, opt.top);
    if (it) {
      it.el.classList.add("f");
      var y = it.el.offsetTop, h = it.el.offsetHeight;
      if (opt.i === 0 && y + h + 40 <= PAGE_H) top = 0;
      else if (y - 40 < top) top = Math.max(0, y - 40);
      else if (y + h + 40 > top + PAGE_H) top = y + h + 40 - PAGE_H;
    } else top = 0;
    if (top !== opt.top) { opt.top = top; optIn.style.transform = "translate3d(0," + (-top) + "px,0)"; }
  }

  function openOpt() {
    opt.panelI = itemI;
    closePanel();
    mode = "opt";
    opt.home = false;
    opt.view = "cards";
    renderOpt();
    optEl.classList.add("on");
    refresh(false);
  }
  function closeOpt() {
    opt.gen++;
    optEl.classList.remove("on");
    openPanel();
    focusItem(opt.panelI);
  }
  function openView(k) {
    opt.view = VIEWS[k];
    opt.i = 0;
    if (opt.view === "privacy") { pv = { step: "intro", lat: null, error: "" }; renderOpt(); }
    else if (opt.view === "apps") appsLoad();
    else { dns.opened(); renderOpt(); dns.report(true); refresh(true); }
  }
  /** OK on the home screen's DNS card: the DNS page at once; Back there comes home. */
  function openDnsFix() {
    mode = "opt";
    opt.home = true;
    opt.gen++;
    opt.view = "dns";
    opt.i = 0;
    dns.opened();
    renderOpt();
    optEl.classList.add("on");
    refresh(true);
  }
  function closeOptHome() {
    opt.gen++;
    opt.home = false;
    opt.view = "cards";
    optEl.classList.remove("on");
    mode = "home";
    showWarn();
    if (zone === "warn") { if (warnOn) focusZone("warn"); else focusChip(); }
  }
  function optBack() {
    if (opt.busy) return;
    if (opt.view === "apps" && av.step === "confirm") { av.step = "list"; opt.i = 0; renderOpt(); return; }
    if (opt.home) { closeOptHome(); return; }
    opt.gen++;
    if (pv.step === "waiting") pv.step = "intro";
    opt.view = "cards";
    renderOpt();
  }
  function optVisible() {
    if (pv.step === "waiting") privacyBack();
    else if (opt.view === "cards" || opt.view === "dns") renderOpt();   // back from Samsung's Settings: DNS may be automatic now
  }
  function optAccountChanged() {
    if (opt.view === "cards") renderCards();
    else if (opt.view === "dns") { renderOpt(); dns.report(true); }
  }

  // Слежка и реклама: Samsung's own screen (the TV does not let apps press its keys); then the honest result.
  function privacyPage(f) {
    if (pv.step === "result") {
      var s = L.adsState(pv.lat);
      if (s === "off") {
        f.appendChild(el("p", "lead ok", O.privacyOk));
        f.appendChild(el("p", "", O.privacyOkSub));
        f.appendChild(optItem(O.done, "", optBack));
        f.appendChild(optItem(O.tryAgain, "", openPrivacyItem));
        return;
      }
      f.appendChild(el("p", "lead", s === "on" ? O.privacyStill : O.privacyUnknown));
      if (s === "on") f.appendChild(el("p", "", O.privacyStillSub));
      f.appendChild(steps(O.privacySteps));
      f.appendChild(optItem(O.tryAgain, "", openPrivacyItem));
      f.appendChild(optItem(O.back, "", optBack));
      return;
    }
    f.appendChild(el("p", "lead", pv.step === "waiting" ? O.waiting : L.adsText(readLat(), lang)));
    f.appendChild(el("p", "", O.privacyLead));
    f.appendChild(steps(O.privacySteps));
    if (pv.error) f.appendChild(el("p", "msg", pv.error));
    f.appendChild(optItem(O.openSamsung, "", openPrivacyItem));
    f.appendChild(optItem(O.back, "", optBack));
  }
  function openPrivacyItem() { openPrivacy(null); }
  /** Samsung's Privacy Choices; [done](opened) when the TV said whether it opened. */
  function openPrivacy(done) {
    var fail = function (e) {
      pv.step = "intro"; pv.error = L.fmt(O.privacyOpenFailed, (e && e.message) || "?"); rerender("privacy");
      if (done) done(false);
    };
    pv.error = "";
    pv.step = "waiting";
    rerender("privacy");
    // Samsung's «Exit» lands on Smart Hub: the return watch brings ReTV back, as after any app opened from here. Sent
    // before the launch: the page may be paused as soon as Samsung's screen is in front.
    if (returnOn()) svcCall("watchReturn", { appId: PRIVACY_APP }, 15000).then(null, quiet);
    try { tizen.application.launch(PRIVACY_APP, function () { if (done) done(true); }, fail); } catch (e) { fail(e); }
  }
  /**
   * Back from Samsung's screen: read the ad flag now and once more a moment later (it may settle after saving); the
   * settled flag goes to the account for ReTV Bridge, with Bridge's [check] id when Bridge asked.
   */
  function privacyBack(check) {
    function take(lat) { if (L.adsState(lat) === "off") write(K.privacyAt, Date.now()); return lat; }
    pv.lat = take(readLat());
    pv.step = "result";
    opt.i = 0;
    rerender("privacy");
    setTimeout(function () {
      var lat = take(readLat());
      reportPrivacy(lat, check || null);
      if (pv.step !== "result") return;
      var was = pv.lat;
      pv.lat = lat;
      if (lat !== was) rerender("privacy");
    }, 1500);
  }
  /** `POST /v1/tv/privacy` when signed in: ReTV Bridge on a computer reads the TV's ad flag from the account. */
  function reportPrivacy(lat, check, opened) {
    if (!acct.token) return;
    var token = acct.token;
    api(acct.cell, "/tv/privacy", L.privacyReport(lat, check, opened), token, function (st, d) {
      if (st === 401 && d && d.error === "revoked" && acct.token === token) forget();
    });
  }
  /** The privacy page in front, whatever was on screen (a removal of apps is never interrupted: see bridgeRequest). */
  function showPrivacy() {
    if (mode === "menu") closeMenu();
    if (moving) endMove();
    if (mode === "all") closeAll();
    if (mode === "panel") { opt.panelI = itemI; closePanel(); }
    else if (mode !== "opt") opt.panelI = 0;
    mode = "opt";
    opt.home = false;
    opt.gen++;
    opt.view = "privacy";
    opt.i = 0;
    optEl.classList.add("on");
  }
  // ReTV Bridge on a computer asks for a privacy round: "privacy" opens Privacy Choices at once when ads are on (Bridge
  // then presses the keys), "privacy-check" reads the flag again. Each answer goes to the account with the check id;
  // Bridge cannot read the TV's setting itself. Bridge leaves the request on the server and starts this app over sdb;
  // this app asks for it (requestPoll). The older way, the launch's PAYLOAD (Bridge's metaTag), is still read: the 2021
  // QE55Q67AA ignores the remote's ed.apps.launch altogether (2026-11-04), other TVs may pass it.
  var bridgeSeen = null;
  function bridgeRequest() {
    var data = null;
    try { data = tizen.application.getCurrentApplication().getRequestedAppControl().appControl.data; } catch (e) { return; }
    runBridge(L.bridgeRequest(data));
  }
  /** `GET /v1/tv/requests/pending` when signed in; the server hands each request out once. */
  function askServer() {
    if (!acct.token) return;
    var token = acct.token;
    apiGet(acct.cell, "/tv/requests/pending", token, function (st, d) {
      if (st === 401 && d && d.error === "revoked" && acct.token === token) { forget(); return; }
      if (st === 200 && acct.token === token) runBridge(L.serverRequest(d));
    });
  }
  // Asked at once, then every REQUEST_POLL_MS for REQUEST_POLL_FOR_MS: after a start, a return to the screen or a new
  // launch (Bridge leaves its request a moment before it starts this app, or while it is already in front).
  // After the burst it keeps asking every 3rd tick (6 s) while ReTV is on screen: on the 2021 TV, Bridge's sdb
  // `was_execute` of an app already in front fires no appcontrol and no visibilitychange (seen 2026-11-04), so a request
  // left then would wait for the next start. 6 s stays inside the server's 600 an hour per TV.
  var pollTimer = 0, pollUntil = 0, pollTick = 0;
  function requestPoll() {
    pollUntil = Date.now() + L.REQUEST_POLL_FOR_MS;
    askServer();
    if (pollTimer) return;
    pollTimer = setInterval(function () {
      if (document.hidden || !acct.token) return;
      pollTick++;
      if (Date.now() < pollUntil || pollTick % 3 === 0) askServer();
    }, L.REQUEST_POLL_MS);
  }
  function runBridge(r) {
    if (!r) return;
    var key = r.kind + ":" + (r.check || "");
    if (r.check && key === bridgeSeen) return;   // the same launch, seen at start and as appcontrol
    bridgeSeen = key;
    if (mode === "opt" && opt.busy) { reportPrivacy(readLat(), r.check, false); return; }
    showPrivacy();
    if (r.kind === "check") { pv = { step: "waiting", lat: null, error: "" }; privacyBack(r.check); return; }
    var lat = readLat();
    if (L.adsState(lat) !== "on") {
      pv = { step: "result", lat: lat, error: "" };
      renderOpt();
      reportPrivacy(lat, r.check, false);
      return;
    }
    pv = { step: "intro", lat: null, error: "" };
    renderOpt();
    // Reported before the launch: the page may be paused as soon as Samsung's screen is in front. Bridge waits a
    // moment and reads again: only a launch that failed (this page still in front) sends a second report.
    reportPrivacy(lat, r.check);
    openPrivacy(function (opened) { if (!opened) reportPrivacy(lat, r.check, false); });
  }

  // Лишние приложения: the service lists Smart Hub's apps with use counts and removes them over sdb.
  function svcError(e) {
    var code = e && e.code;
    return code && code !== "NO_SERVICE" && code !== "DISCONNECTED" && code !== "TIMEOUT" ? O.noService + " (" + code + ")" : O.noService;
  }
  function setBefore(b) {
    if (b && Array.isArray(b.apps)) { beforeN = b.apps.length; write(K.before, beforeN); }
  }
  function appsLoad() {
    var g = ++opt.gen;
    av = { step: "loading", rows: [], guide: null, error: "", progress: "", result: null, btn: null };
    opt.i = 0;
    rerender("apps");
    function guide(kind, ip) { av.step = "setup"; av.guide = L.setupGuide(kind, lang, ip); rerender("apps"); }
    svcCall("setup.check", {}, 30000).then(function (st) {
      if (g !== opt.gen) return;
      var kind = L.setupKind(st && st.state);
      if (kind) { guide(kind, st && st.developerIP); return; }
      return Promise.all([
        svcCall("apps.list", {}, 60000),
        svcCall("apps.before", {}, 15000).then(null, function () { return null; })
      ]).then(function (r) {
        if (g !== opt.gen) return;
        av.rows = L.cleanupRows(r[0], recent);
        setBefore(r[1]);
        av.step = "list";
        rerender("apps");
      });
    }).then(null, function (e) {
      if (g !== opt.gen) return;
      if (e && e.code === "SDB_DOWN") guide("devMode");
      else if (e && e.code === "SDB_DROPPED") guide("hostIp");
      else { av.step = "error"; av.error = svcError(e); rerender("apps"); }
    });
  }
  function ticked() { return av.rows.filter(function (r) { return r.tick; }); }
  function beforeLine(f) { if (typeof beforeN === "number") f.appendChild(el("p", "msg", L.fmt(O.beforeSaved, L.appsText(beforeN, lang)))); }
  function appRow(r) {
    var e = el("div", "item app" + (r.tick ? " on" : "")), t = el("div");
    e.appendChild(el("i", "box"));
    t.appendChild(el("b", "", r.title));
    t.appendChild(el("span", "", L.usesText(r.useCount, lang)));
    e.appendChild(t);
    opt.items.push({ el: e, act: function () {
      r.tick = !r.tick;
      e.classList.toggle("on", r.tick);
      if (av.btn) av.btn.firstChild.textContent = L.fmt(O.removeSelected, ticked().length);
    } });
    return e;
  }
  function appsPage(f) {
    var st = av.step;
    if (st === "loading") { f.appendChild(el("p", "lead", O.appsLoading)); f.appendChild(optItem(O.back, "", optBack)); return; }
    if (st === "error") {
      f.appendChild(el("p", "lead", av.error));
      f.appendChild(optItem(O.retry, "", appsLoad));
      f.appendChild(optItem(O.back, "", optBack));
      return;
    }
    if (st === "setup") {
      f.appendChild(el("p", "lead", av.guide.title));
      f.appendChild(el("p", "", av.guide.lead));
      if (av.guide.alt) f.appendChild(el("p", "", av.guide.alt));
      f.appendChild(steps(av.guide.steps));
      f.appendChild(optItem(O.retry, "", appsLoad));
      f.appendChild(optItem(O.back, "", optBack));
      return;
    }
    if (st === "confirm") {
      var sel = ticked(), names = sel.slice(0, 40).map(function (r) { return r.title; }).join(", ") + (sel.length > 40 ? ", …" : "");
      f.appendChild(el("p", "lead", L.fmt(O.confirmTitle, L.appsText(sel.length, lang))));
      f.appendChild(el("p", "", names));
      f.appendChild(el("p", "msg", O.reinstall));
      f.appendChild(optItem(O.confirmYes, "", runRemoval));
      f.appendChild(optItem(O.cancel, "", function () { av.step = "list"; opt.i = 0; opt.calmUntil = Date.now() + L.CONFIRM_CALM_MS; renderOpt(); }));
      return;
    }
    if (st === "removing") { f.appendChild(el("p", "lead", av.progress)); return; }
    if (st === "result") {
      var res = av.result;
      f.appendChild(el("p", "lead ok", L.fmt(O.removed, res.done.length)));
      if (res.done.length) f.appendChild(el("p", "", res.done.join(", ")));
      if (res.failed.length) f.appendChild(el("p", "", L.fmt(O.failed, res.failed.join(", "))));
      f.appendChild(el("p", "msg", O.reinstall));
      beforeLine(f);
      f.appendChild(optItem(O.done, "", appsLoad));
      return;
    }
    // The list
    if (!av.rows.length) {
      f.appendChild(el("p", "lead", O.noneToRemove));
      beforeLine(f);
      f.appendChild(optItem(O.back, "", optBack));
      return;
    }
    f.appendChild(el("p", "", O.appsHint));
    beforeLine(f);
    var btn = optItem(L.fmt(O.removeSelected, ticked().length), "", function (e) {
      if (!ticked().length) { e.firstChild.textContent = O.nothingSelected; return; }
      av.step = "confirm";
      opt.i = 1;                                // focus on «Отмена»: OK, OK never removes by itself
      opt.calmUntil = Date.now() + L.CONFIRM_CALM_MS;
      renderOpt();
    });
    av.btn = btn;
    f.appendChild(btn);
    for (var i = 0; i < av.rows.length; i++) f.appendChild(appRow(av.rows[i]));
  }
  function runRemoval() {
    var list = ticked(), i = 0, done = [], failed = [];
    opt.busy = true;
    function next() {
      if (i >= list.length) { finish(); return; }
      var r = list[i++];
      av.step = "removing";
      av.progress = L.fmt(O.removing, i, list.length, r.title);
      rerender("apps");
      svcCall("apps.uninstall", { id: r.id }, 180000).then(function () { done.push(r.title); }, function () { failed.push(r.title); }).then(next);
    }
    function finish() {
      opt.busy = false;
      av.step = "result";
      av.result = { done: done, failed: failed };
      opt.i = 0;
      opt.calmUntil = Date.now() + L.CONFIRM_CALM_MS;
      rerender("apps");                         // the result and «Готово» now; «как было» fills in when it comes
      svcCall("apps.before", {}, 15000).then(function (b) {
        var was = beforeN;
        setBefore(b);
        if (beforeN !== was && av.step === "result") rerender("apps");
      }, quiet);
      loadApps();
    }
    next();
  }

  // --- «DNS без рекламы» (dns.js); the service keeps the home network linked, the page asks it every minute ---
  var dns = window.RetvDns({
    L: L, O: O, K: K, read: read, write: write, el: el, api: api, steps: steps, optItem: optItem, optBack: optBack, siteOf: siteOf,
    acct: function () { return acct; }, plan: plan, forget: function () { forget(); }, svcCall: svcCall, rerender: rerender,
    openSettings: hasTizen ? function () { launch({ id: L.SETTINGS_ID, name: S.samsungSettings }); } : null,
    changed: function () { showWarn(); if (mode === "opt" && opt.view === "cards") renderCards(); }
  });
  setInterval(function () { dns.report(false); }, 60000);
  function keyOpt(k, repeat) {
    if (opt.view === "cards") {
      if (k === LEFT) { if (opt.card > 0) focusCard(opt.card - 1); }
      else if (k === RIGHT) { if (opt.card < 2) focusCard(opt.card + 1); }
      else if (k === OK) openView(opt.card);
      else if (k === BACK || k === ESC) closeOpt();
      else if (k !== UP && k !== DOWN && k !== RED) return false;
      return;
    }
    if (opt.busy) return;  // removals run: every key waits
    if (k === UP) focusOptItem(opt.i - 1);
    else if (k === DOWN) focusOptItem(Math.min(opt.items.length - 1, opt.i + 1));
    else if (k === OK) {
      if (!L.okAllowed(repeat, Date.now(), opt.calmUntil)) return;
      var it = opt.items[opt.i];
      if (it) it.act(it.el);
    }
    else if (k === BACK || k === ESC) optBack();
    else if (k !== LEFT && k !== RIGHT && k !== RED) return false;
  }

  // --- Keys ---
  var LEFT = 37, UP = 38, RIGHT = 39, DOWN = 40, OK = 13, BACK = 10009, ESC = 27, RED = 403, TOOLS = 10135;
  function keyHome(k) {
    if (moving) {
      if (k === LEFT) stepMove(-1);
      else if (k === RIGHT) stepMove(1);
      else if (k === OK || k === BACK || k === ESC) endMove();
      else if (k !== UP && k !== DOWN && k !== RED && k !== TOOLS) return false;
      return;
    }
    if (k === RED) { openPanel(); return; }
    if (zone === "chip" || zone === "warn" || zone === "gear") {
      // The top line: the chip and the gear open Settings, the DNS card its steps.
      if (k === OK) { if (zone === "warn") openDnsFix(); else openPanel(); }
      else if (k === LEFT) stepTop(-1);
      else if (k === RIGHT) stepTop(1);
      else if (k === DOWN) { if (dockCount()) focusDock(Math.min(dockI, dockCount() - 1)); else focusAllBtn(); }
      else if (k !== BACK && k !== ESC && k !== UP) return false;
      return;
    }
    if (zone === "allBtn") {
      if (k === OK || k === DOWN) openAll(false);
      else if (k === UP) { if (dockCount()) focusDock(Math.min(dockI, dockCount() - 1)); else focusChip(); }
      else if (k !== BACK && k !== ESC && k !== LEFT && k !== RIGHT) return false;
      return;
    }
    var n = dockCount();
    if (k === LEFT) { if (dockI > 0) focusDock(dockI - 1); }
    else if (k === RIGHT) { if (dockI < n - 1) focusDock(dockI + 1); }
    else if (k === UP) focusChip();
    else if (k === DOWN) focusAllBtn();
    else if (k === OK) launch(docked[dockI]);
    else if (k === TOOLS) openMenu(docked[dockI], "dock");
    else if (k !== BACK && k !== ESC) return false; // Back on the home screen stays here
  }
  function keyAll(k) {
    var n = gl.length, i = gridI;
    if (allZone === "edit") {
      if (k === OK) setEditing(!editing);
      else if (k === DOWN) focusGrid(gridI);
      else if (k === UP || k === BACK || k === ESC) closeAll();
      else if (k === RED) openPanel();
      else if (k !== LEFT && k !== RIGHT && k !== TOOLS) return false;
      return;
    }
    if (k === LEFT) { if (i % COLS) focusGrid(i - 1); }
    else if (k === RIGHT) { if (i % COLS < COLS - 1 && i < n - 1) focusGrid(i + 1); }
    else if (k === UP) { if (i >= COLS) focusGrid(i - COLS); else focusEdit(); }
    else if (k === DOWN) { if (i + COLS < n) focusGrid(i + COLS); else if (Math.floor(i / COLS) < Math.floor((n - 1) / COLS)) focusGrid(n - 1); }
    else if (k === OK) gridOk();
    else if (k === TOOLS) openMenu(gl[i], "all");
    else if (k === BACK || k === ESC) { if (editing) setEditing(false); else closeAll(); }
    else if (k === RED) openPanel();
    else return false;
  }
  var KEYS = { LEFT: LEFT, UP: UP, RIGHT: RIGHT, DOWN: DOWN, OK: OK, BACK: BACK, ESC: ESC, RED: RED, TOOLS: TOOLS };
  function keyFold(k) { return fold.key(k, KEYS); }
  function keyMenu(k, repeat) {
    if (k === UP) focusMenu(menu.i - 1);
    else if (k === DOWN) focusMenu(menu.i + 1);
    else if (k === OK) { if (!repeat) menuAct(); }
    else if (k === BACK || k === ESC || k === TOOLS || k === LEFT) closeMenu();
    else if (k !== RIGHT && k !== RED) return false;
  }
  function keyPanel(k) {
    if (k === UP) focusItem(itemI - 1);
    else if (k === DOWN) focusItem(Math.min(rows.length - 1, itemI + 1));
    else if (k === OK) { if (rows[itemI]) rows[itemI].act(rows[itemI].el); }
    else if (k === BACK || k === ESC || k === LEFT || k === RED) closePanel();
    else if (k !== RIGHT) return false;
  }

  // OK on an app tile acts on release; held for HOLD_MS it opens the app menu instead, and nothing more happens until
  // it is let go (the TV repeats a held key). A press whose release never came is forgotten after a while.
  var okAt = 0, okTimer = 0, okHeld = false;
  function holdTarget() {
    if (mode === "home" && zone === "dock" && !moving) return docked[dockI] ? { app: docked[dockI], from: "dock" } : null;
    if (mode === "all" && allZone === "grid") return gl[gridI] ? { app: gl[gridI], from: "all" } : null;
    if (mode === "folder") return fold.target();
    return null;
  }
  function okDown(e) {
    if (okAt) {
      if (e.repeat || Date.now() - okAt < 3000) return true;
      clearTimeout(okTimer); okAt = 0;
    }
    if (e.repeat || !holdTarget()) return false;
    okAt = Date.now(); okHeld = false;
    okTimer = setTimeout(function () {
      var t = holdTarget();
      okHeld = true;
      if (t && t.from) openMenu(t.app, t.from);
    }, L.HOLD_MS);
    return true;
  }
  function okUp() {
    if (!okAt) return false;
    clearTimeout(okTimer);
    var held = okHeld;
    okAt = 0; okHeld = false;
    if (!held) { if (mode === "home") keyHome(OK); else if (mode === "all") keyAll(OK); else if (mode === "folder") keyFold(OK); }
    return true;
  }
  document.addEventListener("keydown", function (e) {
    var k = e.keyCode;
    if (k === OK && okDown(e)) { e.preventDefault(); return; }
    var r = mode === "panel" ? keyPanel(k) : mode === "opt" ? keyOpt(k, e.repeat) : mode === "menu" ? keyMenu(k, e.repeat)
      : mode === "all" ? keyAll(k) : mode === "folder" ? keyFold(k) : keyHome(k);
    if (r !== false) e.preventDefault();
  });
  document.addEventListener("keyup", function (e) {
    if (e.keyCode === OK && okUp()) e.preventDefault();
  });


  document.addEventListener("visibilitychange", function () {
    if (document.hidden) return;
    // Back from an app (or the Home key): the home screen, focus on the app just opened (first in a dock never edited).
    tick();
    if (mode === "menu") closeMenu();
    if (mode === "folder") fold.close();
    if (mode === "all") closeAll();
    if (mode === "opt") optVisible();
    if (moving) endMove();
    clearTimeout(okTimer); okAt = 0;
    setApps(raw, false);
    if (mode === "home" && dockCount()) focusDock(Math.max(0, dockIndexOf(lastOpened)));
    loadApps();
    refresh(false);
    dns.report(false);
    showWarn();
    requestPoll();
    if (window.RETV_DEV_SINK) devPoll();
  });

  // --- Developer bridge, off unless dev.js (installed only by `install.js --dev`) sets RETV_DEV_SINK ---
  // Once per start, the full app list (system apps too) goes to the developer's Mac; the Mac can ask this app to open
  // another app by id (system ones included) or to evaluate an expression, to learn what the TV allows.
  function devSend(list) {
    if (!window.RETV_DEV_SINK || devSend.sent) return;
    devSend.sent = true;
    try {
      var x = new XMLHttpRequest();
      x.open("POST", window.RETV_DEV_SINK);
      x.send(JSON.stringify(list.map(function (a) { return { id: a.id, name: a.name, icon: a.iconPath, show: a.show, cats: a.categories, pkg: a.packageId }; })));
    } catch (e) {}
  }
  function devPoll() {
    if (!window.RETV_DEV_SINK || document.hidden) return;
    var base = window.RETV_DEV_SINK.replace(/\/apps$/, "");
    var x = new XMLHttpRequest();
    x.open("GET", base + "/cmd");
    x.onload = function () {
      var id = (x.responseText || "").trim();
      if (!id) return;
      var report = function (r) { var y = new XMLHttpRequest(); y.open("POST", base + "/result"); y.send(id + " " + r); };
      if (id.indexOf("eval:") === 0) {
        try { report("= " + JSON.stringify(eval(id.slice(5)))); } catch (e) { report("throw " + e.name + ": " + e.message); }
        return;
      }
      try { tizen.application.launch(id, function () { report("ok"); }, function (e) { report("error " + e.name + ": " + e.message); }); }
      catch (e) { report("throw " + e.name + ": " + e.message); }
    };
    x.send();
  }

  // --- Start: paint from the cache now, ask the TV and the server after the first frame ---
  // ReTV Bridge's request: in this launch (once Samsung's deferred webapis are there) and in each later one.
  // From the account (requestPoll), once Samsung's deferred webapis are there too (adinfo).
  function onLaunch() { bridgeRequest(); requestPoll(); }
  window.addEventListener("appcontrol", onLaunch);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", onLaunch);
  else setTimeout(onLaunch, 0);
  tick();
  accountChanged();
  showHint();
  setApps(raw, false);
  requestAnimationFrame(function () {
    setTimeout(function () {
      loadApps();
      try { tizen.tvinputdevice.registerKey("ColorF0Red"); } catch (e) {}
      // Tools (10135, the «⋯» menu key on remotes that have it) opens the app menu; the 2021 Smart Remote has none,
      // OK held does the same there.
      try { tizen.tvinputdevice.registerKey("Tools"); } catch (e) {}
      // Samsung's full webapis (productinfo, adinfo) come from the deferred $WEBAPIS script in index.html: the TV puts
      // a small built-in `webapis` (avplay and a few others) in every page, and the $WEBAPIS path only resolves in
      // the HTML the TV loads, not in a script added later (checked on the 2021 TV).
      setTimeout(function () { refresh(false); dns.report(false); checkFirmware(); showWarn(); }, 1500);
      // The service is started a little later, so the return watch is ready when the first app opens; the cold start
      // itself does not wait for it.
      setTimeout(function () { if (returnOn()) service().then(function (s) { return s.connect(); }).then(null, quiet); }, 4000);
      if (window.RETV_DEV_SINK) setInterval(devPoll, 2000);
    }, 0);
  });
})();

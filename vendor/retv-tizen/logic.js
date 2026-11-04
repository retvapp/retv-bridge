// ReTV Home for Samsung: pure logic (no DOM, no Tizen), shared by main.js and the node tests (test/retv-tizen.test.ts).
// ES2017 at most: the 2021 TVs run a Chromium 76 era engine.
(function (root) {
  "use strict";

  // --- Which apps ---
  // getAppsInfo on the 2021 TV lists 369 entries (test/fixtures/tizen-apps-qe55q67aa.json), most of them parts of the
  // system: coba.* components, cobalt-* engines, hotel and factory modes, popups, daemons, many with an icon. Shown:
  // store apps (a 10-character package: 9Ur5IzDKqV.TizenYouTube) the TV shows and that are not services, plus the
  // Samsung apps people use, by id (SAMSUNG: the name to show; a key of STRINGS when the name is translated).
  // Ids are Tizen ids here (org.tizen.netflix-app), not the store numbers the REST API uses.
  var SAMSUNG = {
    "org.tizen.netflix-app": "Netflix",
    "org.tizen.primevideo": "Prime Video",
    "com.samsung.tv.gallery": "Gallery",
    "org.tizen.browser": "Internet",
    "com.samsung.tv.store": "Apps",
    "com.samsung.tv.tvplus": "Samsung TV Plus",
    "org.tizen.tv-viewer": "@liveTv",
    "org.tizen.ambientapp": "Ambient Mode",
    "org.tizen.emanual-app": "e-Manual",
    "org.tizen.menu": "@samsungSettings"
  };
  // One whose launch fails is hidden on that TV from then on (main.js, retv.noLaunch). Checked on the 2021 TV
  // (2026-11-04): Apps, Live TV and Ambient Mode open; TV Plus fails there. SmartThings reports success but shows
  // nothing (it is a background part), so it is not offered at all.
  var UNTRIED = {
    "com.samsung.tv.store": 1, "com.samsung.tv.tvplus": 1, "org.tizen.tv-viewer": 1,
    "org.tizen.ambientapp": 1, "org.tizen.emanual-app": 1, "org.tizen.menu": 1
  };
  // Samsung's Settings (org.tizen.menu, listed with show: false): the full settings menu, drawn over ReTV, which stays
  // on screen; Back twice comes back (checked on the 2021 TV, 2026-11-04). Opened like an app, not watched for the
  // return and not counted as recent.
  var SETTINGS_ID = "org.tizen.menu";
  function overlayApp(id) { return id === SETTINGS_ID; }
  // Store apps whose own name reads badly.
  var RENAME = { "xvvl3S1bvH.TizenBrewStandalone": "TizenBrew" };
  var OWN_PACKAGE = "RetvHome00.";
  var STORE_ID = /^[A-Za-z0-9]{10}\.[A-Za-z0-9_-]+$/;

  function has(o, k) { return Object.prototype.hasOwnProperty.call(o, k); }
  function isService(a) {
    if (/service/i.test(a.id)) return true;
    var c = a.categories;
    if (!Array.isArray(c)) return false;
    for (var i = 0; i < c.length; i++) if (/service|ime$/i.test(String(c[i]))) return true;
    return false;
  }
  function wanted(a) {
    if (!a || typeof a.id !== "string" || !a.id || a.id.indexOf(OWN_PACKAGE) === 0) return false;
    if (has(SAMSUNG, a.id)) return true;
    if (a.show === false || !a.name || !a.iconPath) return false;
    return STORE_ID.test(a.id) && !isService(a);
  }
  /** The name shown for an app: ours for Samsung's (the TV says «tv-viewer», «tvplus»), else the TV's. */
  function appName(a, lang) {
    var n = has(SAMSUNG, a.id) ? SAMSUNG[a.id] : has(RENAME, a.id) ? RENAME[a.id] : null;
    if (n && n.charAt(0) === "@") n = (STRINGS[lang] || STRINGS.en)[n.slice(1)];
    return n || String(a.name || a.id);
  }
  function untried(id) { return has(UNTRIED, id); }

  // Tiles drawn by ReTV: [background, text colour, label (null: the app's name), font px (0: the usual 24)]. Flat
  // fills only: a gradient on a tile repaints on every key press on the 2021 TV.
  var BRAND = {
    "9Ur5IzDKqV.TizenYouTube": ["#ffffff", "#ff0033", "YouTube", 0],
    "org.tizen.netflix-app": ["#000000", "#e50914", "NETFLIX", 0],
    "org.tizen.primevideo": ["#0f79af", "#ffffff", "prime video", 0],
    "vbUQClczfR.Wuakitv": ["#bf0000", "#ffffff", "Rakuten TV", 0],
    "com.samsung.tv.tvplus": ["#1428a0", "#ffffff", "TV Plus", 0],
    "org.tizen.tv-viewer": ["#2a3142", "#ffffff", null, 22],
    "com.samsung.tv.gallery": ["#7b5cff", "#ffffff", "Gallery", 0],
    "org.tizen.browser": ["#1e66d0", "#ffffff", "Internet", 0],
    "com.samsung.tv.store": ["#ffffff", "#1428a0", "Apps", 0],
    "org.tizen.ambientapp": ["#3a3f4b", "#ffffff", "Ambient", 0],
    "org.tizen.emanual-app": ["#2f3542", "#ffffff", "e-Manual", 0],
    "org.tizen.menu": ["#e8eaef", "#1d1d1f", null, 20]
  };
  /** [background, text colour, label, font px] for a drawn tile, or null (the TV's icon). */
  function brandOf(a, lang) {
    var b = has(BRAND, a.id) ? BRAND[a.id] : null;
    return b ? [b[0], b[1], b[2] || appName(a, lang), b[3]] : null;
  }
  // Default order (after the apps opened from ReTV): the big video apps, well-known store apps, the other store apps,
  // then Samsung's own. On the 2021 TV the default dock stays what it was before Samsung's apps were added.
  var FIRST = ["9Ur5IzDKqV.TizenYouTube", "org.tizen.netflix-app", "org.tizen.primevideo", "vbUQClczfR.Wuakitv"];
  var KNOWN_NAMES = ["Disney+", "Apple TV", "Spotify"];
  var LAST = ["com.samsung.tv.gallery", "org.tizen.browser", "com.samsung.tv.tvplus", "org.tizen.tv-viewer",
    "com.samsung.tv.store", "org.tizen.ambientapp", "org.tizen.emanual-app", "org.tizen.menu"];

  function rank(a) {
    var i = FIRST.indexOf(a.id);
    if (i >= 0) return i;
    i = KNOWN_NAMES.indexOf(a.name.replace(/ - .*$/, ""));
    if (i >= 0) return 100 + i;
    i = LAST.indexOf(a.id);
    return i >= 0 ? 1000 + i : 500;
  }

  /** The apps in home order: opened from ReTV most recently first ([recent], ids), then the brand order, then by name. */
  function order(apps, recent) {
    var pos = {};
    (recent || []).forEach(function (id, i) { if (!(id in pos)) pos[id] = i; });
    return apps.slice().sort(function (x, y) {
      var px = x.id in pos ? pos[x.id] : 1e6, py = y.id in pos ? pos[y.id] : 1e6;
      return px - py || rank(x) - rank(y) || x.name.localeCompare(y.name, undefined, { numeric: true, sensitivity: "base" }) || (x.id < y.id ? -1 : 1);
    });
  }

  /** [recent] with [id] moved to the front, at most [max] ids. */
  function pushRecent(recent, id, max) {
    var out = [id];
    (recent || []).forEach(function (r) { if (r !== id && typeof r === "string") out.push(r); });
    return out.slice(0, max || 30);
  }

  /** What the cache keeps of an app list: [id, name, icon] triples. */
  function compact(apps) { return apps.map(function (a) { return [a.id, a.name, a.iconPath]; }); }
  function expand(rows) {
    if (!Array.isArray(rows)) return [];
    return rows.filter(function (r) { return Array.isArray(r) && typeof r[0] === "string" && typeof r[1] === "string"; })
      .map(function (r) { return { id: r[0], name: r[1], iconPath: r[2] || "" }; });
  }
  /** One string per list, to tell whether a re-render is needed at all. */
  function signature(apps) { return apps.map(function (a) { return a.id + "\u0001" + a.name + "\u0001" + a.iconPath; }).join("\u0002"); }

  // --- Layout (board px, 1920×1080, as ReTV Home on Android) ---
  var TILE_W = 160, TILE_H = 90, TILE_GAP = 20, DOCK_INSET = 24, DOCK_MAX_W = 1920 - 2 * 96, DOCK_MIN_W = 400;
  var DOCK_MAX_TILES = Math.floor((DOCK_MAX_W - 2 * DOCK_INSET + TILE_GAP) / (TILE_W + TILE_GAP));
  function dockWidth(n) {
    var w = n * TILE_W + Math.max(0, n - 1) * TILE_GAP + 2 * DOCK_INSET;
    return Math.min(DOCK_MAX_W, Math.max(DOCK_MIN_W, w));
  }

  // --- The dock, as ReTV Home on Android (HomeLayout: DOCK_MAX 8) ---
  // Until the first edit the dock is the first DOCK_MAX apps in home order; after it, the ids the person chose
  // (localStorage retv.dock), in their order. An id whose app is gone is skipped, not forgotten.
  var DOCK_MAX = Math.min(8, DOCK_MAX_TILES);
  /** The saved dock ([ids]), or null for «not edited yet» (anything else read from storage is null too). */
  function dockIds(v) {
    if (!Array.isArray(v)) return null;
    var out = [];
    for (var i = 0; i < v.length && out.length < 64; i++) if (typeof v[i] === "string" && v[i] && out.indexOf(v[i]) < 0) out.push(v[i]);
    return out;
  }
  /** The dock's apps: [saved] ids that are installed, at most DOCK_MAX; the default when nothing is saved. */
  function dockApps(apps, saved) {
    if (!saved) return apps.slice(0, DOCK_MAX);
    var byId = {}, out = [];
    for (var i = 0; i < apps.length; i++) byId[apps[i].id] = apps[i];
    for (var j = 0; j < saved.length && out.length < DOCK_MAX; j++) if (has(byId, saved[j])) out.push(byId[saved[j]]);
    return out;
  }
  /** [ids] (the dock as shown) with [id] added at the end; null when the dock is full. Already there: unchanged. */
  function dockPin(ids, id) {
    if (ids.indexOf(id) >= 0) return ids.slice();
    return ids.length >= DOCK_MAX ? null : ids.concat([id]);
  }
  function dockUnpin(ids, id) { return ids.filter(function (x) { return x !== id; }); }
  /** [id] one place left ([delta] -1) or right (+1); at an end it stays. */
  function dockMove(ids, id, delta) {
    var out = ids.slice(), i = out.indexOf(id), j = i + delta;
    if (i < 0 || j < 0 || j >= out.length) return out;
    out[i] = out[j]; out[j] = id;
    return out;
  }
  /**
   * The app menu's items: an app on a dock tile (from "dock"), in «Все приложения» ("all") or in an open folder
   * ("inFolder"); a folder tile ([isFolder]) in the dock or in «Все приложения».
   */
  function menuKeys(from, inDock, dockSize, isFolder) {
    var move = from === "dock" && dockSize > 1;
    if (isFolder) return ["open", "rename"].concat(move ? ["move"] : [], [inDock ? "unpin" : "pin", "dissolve"]);
    if (from === "inFolder") return ["open", "outFolder", "hide"];
    if (from === "dock") return ["open"].concat(move ? ["move"] : [], ["toFolder", "unpin", "hide", "edit"]);
    return ["open", inDock ? "unpin" : "pin", "toFolder", "hide"];
  }

  // --- Folders and hidden apps, as ReTV Home on Android (HomeLayout, LayoutOps) ---
  // Saved next to retv.dock: retv.folders ([{ id: "folder:N", name, apps: [ids] }]; name null: «Папка», "preset:<key>":
  // a preset in the TV's language, so no on-screen keyboard is needed) and retv.hidden ([ids]: hidden from the home
  // screen and «Все приложения» only; the app stays on the TV). An app is in one folder at most; a folder stands in
  // the dock by its id like an app, and in «Все приложения» where its first app would stand.
  var FOLDER_PREFIX = "folder:", PRESET_PREFIX = "preset:";
  var FOLDER_PRESETS = ["movies", "music", "games", "kids", "sports", "news", "tv", "services", "utilities", "favourites"];
  function isFolderId(id) { return typeof id === "string" && id.indexOf(FOLDER_PREFIX) === 0; }
  /** The saved folders, cleaned: well-formed ids, each app in one folder only, no empty folder. */
  function folderList(v) {
    if (!Array.isArray(v)) return [];
    var out = [], seen = {}, ids = {};
    for (var i = 0; i < v.length && out.length < 64; i++) {
      var f = v[i];
      if (!f || typeof f.id !== "string" || !/^folder:\d{1,6}$/.test(f.id) || ids[f.id] || !Array.isArray(f.apps)) continue;
      var apps = [];
      for (var j = 0; j < f.apps.length; j++) {
        var a = f.apps[j];
        if (typeof a === "string" && a && !isFolderId(a) && !has(seen, a)) { seen[a] = 1; apps.push(a); }
      }
      if (!apps.length) continue;
      ids[f.id] = 1;
      var name = typeof f.name === "string" && f.name.trim() ? f.name.trim().slice(0, 40) : null;
      out.push({ id: f.id, name: name, apps: apps });
    }
    return out;
  }
  /** The saved hidden ids, cleaned. */
  function hiddenList(v) {
    var ids = dockIds(v) || [];
    return ids.filter(function (id) { return !isFolderId(id); });
  }
  /** A folder's name as shown: null is «Папка», a preset key the preset in [lang], anything else as it is. */
  function folderLabel(name, lang) {
    var m = (STRINGS[lang] || STRINGS.en).folder;
    if (name == null) return m.defaultName;
    if (String(name).indexOf(PRESET_PREFIX) === 0) {
      var k = String(name).slice(PRESET_PREFIX.length);
      return FOLDER_PRESETS.indexOf(k) >= 0 ? m.presets[k] : m.defaultName;
    }
    return String(name);
  }
  /**
   * What «Все приложения» shows, in home order ([apps]): each app on its own, except hidden ones and those in a
   * folder; a folder ({ id, folder: true, name, label, apps }) where its first installed app would be. A folder with
   * no installed app is left out.
   */
  function layoutItems(apps, folders, hidden, lang) {
    var byId = {}, inFolder = {}, hid = {}, made = {}, i;
    for (i = 0; i < apps.length; i++) byId[apps[i].id] = apps[i];
    for (i = 0; i < (hidden || []).length; i++) hid[hidden[i]] = 1;
    (folders || []).forEach(function (f) {
      var list = f.apps.filter(function (id) { return has(byId, id) && !has(hid, id); }).map(function (id) { return byId[id]; });
      list.forEach(function (a) { inFolder[a.id] = f.id; });
      if (list.length) made[f.id] = { id: f.id, folder: true, name: folderLabel(f.name, lang), stored: f.name, apps: list };
    });
    var out = [], placed = {};
    for (i = 0; i < apps.length; i++) {
      var a = apps[i];
      if (has(hid, a.id)) continue;
      if (has(inFolder, a.id)) {
        var fid = inFolder[a.id];
        if (!placed[fid]) { placed[fid] = 1; out.push(made[fid]); }
      } else out.push(a);
    }
    return out;
  }
  /** The installed hidden apps, in home order. */
  function hiddenApps(apps, hidden) {
    return apps.filter(function (a) { return (hidden || []).indexOf(a.id) >= 0; });
  }
  /** The dock's items (apps and folders) from [items] (layoutItems): the saved ids, or the first DOCK_MAX. */
  function dockItems(items, saved) { return dockApps(items, saved); }

  // Edits of a layout { dock: [ids as shown], folders, hidden }; each returns a new layout.
  function copyLayout(l) {
    return {
      dock: l.dock.slice(), hidden: (l.hidden || []).slice(),
      folders: (l.folders || []).map(function (f) { return { id: f.id, name: f.name, apps: f.apps.slice() }; })
    };
  }
  function folderById(l, id) { for (var i = 0; i < l.folders.length; i++) if (l.folders[i].id === id) return l.folders[i]; return null; }
  function folderOf(l, appId) { for (var i = 0; i < l.folders.length; i++) if (l.folders[i].apps.indexOf(appId) >= 0) return l.folders[i]; return null; }
  function nextFolderId(l) {
    var max = 0;
    l.folders.forEach(function (f) { var n = parseInt(f.id.slice(FOLDER_PREFIX.length), 10); if (n > max) max = n; });
    return FOLDER_PREFIX + (max + 1);
  }
  /** Folder [id] replaced by its apps: in the dock in its place (what does not fit stays in «Все приложения»). */
  function dissolveIn(l, id) {
    var f = folderById(l, id);
    if (!f) return l;
    l.folders = l.folders.filter(function (x) { return x !== f; });
    var i = l.dock.indexOf(id);
    if (i >= 0) {
      var rest = f.apps.filter(function (a) { return l.dock.indexOf(a) < 0; });
      l.dock = l.dock.slice(0, i).concat(rest, l.dock.slice(i + 1)).slice(0, DOCK_MAX);
    }
    return l;
  }
  /** [appId] out of its folder; a folder left with one app or none dissolves. */
  function takeOut(l, appId) {
    var f = folderOf(l, appId);
    if (!f) return l;
    f.apps = f.apps.filter(function (a) { return a !== appId; });
    return f.apps.length <= 1 ? dissolveIn(l, f.id) : l;
  }
  /** «Новая папка»: a folder holding [appId], in the app's dock place (if it is docked). */
  function newFolder(l0, appId) {
    var l = copyLayout(l0), id = nextFolderId(l);
    takeOut(l, appId);
    l.hidden = l.hidden.filter(function (x) { return x !== appId; });
    l.folders.push({ id: id, name: null, apps: [appId] });
    var i = l.dock.indexOf(appId);
    if (i >= 0) l.dock[i] = id;
    return { layout: l, id: id };
  }
  /** [appId] into folder [id], out of the dock and any other folder. */
  function addToFolder(l0, id, appId) {
    var l = copyLayout(l0), f0 = folderById(l, id);
    if (!f0 || isFolderId(appId) || f0.apps.indexOf(appId) >= 0) return l;
    takeOut(l, appId);
    var f = folderById(l, id);
    if (!f) return l;
    f.apps.push(appId);
    l.dock = l.dock.filter(function (x) { return x !== appId; });
    l.hidden = l.hidden.filter(function (x) { return x !== appId; });
    return l;
  }
  /** «Убрать из папки»: the app is on its own again; a folder left with one app dissolves into it. */
  function removeFromFolder(l0, appId) { return takeOut(copyLayout(l0), appId); }
  function dissolveFolder(l0, id) { return dissolveIn(copyLayout(l0), id); }
  function renameFolder(l0, id, name) {
    var l = copyLayout(l0), f = folderById(l, id);
    if (f) f.name = name == null ? null : String(name);
    return l;
  }
  /** «Скрыть»: off the home screen and «Все приложения» (out of the dock and its folder); the app stays on the TV. */
  function hideApp(l0, appId) {
    var l = takeOut(copyLayout(l0), appId);
    l.dock = l.dock.filter(function (x) { return x !== appId; });
    if (l.hidden.indexOf(appId) < 0) l.hidden.push(appId);
    return l;
  }
  function unhideApp(l0, appId) {
    var l = copyLayout(l0);
    l.hidden = l.hidden.filter(function (x) { return x !== appId; });
    return l;
  }
  /** The home screen's hint line goes after the app menu has been opened this many times. */
  var HINT_USES = 3;
  /** OK held this long opens the app menu (ReTV Home on Android: a long press). */
  var HOLD_MS = 600;

  // --- Market: which site this TV signs in to (RelaySite.kt) ---
  var SITES = { world: "https://retv.app", ru: "https://ru.retv.app" };
  var RU_ZONES = [
    "Europe/Moscow", "Europe/Kaliningrad", "Europe/Samara", "Europe/Volgograd", "Europe/Kirov", "Europe/Astrakhan",
    "Europe/Saratov", "Europe/Ulyanovsk", "W-SU",
    "Asia/Yekaterinburg", "Asia/Omsk", "Asia/Novosibirsk", "Asia/Barnaul", "Asia/Tomsk", "Asia/Novokuznetsk",
    "Asia/Krasnoyarsk", "Asia/Irkutsk", "Asia/Chita", "Asia/Yakutsk", "Asia/Khandyga", "Asia/Vladivostok",
    "Asia/Ust-Nera", "Asia/Magadan", "Asia/Sakhalin", "Asia/Srednekolymsk", "Asia/Kamchatka", "Asia/Anadyr"
  ];
  function placeless(zone) {
    return !zone || zone === "UTC" || zone === "GMT" || zone === "UCT" || zone === "Zulu" || zone === "Universal" || zone.indexOf("Etc/") === 0;
  }
  /** "ru" for a Russian-language TV whose clock is in a Russian zone (or in no place at all), else "world". */
  function marketOf(language, zone) {
    var lang = String(language || "").toLowerCase().split(/[-_]/)[0];
    return lang === "ru" && (RU_ZONES.indexOf(zone) >= 0 || placeless(zone)) ? "ru" : "world";
  }
  function cellOf(v) { return v === "ru" || v === "world" ? v : null; }

  // --- Strings ---
  function langOf(language) {
    var l = String(language || "").toLowerCase().split(/[-_]/)[0];
    return l === "ru" || l === "uk" ? l : "en";
  }
  var STRINGS = {
    en: {
      days: ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
      months: ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
      profile: "Main", allApps: "All apps", settings: "Settings",
      signIn: "Sign in to ReTV", signInSub: "With a code on %s",
      signInBody: "Open %s on your phone and enter the code", wait: "Waiting for the code",
      expired: "The code has expired. Press OK for a new one", offline: "No internet: check the network",
      serverError: "The ReTV server returned an error. Try again in a minute", rateLimited: "Too many attempts: wait a minute",
      done: "Done: Pro is active", donePlus: "Done: Pro+ is active", doneFree: "Done: Pro is not active on this account",
      newCode: "New code", notSignedIn: "Not signed in to ReTV", account: "Account: %s",
      free: "Free", pro: "Pro", plus: "Pro+", proUntil: "Pro until %s", plusUntil: "Pro+ until %s",
      signOut: "Sign out on this TV", signOutSub: "Pro on this TV turns off, the place in the account is freed",
      signOutAgain: "Press OK again to sign out",
      via: { vk: "Signed in with VK", yandex: "Signed in with Yandex", google: "Signed in with Google", telegram: "Signed in with Telegram" },
      noApps: "No apps found", appsFailed: "The app list is not available: %s", openFailed: "Could not open: %s",
      version: "ReTV Home %s", liveTv: "Live TV", samsungSettings: "Samsung Settings",
      menu: {
        open: "Open", move: "Move", moveHint: "With ← →", pin: "Add to the dock", unpin: "Remove from the dock",
        unpinHint: "The app stays on the TV", editDock: "Change the dock", editDockHint: "Add or remove apps",
        close: "Back: close the menu", moving: "◀ ▶ move · OK done",
        dockFull: ["The dock already has %s app. Remove one first", "The dock already has %s apps. Remove one first"],
        pinned: "Added to the dock: %s", unpinned: "Removed from the dock: %s",
        edit: "Change the dock", editDone: "Done", editHint: "OK adds an app to the dock or removes it from there",
        holdHint: "Hold OK on an app for its menu",
        toFolder: "Move to a folder", outFolder: "Take out of the folder", hide: "Hide", hideHint: "The app stays on the TV",
        rename: "Rename", dissolve: "Ungroup the folder", dissolveHint: "The apps stay",
        hidden: "Hidden: %s", folderNew: "New folder", folderPick: "Move to a folder", renameTitle: "Folder name",
        movedTo: "In the folder «%s»: %s", outOf: "Out of the folder: %s", unhidden: "Shown again: %s",
        homeHint: "Hold OK on an app: its menu (folders, hide, move, dock)", allBtn: "All apps"
      },
      folder: {
        defaultName: "Folder", hint: "Hold OK: menu · Back: close", hiddenTitle: "Hidden apps",
        hiddenHint: "OK shows the app again. Hidden only from the home screen: the apps stay on the TV.",
        hiddenTile: "Hidden (%s)", n: ["%s app", "%s apps"],
        presets: { movies: "Movies and series", music: "Music", games: "Games", kids: "Kids", sports: "Sports", news: "News",
          tv: "TV channels", services: "Services", utilities: "Utilities", favourites: "Favourites" }
      },
      opt: {
        item: "Speed up and clean the TV", itemSub: "Tracking, ads, unneeded apps, DNS", title: "Optimisation",
        returnItem: "Return to ReTV after leaving an app", on: "On", off: "Off",
        open: "OK: open", back: "Back", retry: "Try again", done: "Done",
        privacy: "Tracking and ads",
        adsOff: "Interest-based ads: off ✓", adsOn: "Interest-based ads: on", adsUnknown: "Interest-based ads: could not check",
        viewing: "Viewing information: cannot be checked automatically", doneAt: "Done on %s",
        openSamsung: "Open Samsung settings", privacyLead: "Samsung's privacy settings open next. There:",
        privacySteps: [
          "Open «Terms & Conditions, Privacy Policy» (the TV may show it in another language).",
          "Untick both boxes: «Viewing Information Services» and «Interest-Based Advertisements».",
          "Press OK, then Back on the remote: you return to ReTV."
        ],
        waiting: "Waiting for you to come back from Samsung settings",
        privacyOk: "Done: interest-based ads are off ✓",
        privacyOkSub: "The TV does not let apps read the viewing information setting: if both boxes are unticked, it is off too.",
        privacyStill: "Interest-based ads are still on", privacyStillSub: "Try again: untick both boxes and be sure to press OK.",
        privacyUnknown: "Could not read the ad setting on this TV", privacyOpenFailed: "Samsung settings did not open: %s",
        tryAgain: "Open again",
        apps: "Unneeded apps", appsSub: "Remove what you do not use", appsLoading: "Checking what is installed…",
        noService: "The ReTV service on the TV does not answer. Turn the TV off and on, then try again.",
        uses: ["%s launch", "%s launches"], never: "never opened", usesUnknown: "launches unknown", appsN: ["%s app", "%s apps"],
        appsHint: "Apps never opened are ticked; Netflix, YouTube and Gallery never are. OK ticks or unticks an app.",
        removeSelected: "Remove selected (%s)", nothingSelected: "Nothing is ticked",
        noneToRemove: "Nothing to remove: the remaining apps are in use or belong to the TV.",
        confirmTitle: "Remove %s?", confirmYes: "Remove", cancel: "Cancel",
        reinstall: "You can install them again from Samsung Apps.",
        removing: "Removing %s of %s: %s", removed: "Removed: %s", failed: "Could not remove: %s",
        beforeSaved: "The «as it was» list is saved: %s",
        dns: "DNS without ads", dnsPro: "In Pro: ads are blocked in the network itself, for every app on the TV.",
        dnsProHow: "Get Pro on %s and sign in to ReTV in Settings.", dnsCardOn: "Pro: address and steps inside",
        dnsTurnOn: "Turn on «DNS without ads» in your account on %s, then open this again.",
        dnsProEnded: "Pro has ended, so DNS without ads is off. Renew Pro on %s.",
        dnsLinking: "Linking this home network to your account…",
        dnsLinked: "This home network is linked to your account (%s).",
        dnsLater: "The ReTV server asks to wait: open this again in 15 minutes.",
        dnsNotLinked: "Could not link this network: blocking works only in a linked home network. Try again later.",
        dnsLead: "Enter the DNS address in Samsung's network settings:",
        dnsSteps: ["Settings → General → Network → Network Status → IP Settings.", "DNS Settings: Enter manually.", "DNS Server: %s, then OK."],
        dnsSite: "the address shown in your account on %s",
        setup: {
          devModeTitle: "Developer Mode is needed", devModeLead: "ReTV removes apps through Samsung's Developer Mode. Turn it on once:",
          hostIpTitle: "Set the address 127.0.0.1", hostIpLead: "Host PC IP is now %s, it needs to be 127.0.0.1:",
          hostIpLeadNone: "Host PC IP needs to be 127.0.0.1:",
          restartTitle: "Restart the TV", restartLead: "The address is right, the TV only needs a restart:",
          tvApiTitle: "The TV does not answer", tvApiLead: "ReTV could not read the TV's state. Usually a restart helps:",
          s1: "Open Apps on the Samsung home screen.", s2: "Press 1 2 3 4 5 on the remote.", s3: "Turn Developer Mode on.",
          s4: "In Host PC IP enter 127.0.0.1 and press OK.",
          s5: "Restart the TV: hold the power button on the remote until the TV turns off and on again.",
          s6: "Open ReTV and this screen again."
        }
      }
    },
    ru: {
      days: ["Воскресенье", "Понедельник", "Вторник", "Среда", "Четверг", "Пятница", "Суббота"],
      months: ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"],
      profile: "Основной", allApps: "Все приложения", settings: "Настройки",
      signIn: "Войти в ReTV", signInSub: "По коду на %s",
      signInBody: "Откройте %s на телефоне и введите код", wait: "Ждём, когда введут код",
      expired: "Код устарел. Нажмите OK, чтобы получить новый", offline: "Нет интернета: проверьте сеть",
      serverError: "Сервер ReTV ответил ошибкой. Попробуйте через минуту", rateLimited: "Слишком много попыток: подождите минуту",
      done: "Готово: Pro активен", donePlus: "Готово: Pro+ активен", doneFree: "Готово: Pro на этом аккаунте не активен",
      newCode: "Новый код", notSignedIn: "Аккаунт ReTV не подключён", account: "Аккаунт: %s",
      free: "Бесплатно", pro: "Pro", plus: "Pro+", proUntil: "Pro до %s", plusUntil: "Pro+ до %s",
      signOut: "Выйти на этом ТВ", signOutSub: "Pro на этом ТВ выключится, место в аккаунте освободится",
      signOutAgain: "Нажмите OK ещё раз, чтобы выйти",
      via: { vk: "Вход через VK", yandex: "Вход через Яндекс", google: "Вход через Google", telegram: "Вход через Telegram" },
      noApps: "Приложения не найдены", appsFailed: "Список приложений недоступен: %s", openFailed: "Не открылось: %s",
      version: "ReTV Home %s", liveTv: "Телеканалы", samsungSettings: "Настройки Samsung",
      menu: {
        open: "Открыть", move: "Переместить", moveHint: "Стрелками ← →", pin: "Добавить в док", unpin: "Убрать из дока",
        unpinHint: "Приложение останется на ТВ", editDock: "Изменить док", editDockHint: "Добавить или убрать приложения",
        close: "Назад: закрыть меню", moving: "◀ ▶ переместить · OK готово",
        dockFull: ["В доке уже %s приложение. Уберите одно", "В доке уже %s приложения. Уберите одно", "В доке уже %s приложений. Уберите одно"],
        pinned: "Добавлено в док: %s", unpinned: "Убрано из дока: %s",
        edit: "Изменить док", editDone: "Готово", editHint: "OK добавляет приложение в док или убирает его оттуда",
        holdHint: "Удерживайте OK на приложении, чтобы открыть его меню",
        toFolder: "В папку", outFolder: "Убрать из папки", hide: "Скрыть", hideHint: "Приложение останется на ТВ",
        rename: "Переименовать", dissolve: "Расформировать папку", dissolveHint: "Приложения останутся",
        hidden: "Скрыто: %s", folderNew: "Новая папка", folderPick: "Переместить в папку", renameTitle: "Название папки",
        movedTo: "В папке «%s»: %s", outOf: "Убрано из папки: %s", unhidden: "Снова видно: %s",
        homeHint: "Удерживайте OK на приложении: меню (папки, скрыть, переместить, док)", allBtn: "Все приложения"
      },
      folder: {
        defaultName: "Папка", hint: "Удерживайте OK: меню · Назад: закрыть", hiddenTitle: "Скрытые приложения",
        hiddenHint: "OK возвращает приложение. Скрыты только с главного экрана: сами приложения остались на ТВ.",
        hiddenTile: "Скрытые (%s)", n: ["%s приложение", "%s приложения", "%s приложений"],
        presets: { movies: "Кино и сериалы", music: "Музыка", games: "Игры", kids: "Детям", sports: "Спорт", news: "Новости",
          tv: "ТВ-каналы", services: "Сервисы", utilities: "Утилиты", favourites: "Избранное" }
      },
      opt: {
        item: "Ускорить и очистить ТВ", itemSub: "Слежка, реклама, лишние приложения, DNS", title: "Оптимизация",
        returnItem: "Возвращаться в ReTV после выхода из приложения", on: "Вкл.", off: "Выкл.",
        open: "OK: открыть", back: "Назад", retry: "Попробовать снова", done: "Готово",
        privacy: "Слежка и реклама",
        adsOff: "Реклама по интересам: выключена ✓", adsOn: "Реклама по интересам: включена", adsUnknown: "Реклама по интересам: не удалось проверить",
        viewing: "Сведения о просмотре: не проверить автоматически", doneAt: "Сделано %s",
        openSamsung: "Открыть настройки Samsung", privacyLead: "Сейчас откроются настройки конфиденциальности Samsung. Там:",
        privacySteps: [
          "Откройте пункт «Terms & Conditions, Privacy Policy» (условия и политика конфиденциальности; ТВ может показать его на другом языке).",
          "Снимите обе галочки: «Viewing Information Services» (сведения о просмотре) и «Interest-Based Advertisements» (реклама по интересам).",
          "Нажмите OK, затем Назад на пульте: вы вернётесь в ReTV."
        ],
        waiting: "Ждём, когда вы вернётесь из настроек Samsung",
        privacyOk: "Готово: реклама по интересам выключена ✓",
        privacyOkSub: "Сведения о просмотре ТВ проверить не даёт: если обе галочки сняты, они тоже выключены.",
        privacyStill: "Реклама по интересам всё ещё включена", privacyStillSub: "Попробуйте ещё раз: снимите обе галочки и обязательно нажмите OK.",
        privacyUnknown: "Не удалось прочитать настройку рекламы на этом ТВ", privacyOpenFailed: "Настройки Samsung не открылись: %s",
        tryAgain: "Открыть ещё раз",
        apps: "Лишние приложения", appsSub: "Удалить то, чем не пользуетесь", appsLoading: "Смотрим, что установлено…",
        noService: "Служба ReTV на ТВ не отвечает. Выключите и включите ТВ, затем попробуйте снова.",
        uses: ["%s запуск", "%s запуска", "%s запусков"], never: "ни разу не открывали", usesUnknown: "сколько запускали, неизвестно", appsN: ["%s приложение", "%s приложения", "%s приложений"],
        appsHint: "Отмечены приложения, которые ни разу не открывали; Netflix, YouTube и Галерею мы не отмечаем. OK ставит или снимает отметку.",
        removeSelected: "Удалить выбранные (%s)", nothingSelected: "Ничего не отмечено",
        noneToRemove: "Удалять нечего: остальными приложениями пользуются, или они встроены в ТВ.",
        confirmTitle: "Удалить %s?", confirmYes: "Удалить", cancel: "Отмена",
        reinstall: "Их можно снова установить из Samsung Apps.",
        removing: "Удаляем %s из %s: %s", removed: "Удалено: %s", failed: "Не удалось удалить: %s",
        beforeSaved: "Список «как было» сохранён: %s",
        dns: "DNS без рекламы", dnsPro: "Есть в Pro: реклама блокируется прямо в сети, для всех приложений на ТВ.",
        dnsProHow: "Подключите Pro на %s и войдите в ReTV в настройках.", dnsCardOn: "Pro: адрес и шаги внутри",
        dnsTurnOn: "Включите «DNS без рекламы» в кабинете на %s, затем откройте этот раздел снова.",
        dnsProEnded: "Pro закончился, поэтому DNS без рекламы выключен. Продлите Pro на %s.",
        dnsLinking: "Привязываем эту домашнюю сеть к аккаунту…",
        dnsLinked: "Эта домашняя сеть привязана к аккаунту (%s).",
        dnsLater: "Сервер ReTV просит подождать: откройте этот раздел снова через 15 минут.",
        dnsNotLinked: "Не удалось привязать сеть: блокировка работает только в привязанной домашней сети. Попробуйте позже.",
        dnsLead: "Впишите адрес DNS в настройках сети Samsung:",
        dnsSteps: ["Настройки → Общие → Сеть → Состояние сети → Настройки IP.", "Настройка DNS: Ввести вручную.", "Сервер DNS: %s, затем OK."],
        dnsSite: "адрес из кабинета на %s",
        setup: {
          devModeTitle: "Нужен режим разработчика", devModeLead: "ReTV удаляет приложения через режим разработчика Samsung. Включите его один раз:",
          hostIpTitle: "Укажите адрес 127.0.0.1", hostIpLead: "Сейчас в Host PC IP указано %s, а нужно 127.0.0.1:",
          hostIpLeadNone: "В Host PC IP нужно указать 127.0.0.1:",
          restartTitle: "Перезапустите ТВ", restartLead: "Адрес указан верно, осталось перезапустить ТВ:",
          tvApiTitle: "ТВ не отвечает", tvApiLead: "ReTV не смог прочитать состояние ТВ. Обычно помогает перезапуск:",
          s1: "Откройте Apps (Приложения) на главном экране Samsung.", s2: "Наберите на пульте 1 2 3 4 5.",
          s3: "Включите Developer Mode (режим разработчика).", s4: "В поле Host PC IP введите 127.0.0.1 и нажмите OK.",
          s5: "Перезапустите ТВ: удерживайте кнопку питания на пульте, пока ТВ не выключится и не включится снова.",
          s6: "Откройте ReTV и этот раздел ещё раз."
        }
      }
    },
    uk: {
      days: ["Неділя", "Понеділок", "Вівторок", "Середа", "Четвер", "Пʼятниця", "Субота"],
      months: ["січня", "лютого", "березня", "квітня", "травня", "червня", "липня", "серпня", "вересня", "жовтня", "листопада", "грудня"],
      profile: "Основний", allApps: "Усі застосунки", settings: "Налаштування",
      signIn: "Увійти в ReTV", signInSub: "За кодом на %s",
      signInBody: "Відкрийте %s на телефоні та введіть код", wait: "Чекаємо, коли введуть код",
      expired: "Код застарів. Натисніть OK, щоб отримати новий", offline: "Немає інтернету: перевірте мережу",
      serverError: "Сервер ReTV відповів помилкою. Спробуйте за хвилину", rateLimited: "Забагато спроб: зачекайте хвилину",
      done: "Готово: Pro активний", donePlus: "Готово: Pro+ активний", doneFree: "Готово: Pro на цьому акаунті неактивний",
      newCode: "Новий код", notSignedIn: "Акаунт ReTV не підключено", account: "Акаунт: %s",
      free: "Безкоштовно", pro: "Pro", plus: "Pro+", proUntil: "Pro до %s", plusUntil: "Pro+ до %s",
      signOut: "Вийти на цьому ТВ", signOutSub: "Pro на цьому ТВ вимкнеться, місце в акаунті звільниться",
      signOutAgain: "Натисніть OK ще раз, щоб вийти",
      via: { vk: "Вхід через VK", yandex: "Вхід через Яндекс", google: "Вхід через Google", telegram: "Вхід через Telegram" },
      noApps: "Застосунки не знайдено", appsFailed: "Список застосунків недоступний: %s", openFailed: "Не відкрилося: %s",
      version: "ReTV Home %s", liveTv: "Телеканали", samsungSettings: "Налаштування Samsung",
      menu: {
        open: "Відкрити", move: "Перемістити", moveHint: "Стрілками ← →", pin: "Додати в док", unpin: "Прибрати з доку",
        unpinHint: "Застосунок залишиться на ТВ", editDock: "Змінити док", editDockHint: "Додати або прибрати застосунки",
        close: "Назад: закрити меню", moving: "◀ ▶ перемістити · OK готово",
        dockFull: ["У доку вже %s застосунок. Приберіть один", "У доку вже %s застосунки. Приберіть один", "У доку вже %s застосунків. Приберіть один"],
        pinned: "Додано в док: %s", unpinned: "Прибрано з доку: %s",
        edit: "Змінити док", editDone: "Готово", editHint: "OK додає застосунок у док або прибирає його звідти",
        holdHint: "Утримуйте OK на застосунку, щоб відкрити його меню",
        toFolder: "У папку", outFolder: "Прибрати з папки", hide: "Сховати", hideHint: "Застосунок залишиться на ТВ",
        rename: "Перейменувати", dissolve: "Розформувати папку", dissolveHint: "Застосунки залишаться",
        hidden: "Сховано: %s", folderNew: "Нова папка", folderPick: "Перемістити в папку", renameTitle: "Назва папки",
        movedTo: "У папці «%s»: %s", outOf: "Прибрано з папки: %s", unhidden: "Знову видно: %s",
        homeHint: "Утримуйте OK на застосунку: меню (папки, сховати, перемістити, док)", allBtn: "Усі застосунки"
      },
      folder: {
        defaultName: "Папка", hint: "Утримуйте OK: меню · Назад: закрити", hiddenTitle: "Сховані застосунки",
        hiddenHint: "OK повертає застосунок. Сховано лише з головного екрана: самі застосунки залишилися на ТВ.",
        hiddenTile: "Сховані (%s)", n: ["%s застосунок", "%s застосунки", "%s застосунків"],
        presets: { movies: "Кіно та серіали", music: "Музика", games: "Ігри", kids: "Дітям", sports: "Спорт", news: "Новини",
          tv: "ТВ-канали", services: "Сервіси", utilities: "Утиліти", favourites: "Обране" }
      },
      opt: {
        item: "Прискорити й очистити ТВ", itemSub: "Стеження, реклама, зайві застосунки, DNS", title: "Оптимізація",
        returnItem: "Повертатися в ReTV після виходу із застосунку", on: "Увімк.", off: "Вимк.",
        open: "OK: відкрити", back: "Назад", retry: "Спробувати знову", done: "Готово",
        privacy: "Стеження і реклама",
        adsOff: "Реклама за інтересами: вимкнена ✓", adsOn: "Реклама за інтересами: увімкнена", adsUnknown: "Реклама за інтересами: не вдалося перевірити",
        viewing: "Відомості про перегляд: не перевірити автоматично", doneAt: "Зроблено %s",
        openSamsung: "Відкрити налаштування Samsung", privacyLead: "Зараз відкриються налаштування конфіденційності Samsung. Там:",
        privacySteps: [
          "Відкрийте пункт «Terms & Conditions, Privacy Policy» (умови та політика конфіденційності; ТВ може показати його іншою мовою).",
          "Зніміть обидві позначки: «Viewing Information Services» (відомості про перегляд) і «Interest-Based Advertisements» (реклама за інтересами).",
          "Натисніть OK, потім Назад на пульті: ви повернетеся в ReTV."
        ],
        waiting: "Чекаємо, коли ви повернетеся з налаштувань Samsung",
        privacyOk: "Готово: рекламу за інтересами вимкнено ✓",
        privacyOkSub: "Відомості про перегляд ТВ перевірити не дає: якщо обидві позначки зняті, їх теж вимкнено.",
        privacyStill: "Реклама за інтересами досі увімкнена", privacyStillSub: "Спробуйте ще раз: зніміть обидві позначки й обовʼязково натисніть OK.",
        privacyUnknown: "Не вдалося прочитати налаштування реклами на цьому ТВ", privacyOpenFailed: "Налаштування Samsung не відкрилися: %s",
        tryAgain: "Відкрити ще раз",
        apps: "Зайві застосунки", appsSub: "Видалити те, чим не користуєтеся", appsLoading: "Дивимося, що встановлено…",
        noService: "Служба ReTV на ТВ не відповідає. Вимкніть і ввімкніть ТВ, потім спробуйте знову.",
        uses: ["%s запуск", "%s запуски", "%s запусків"], never: "жодного разу не відкривали", usesUnknown: "скільки запускали, невідомо", appsN: ["%s застосунок", "%s застосунки", "%s застосунків"],
        appsHint: "Позначено застосунки, які жодного разу не відкривали; Netflix, YouTube і Галерею ми не позначаємо. OK ставить або знімає позначку.",
        removeSelected: "Видалити вибрані (%s)", nothingSelected: "Нічого не позначено",
        noneToRemove: "Видаляти нічого: іншими застосунками користуються, або вони вбудовані в ТВ.",
        confirmTitle: "Видалити %s?", confirmYes: "Видалити", cancel: "Скасувати",
        reinstall: "Їх можна знову встановити із Samsung Apps.",
        removing: "Видаляємо %s із %s: %s", removed: "Видалено: %s", failed: "Не вдалося видалити: %s",
        beforeSaved: "Список «як було» збережено: %s",
        dns: "DNS без реклами", dnsPro: "Є в Pro: реклама блокується просто в мережі, для всіх застосунків на ТВ.",
        dnsProHow: "Підключіть Pro на %s і увійдіть у ReTV в налаштуваннях.", dnsCardOn: "Pro: адреса й кроки всередині",
        dnsTurnOn: "Увімкніть «DNS без реклами» в кабінеті на %s, потім відкрийте цей розділ знову.",
        dnsProEnded: "Pro закінчився, тому DNS без реклами вимкнено. Продовжте Pro на %s.",
        dnsLinking: "Привʼязуємо цю домашню мережу до акаунта…",
        dnsLinked: "Ця домашня мережа привʼязана до акаунта (%s).",
        dnsLater: "Сервер ReTV просить зачекати: відкрийте цей розділ знову через 15 хвилин.",
        dnsNotLinked: "Не вдалося привʼязати мережу: блокування працює лише в привʼязаній домашній мережі. Спробуйте пізніше.",
        dnsLead: "Впишіть адресу DNS у налаштуваннях мережі Samsung:",
        dnsSteps: ["Налаштування → Загальні → Мережа → Стан мережі → Налаштування IP.", "Налаштування DNS: Ввести вручну.", "Сервер DNS: %s, потім OK."],
        dnsSite: "адресу з кабінету на %s",
        setup: {
          devModeTitle: "Потрібен режим розробника", devModeLead: "ReTV видаляє застосунки через режим розробника Samsung. Увімкніть його один раз:",
          hostIpTitle: "Вкажіть адресу 127.0.0.1", hostIpLead: "Зараз у Host PC IP вказано %s, а потрібно 127.0.0.1:",
          hostIpLeadNone: "У Host PC IP потрібно вказати 127.0.0.1:",
          restartTitle: "Перезапустіть ТВ", restartLead: "Адресу вказано правильно, лишилося перезапустити ТВ:",
          tvApiTitle: "ТВ не відповідає", tvApiLead: "ReTV не зміг прочитати стан ТВ. Зазвичай допомагає перезапуск:",
          s1: "Відкрийте Apps (Застосунки) на головному екрані Samsung.", s2: "Наберіть на пульті 1 2 3 4 5.",
          s3: "Увімкніть Developer Mode (режим розробника).", s4: "У полі Host PC IP введіть 127.0.0.1 і натисніть OK.",
          s5: "Перезапустіть ТВ: утримуйте кнопку живлення на пульті, доки ТВ не вимкнеться й не ввімкнеться знову.",
          s6: "Відкрийте ReTV і цей розділ ще раз."
        }
      }
    }
  };
  function fmt(s) {
    var args = Array.prototype.slice.call(arguments, 1), i = 0;
    return String(s).replace(/%s/g, function () { return i < args.length ? String(args[i++]) : ""; });
  }

  function pad(n) { return (n < 10 ? "0" : "") + n; }
  function clockText(d, h24) {
    if (h24) return pad(d.getHours()) + ":" + pad(d.getMinutes());
    var h = d.getHours() % 12;
    return (h === 0 ? 12 : h) + ":" + pad(d.getMinutes());
  }
  /** «Вторник, 3 ноября»; English: «Tuesday, November 3». */
  function dateText(d, lang) {
    var s = STRINGS[lang] || STRINGS.en;
    return s.days[d.getDay()] + ", " + (lang === "en" ? s.months[d.getMonth()] + " " + d.getDate() : d.getDate() + " " + s.months[d.getMonth()]);
  }
  /** «3 ноября 2026» / «November 3, 2026». */
  function dayText(d, lang) {
    var s = STRINGS[lang] || STRINGS.en;
    return lang === "en" ? s.months[d.getMonth()] + " " + d.getDate() + ", " + d.getFullYear() : d.getDate() + " " + s.months[d.getMonth()] + " " + d.getFullYear();
  }

  // --- Account ---
  /** The claims of an entitlement JWT, unverified (display only: nothing on this TV is unlocked by them yet). */
  function claimsOf(token, decode) {
    try {
      var part = String(token).split(".")[1];
      if (!part) return null;
      var b64 = part.replace(/-/g, "+").replace(/_/g, "/");
      while (b64.length % 4) b64 += "=";
      var raw = (decode || root.atob)(b64);
      var c = JSON.parse(decodeURIComponent(raw.split("").map(function (ch) { return "%" + ("0" + ch.charCodeAt(0).toString(16)).slice(-2); }).join("")));
      return c && typeof c === "object" ? c : null;
    } catch (e) { return null; }
  }
  /** "plus", "pro" or "free" at [nowSec]: a token past its `exp`, or Pro past its `pe`, is free. */
  function planOf(c, nowSec) {
    if (!c || !c.pro || typeof c.exp !== "number" || c.exp <= nowSec) return "free";
    if (c.pe && c.pe <= nowSec) return "free";
    return c.tier === "plus" ? "plus" : "pro";
  }
  /** The plan line: «Pro до 3 ноября 2026», «Pro+», «Бесплатно». */
  function planText(c, nowSec, lang) {
    var s = STRINGS[lang] || STRINGS.en, p = planOf(c, nowSec);
    if (p === "free") return s.free;
    if (c.pe) return fmt(p === "plus" ? s.plusUntil : s.proUntil, dayText(new Date(c.pe * 1000), lang));
    return s[p];
  }
  /** The account label for display: a masked email as is, a provider as «Вход через VK»; anything else is dropped. */
  function labelText(label, lang) {
    var s = STRINGS[lang] || STRINGS.en;
    if (typeof label !== "string" || !label || label.length > 120) return "";
    if (s.via[label]) return s.via[label];
    return label.indexOf("@") > 0 ? label : "";
  }
  /** The sign-in code as the server gives it, grouped for reading off a TV: «ABCD-EFGH». */
  function codeText(code) {
    var c = String(code || "").replace(/[^0-9A-Za-z]/g, "").toUpperCase();
    return c.length === 8 ? c.slice(0, 4) + "-" + c.slice(4) : c;
  }
  /** What a failed sign-in poll means: the server's error code (RFC 8628 names) or the HTTP status. */
  function pollOutcome(status, error) {
    if (status >= 200 && status < 300) return "done";
    if (error === "authorization-pending") return "pending";
    if (error === "slow-down") return "slow";
    if (error === "expired-token" || error === "access-denied") return "expired";
    if (status === 0) return "offline";
    if (status === 429) return "rate";
    return "server";
  }
  // --- «Ускорить и очистить ТВ» ---
  /** Plural forms: ru/uk [one, few, many], en [one, other]; «%s» in the form is the number. */
  function plural(n, forms, lang) {
    var f;
    if (lang === "en") f = n === 1 ? forms[0] : forms[1];
    else {
      var m10 = n % 10, m100 = n % 100;
      f = m10 === 1 && m100 !== 11 ? forms[0] : m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14) ? forms[1] : forms[2];
    }
    return fmt(f, n);
  }
  function opt(lang) { return (STRINGS[lang] || STRINGS.en).opt; }
  /** «не запускали», «1 запуск», «5 запусков». */
  function usesText(n, lang) {
    var o = opt(lang);
    if (typeof n !== "number" || n < 0) return o.usesUnknown;
    return n > 0 ? plural(n, o.uses, lang) : o.never;
  }
  function appsText(n, lang) { return plural(n, opt(lang).appsN, lang); }

  // Apps the person surely uses even when Smart Hub counted no launches (the TV opens some of them by itself).
  var KEEP_TIZEN = { "org.tizen.netflix-app": 1, "9Ur5IzDKqV.TizenYouTube": 1, "com.samsung.tv.gallery": 1 };
  var KEEP_TITLE = /netflix|youtube|gallery|галере/i;
  function keepApp(a) { return KEEP_TIZEN[a.tizenId] === 1 || KEEP_TITLE.test(a.title || "") || KEEP_TITLE.test(a.tizenId || ""); }
  /**
   * Pre-ticked for removal: Smart Hub counted 0 uses, it is not one of the apps above, and it was never opened from
   * ReTV ([recent]: Tizen ids; Smart Hub may not count launches from ReTV). An unknown count (null) is never pre-ticked.
   */
  function preTick(a, recent) {
    if (!a || typeof a.useCount !== "number" || a.useCount !== 0 || keepApp(a)) return false;
    return !(a.tizenId && Array.isArray(recent) && recent.indexOf(a.tizenId) >= 0);
  }
  /**
   * The rows of «Лишние приложения» from the service's `apps.list`: only what can be removed (ReTV, TizenBrew, hidden
   * and system apps left out), least used first (unknown counts last), then by name. `tick`: pre-ticked ([recent]: the
   * Tizen ids opened from ReTV).
   */
  function cleanupRows(list, recent) {
    if (!Array.isArray(list)) return [];
    var key = function (r) { return r.useCount === null ? 1e9 : r.useCount; };
    return list.filter(function (a) { return a && typeof a.id === "string" && a.removable === true && !a.protected && !a.hidden; })
      .map(function (a) {
        var n = typeof a.useCount === "number" && a.useCount >= 0 ? a.useCount : null;
        var r = { id: a.id, title: String(a.title || a.id), useCount: n, tizenId: a.tizenId || "" };
        r.tick = preTick(r, recent);
        return r;
      })
      .sort(function (x, y) { return key(x) - key(y) || x.title.localeCompare(y.title, undefined, { numeric: true, sensitivity: "base" }); });
  }
  /** After the confirm step opens or closes, OK is ignored this long: a double or held OK must not remove apps. */
  var CONFIRM_CALM_MS = 500;
  /** Whether an OK press counts: never an auto-repeat (a held key), never inside the calm window. */
  function okAllowed(repeat, now, calmUntil) { return !repeat && !(typeof calmUntil === "number" && now < calmUntil); }
  /** The ad-tracking state from `webapis.adinfo.isLATEnabled()`: true means interest-based ads are off. */
  function adsState(lat) { return lat === true ? "off" : lat === false ? "on" : "unknown"; }
  function adsText(lat, lang) { var o = opt(lang), s = adsState(lat); return s === "off" ? o.adsOff : s === "on" ? o.adsOn : o.adsUnknown; }
  /**
   * ReTV Bridge's request in this launch's app-control data, or null. Bridge launches ReTV Home over the TV's remote
   * channel (`ed.apps.launch` with a `metaTag`); Samsung hands a web app the tag as the `PAYLOAD` value, raw or as
   * Smart Hub's JSON `{ "values": … }`. "retv:privacy[:<check>]" opens Privacy Choices ({ kind: "open" }),
   * "retv:privacy-check[:<check>]" reads the ad flag again ({ kind: "check" }); `check` names Bridge's request.
   * Kept for TVs that pass it: the 2021 QE55Q67AA ignores that launch altogether (2026-11-04), so Bridge now asks
   * through the account (serverRequest).
   */
  function bridgeRequest(data) {
    if (!Array.isArray(data)) return null;
    for (var i = 0; i < data.length; i++) {
      var d = data[i];
      if (!d || typeof d.key !== "string" || d.key.toUpperCase() !== "PAYLOAD") continue;
      var v = Array.isArray(d.value) ? d.value[0] : d.value;
      if (typeof v !== "string") continue;
      var s = v.trim();
      if (s.charAt(0) === "{") {
        try { var j = JSON.parse(s); s = j && typeof j.values === "string" ? j.values.trim() : ""; } catch (e) { s = ""; }
      }
      var m = /^retv:(privacy|privacy-check)(?::([A-Za-z0-9]{8,32}))?$/.exec(s);
      if (m) return { kind: m[1] === "privacy" ? "open" : "check", check: m[2] || null };
    }
    return null;
  }
  /**
   * ReTV Bridge's request from the account (`GET /v1/tv/requests/pending` answers `{ request }`), or null. The way that
   * works on every TV: a 2021 Tizen ignores the remote's `ed.apps.launch`, so Bridge leaves the request on the server
   * and starts ReTV Home over sdb. "privacy" opens Privacy Choices ({ kind: "open" }), "privacy-check" reads again.
   */
  function serverRequest(d) {
    var r = d && typeof d === "object" ? d.request : null;
    if (!r || typeof r !== "object" || typeof r.check !== "string" || !/^[A-Za-z0-9]{8,32}$/.test(r.check)) return null;
    if (r.kind === "privacy") return { kind: "open", check: r.check };
    if (r.kind === "privacy-check") return { kind: "check", check: r.check };
    return null;
  }
  /** How often and how long ReTV Home asks for Bridge's request after a start or a return to the screen. */
  var REQUEST_POLL_MS = 2000, REQUEST_POLL_FOR_MS = 20000;
  /** The body of `POST /v1/tv/privacy`: the flag as read (null when the TV would not say), Bridge's check id, whether Privacy Choices opened. */
  function privacyReport(lat, check, opened) {
    var b = { latEnabled: lat === true ? true : lat === false ? false : null };
    if (check) b.check = check;
    if (typeof opened === "boolean") b.opened = opened;
    return b;
  }
  /** Which guide `setup.check`'s state needs: null when the service can remove apps now. */
  function setupKind(state) {
    if (state === "READY") return null;
    if (state === "DEV_MODE_OFF") return "devMode";
    if (state === "NEEDS_HOST_IP") return "hostIp";
    if (state === "NEEDS_RESTART") return "restart";
    return "tvApi";
  }
  /** { title, lead, steps } for a setup guide; [ip]: what Host PC IP is now. */
  function setupGuide(kind, lang, ip) {
    var o = opt(lang), st = o.setup;
    if (kind === "devMode") return { title: st.devModeTitle, lead: st.devModeLead, steps: [st.s1, st.s2, st.s3, st.s4, st.s5, st.s6] };
    if (kind === "hostIp") return { title: st.hostIpTitle, lead: ip ? fmt(st.hostIpLead, ip) : st.hostIpLeadNone, steps: [st.s1, st.s2, st.s4, st.s5, st.s6] };
    if (kind === "restart") return { title: st.restartTitle, lead: st.restartLead, steps: [st.s5, st.s6] };
    return { title: st.tvApiTitle, lead: st.tvApiLead, steps: [st.s5, st.s6] };
  }
  /**
   * What the DNS card shows: "pro" (no Pro on this TV: «Есть в Pro»), "proEnded", "turnOn" (Pro, but DNS is not switched
   * on in the cabinet yet) or "ready". [dns]: the refresh answer's `dns` ({ host, on, proEnded? }) or null.
   */
  function dnsView(plan, dns) {
    if (dns && dns.proEnded && plan === "free") return "proEnded";
    if (plan === "free") return "pro";
    return dns && dns.on === true ? "ready" : "turnOn";
  }
  // When to link the home network again (ReTV Home on Android: DnsReportPolicy): every 6 h, never twice in 15 min.
  var DNS_REPORT_EVERY_MS = 6 * 3600000, DNS_REPORT_GAP_MS = 15 * 60000;
  /** [lastOk]: the last report the server took; [lastTried]: the last try; [force]: the page is open (still not in the gap). */
  function dnsReportDue(on, now, lastOk, lastTried, force) {
    if (!on) return false;
    if (lastTried && now >= lastTried && now - lastTried < DNS_REPORT_GAP_MS) return false;
    if (force) return true;
    return typeof lastOk !== "number" || now < lastOk || now - lastOk >= DNS_REPORT_EVERY_MS;
  }
  /** What a `POST /v1/tv/dns` answer means for the person: "linked", "later" (429: wait, try later) or "notLinked". */
  function dnsLinkState(status, d) {
    if (status === 429) return "later";
    return status === 200 && d && d.on === true && typeof d.ip === "string" && d.ip ? "linked" : "notLinked";
  }
  /** The IPv4 address to type on the TV (Samsung's manual DNS takes IPv4), from a list of resolver addresses. */
  function dnsServer(servers) {
    if (!Array.isArray(servers)) return null;
    for (var i = 0; i < servers.length; i++) if (typeof servers[i] === "string" && /^\d{1,3}(\.\d{1,3}){3}$/.test(servers[i])) return servers[i];
    return null;
  }

  function isDeviceKey(k) { return typeof k === "string" && /^[0-9a-f]{64}$/.test(k); }
  function hex(bytes) { var s = ""; for (var i = 0; i < bytes.length; i++) s += ("0" + bytes[i].toString(16)).slice(-2); return s; }

  root.RetvLogic = {
    wanted: wanted, order: order, pushRecent: pushRecent, compact: compact, expand: expand, signature: signature,
    appName: appName, untried: untried, brandOf: brandOf, DOCK_MAX: DOCK_MAX, dockIds: dockIds, dockApps: dockApps,
    dockPin: dockPin, dockUnpin: dockUnpin, dockMove: dockMove, menuKeys: menuKeys, HOLD_MS: HOLD_MS,
    BRAND: BRAND, TILE_W: TILE_W, TILE_H: TILE_H, TILE_GAP: TILE_GAP, DOCK_INSET: DOCK_INSET, DOCK_MAX_TILES: DOCK_MAX_TILES,
    dockWidth: dockWidth, SITES: SITES, marketOf: marketOf, cellOf: cellOf, langOf: langOf, STRINGS: STRINGS, fmt: fmt,
    clockText: clockText, dateText: dateText, claimsOf: claimsOf, planOf: planOf, planText: planText, labelText: labelText,
    codeText: codeText, pollOutcome: pollOutcome, isDeviceKey: isDeviceKey, hex: hex, dayText: dayText,
    plural: plural, usesText: usesText, appsText: appsText, keepApp: keepApp, preTick: preTick, cleanupRows: cleanupRows,
    adsState: adsState, adsText: adsText, setupKind: setupKind, setupGuide: setupGuide, dnsView: dnsView, dnsServer: dnsServer,
    CONFIRM_CALM_MS: CONFIRM_CALM_MS, okAllowed: okAllowed, dnsReportDue: dnsReportDue, dnsLinkState: dnsLinkState,
    bridgeRequest: bridgeRequest, serverRequest: serverRequest, REQUEST_POLL_MS: REQUEST_POLL_MS, REQUEST_POLL_FOR_MS: REQUEST_POLL_FOR_MS,
    privacyReport: privacyReport, SETTINGS_ID: SETTINGS_ID, overlayApp: overlayApp,
    FOLDER_PRESETS: FOLDER_PRESETS, PRESET_PREFIX: PRESET_PREFIX, isFolderId: isFolderId, folderList: folderList,
    hiddenList: hiddenList, folderLabel: folderLabel, layoutItems: layoutItems, hiddenApps: hiddenApps, dockItems: dockItems,
    newFolder: newFolder, addToFolder: addToFolder, removeFromFolder: removeFromFolder, dissolveFolder: dissolveFolder,
    renameFolder: renameFolder, hideApp: hideApp, unhideApp: unhideApp, HINT_USES: HINT_USES
  };
})(typeof window !== "undefined" ? window : this);

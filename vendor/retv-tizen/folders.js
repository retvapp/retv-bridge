// ReTV Home for Samsung: an open folder, or «Скрытые приложения», a layer with its own grid over the home screen or
// «Все приложения». In a folder OK opens an app and OK held its menu; in «Скрытые» OK shows the app again (also the
// Samsung apps ReTV hid because this TV would not open them). main.js creates it with what it needs ([h] below).
// ES2017 at most, no framework; tiles are drawn once per opening, a key press moves the ring with a transform.
(function (root) {
  "use strict";

  /**
   * RetvFolders(h): h = { $, F (folder strings), tile(a, x, y), clearTiles(box, keep), ringAt(ring, x, y), GX, GY,
   *   mode(), setMode(m), menuBack(), setMenuBack(m), items(), hiddenAll(), launch(a), unhide(a), openMenu(a, from),
   *   openPanel(), afterClose() }. key(k, keys) takes the key codes main.js uses.
   */
  root.RetvFolders = function (h) {
    var F = h.F, COLS = 8, HIDDEN = "hidden:";
    var layer = h.$("fold"), grid = h.$("foldGrid"), ring = h.$("foldRing"), hint = h.$("foldHint"), title = h.$("foldTitle");
    var st = { on: false, id: null, list: [], i: 0, back: "home" };

    function hintText() {
      if (st.id !== HIDDEN) return F.hint;
      // Samsung apps this TV would not open are in the list too: say why they are here.
      for (var i = 0; i < st.list.length; i++) if (st.list[i].blocked) return F.blockedHint;
      return F.hiddenHint;
    }
    function draw() {
      var f = document.createDocumentFragment();
      for (var i = 0; i < st.list.length; i++) f.appendChild(h.tile(st.list[i], (i % COLS) * h.GX, Math.floor(i / COLS) * h.GY));
      h.clearTiles(grid, ring);
      grid.insertBefore(f, ring);
      hint.textContent = hintText();
      focus(Math.max(0, Math.min(st.i, st.list.length - 1)));
    }
    function focus(i) {
      var tiles = grid.children;
      if (tiles[st.i] && tiles[st.i] !== ring) tiles[st.i].classList.remove("f");
      st.i = i;
      if (tiles[i] && tiles[i] !== ring) tiles[i].classList.add("f");
      h.ringAt(ring, (i % COLS) * h.GX, Math.floor(i / COLS) * h.GY);
    }
    function openLayer(id, name, list) {
      st.on = true; st.id = id; st.list = list; st.i = 0;
      st.back = h.mode() === "menu" ? h.menuBack() : h.mode();
      title.textContent = name;
      h.setMode("folder");
      draw();
      layer.classList.add("on");
    }
    function open(item) { openLayer(item.id, item.name, item.apps); }
    function openHidden() { openLayer(HIDDEN, F.hiddenTitle, h.hiddenAll()); }
    /** After an edit: the folder's apps again; a folder that is gone, or an empty hidden list, closes. */
    function refresh() {
      var list = null, name = title.textContent, items = h.items();
      if (st.id === HIDDEN) list = h.hiddenAll();
      else for (var i = 0; i < items.length; i++) if (items[i].id === st.id) { list = items[i].apps; name = items[i].name; }
      if (!list || !list.length) { close(); return; }
      st.list = list;
      title.textContent = name;
      draw();
    }
    function close() {
      if (!st.on) return;
      st.on = false;
      layer.classList.remove("on");
      if (h.mode() === "folder") h.setMode(st.back);
      else if (h.mode() === "menu" && h.menuBack() === "folder") h.setMenuBack(st.back);
      h.afterClose();
    }
    function ok() {
      var a = st.list[st.i];
      if (!a) return;
      if (st.id !== HIDDEN) h.launch(a); else h.unhide(a);
    }
    /** What OK held acts on: an app in a folder has a menu, one in «Скрытые» has none (from: null). */
    function target() {
      var a = st.list[st.i];
      return a ? { app: a, from: st.id === HIDDEN ? null : "inFolder" } : null;
    }
    function key(k, K) {
      var n = st.list.length, i = st.i;
      if (k === K.LEFT) { if (i % COLS) focus(i - 1); }
      else if (k === K.RIGHT) { if (i % COLS < COLS - 1 && i < n - 1) focus(i + 1); }
      else if (k === K.UP) { if (i >= COLS) focus(i - COLS); }
      else if (k === K.DOWN) { if (i + COLS < n) focus(i + COLS); }
      else if (k === K.OK) ok();
      else if (k === K.TOOLS) { if (st.id !== HIDDEN) h.openMenu(st.list[i], "inFolder"); }
      else if (k === K.BACK || k === K.ESC) close();
      else if (k === K.RED) h.openPanel();
      else return false;
    }
    return { state: st, open: open, openHidden: openHidden, refresh: refresh, close: close, key: key, target: target, hintText: hintText };
  };
})(typeof window !== "undefined" ? window : this);

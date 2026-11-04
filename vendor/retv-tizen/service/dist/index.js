/* ReTV Home service, bundled by apps/retv-tizen/service/build.js from src/. Do not edit. */
(function (nodeRequire, hostModule) {
var defs = {}, cache = {};
function load(id) {
  if (cache[id]) return cache[id].exports;
  if (!defs[id]) return nodeRequire(id);
  var m = cache[id] = { exports: {} };
  defs[id].call(m.exports, m, m.exports, load);
  return m.exports;
}
defs["./compat.js"] = function (module, exports, require) {
// Buffers that work on the TV's Node: v4.4.3 on Tizen 3 to 6 sets has no Buffer.from(string, encoding)
// (it is Uint8Array.from there) and no Buffer.alloc. Every module builds buffers through here.
// All service code is ES5 + Promise for the same reason.
'use strict';
var NATIVE = typeof Buffer.alloc === 'function' && Buffer.from !== Uint8Array.from;

exports.from = function (value, encoding) {
  return NATIVE ? Buffer.from(value, encoding) : new Buffer(value, encoding);
};
exports.alloc = function (size) {
  if (NATIVE) return Buffer.alloc(size);
  var b = new Buffer(size); b.fill(0); return b;
};
/** setTimeout as a promise. */
exports.wait = function (ms) {
  return new Promise(function (resolve) { setTimeout(resolve, ms); });
};
/** An Error with a stable code the protocol carries to the page. */
exports.fail = function (code, message) {
  var e = new Error(message || code); e.code = code; return e;
};

};
defs["./sdb.js"] = function (module, exports, require) {
// sdb (Samsung's adb) client over loopback, ES5 port of tools/samsung/sdb.js (run only, no push).
// Retail TVs allow only Samsung's `0 <cmd>` services: vd_applist, vd_appuninstall, getduid, was_execute.
// sdbd answers a loopback client only when Developer Mode's Host PC IP is 127.0.0.1.
'use strict';
var net = require('net');
var compat = require('./compat.js');

var A_VERSION = 0x02000000;
var MAX = 256 * 1024;

function packet(cmd, a0, a1, data) {
  data = data || compat.alloc(0);
  var c = compat.from(cmd, 'ascii').readUInt32LE(0);
  var h = compat.alloc(24);
  var sum = 0;
  for (var i = 0; i < data.length; i++) sum = (sum + data[i]) >>> 0;
  h.writeUInt32LE(c, 0); h.writeUInt32LE(a0 >>> 0, 4); h.writeUInt32LE(a1 >>> 0, 8);
  h.writeUInt32LE(data.length, 12); h.writeUInt32LE(sum, 16); h.writeUInt32LE((c ^ 0xffffffff) >>> 0, 20);
  return Buffer.concat([h, data]);
}

/** Splits a byte stream into packets: push(bytes) returns the whole packets seen so far. */
function Parser() { this.buf = compat.alloc(0); }
Parser.prototype.push = function (d) {
  this.buf = Buffer.concat([this.buf, d]);
  var out = [];
  while (this.buf.length >= 24) {
    var n = this.buf.readUInt32LE(12);
    if (this.buf.length < 24 + n) break;
    out.push({ cmd: this.buf.slice(0, 4).toString('ascii'), a0: this.buf.readUInt32LE(4), a1: this.buf.readUInt32LE(8),
      data: this.buf.slice(24, 24 + n) });
    this.buf = this.buf.slice(24 + n);
  }
  return out;
};

/**
 * Runs one service (e.g. `shell:0 vd_applist`) and resolves with its whole text output.
 * opts: { host = 127.0.0.1, port = 26101, timeoutMs = 120000 }. Errors carry code SDB_DOWN (nothing
 * listens), SDB_DROPPED (sdbd closed without a word: Host PC IP is not this address), SDB_TIMEOUT, SDB_ERROR.
 */
function run(service, opts) {
  opts = opts || {};
  var host = opts.host || '127.0.0.1', port = opts.port || 26101;
  return new Promise(function (resolve, reject) {
    var parser = new Parser();
    var out = '', hello = false, done = false;
    var sock = net.createConnection({ host: host, port: port });
    function finish(err) {
      if (done) return;
      done = true;
      try { sock.destroy(); } catch (e) {}
      if (err && !out) reject(err); else resolve(out);
    }
    sock.setTimeout(opts.timeoutMs || 120000, function () { finish(compat.fail('SDB_TIMEOUT', 'sdb did not answer')); });
    sock.on('connect', function () { sock.write(packet('CNXN', A_VERSION, MAX, compat.from('host::\0'))); });
    sock.on('data', function (d) {
      var ps = parser.push(d);
      for (var i = 0; i < ps.length; i++) {
        var p = ps[i];
        if (!hello) {
          if (p.cmd !== 'CNXN') return finish(compat.fail('SDB_ERROR', 'not an sdb device'));
          hello = true;
          sock.write(packet('OPEN', 1, 0, compat.from(service + '\0')));
        } else if (p.cmd === 'WRTE') {
          out += p.data.toString('utf8');
          sock.write(packet('OKAY', 1, p.a0));
        } else if (p.cmd === 'CLSE') {
          return finish(null);
        }
      }
    });
    function dropped() {
      return compat.fail('SDB_DROPPED', 'sdbd closed the connection: Host PC IP is not 127.0.0.1, or the TV needs a restart');
    }
    sock.on('error', function (e) {
      if (e.code === 'ECONNREFUSED') return finish(compat.fail('SDB_DOWN', 'nothing listens on sdb ' + host + ':' + port + ': Developer Mode is off'));
      if (!hello && (e.code === 'ECONNRESET' || e.code === 'EPIPE')) return finish(dropped());
      finish(compat.fail('SDB_ERROR', 'sdb: ' + (e.code || e.message)));
    });
    sock.on('close', function () { finish(hello ? null : dropped()); });
  });
}

module.exports = { run: run, packet: packet, Parser: Parser };

};
defs["./store.js"] = function (module, exports, require) {
// A small JSON key/value file in the service's data folder. The service runs from the read-only
// package folder; the package's private data folder under apps_rw survives reboots and upgrades.
'use strict';
var fs = require('fs');
var path = require('path');

function dataDirs(pkg) {
  // The TV's own app-data folders (the Tizen user is called «owner»); built from parts so no text looks like a Mac home.
  var rw = ['home', 'owner', 'apps_rw'].join('/') + '/' + pkg + '/data';
  return ['/opt/usr/' + rw, '/' + rw, '/tmp/' + pkg];
}

/** The first folder of [dirs] the service can write to, or null. */
function pickDir(dirs) {
  for (var i = 0; i < dirs.length; i++) {
    try {
      try { fs.mkdirSync(dirs[i]); } catch (e) {}
      var probe = path.join(dirs[i], '.retv-write-test');
      fs.writeFileSync(probe, '');
      fs.unlinkSync(probe);
      return dirs[i];
    } catch (e) {}
  }
  return null;
}

/** Store(file): get(key), set(key, value). Without a file (no writable folder) it keeps values in memory. */
function Store(file) {
  this.file = file;
  this.data = {};
  if (file) { try { this.data = JSON.parse(fs.readFileSync(file, 'utf8')) || {}; } catch (e) { this.data = {}; } }
}
Store.prototype.get = function (key) {
  return Object.prototype.hasOwnProperty.call(this.data, key) ? this.data[key] : undefined;
};
Store.prototype.set = function (key, value) {
  if (value === undefined) delete this.data[key]; else this.data[key] = value;
  if (!this.file) return;
  var tmp = this.file + '.tmp';
  fs.writeFileSync(tmp, JSON.stringify(this.data, null, 1));
  fs.renameSync(tmp, this.file);
};

module.exports = { Store: Store, dataDirs: dataDirs, pickDir: pickDir };

};
defs["./applist.js"] = function (module, exports, require) {
// Parses the output of `0 vd_applist`: blocks of `--------------key   =value-------------` lines
// between rows of dashes, one block per app Smart Hub lists.
'use strict';

var LINE = /^-+([A-Za-z_][^=]*?)\s*=(.*?)-{13}\s*$/;

function parse(text) {
  var apps = [], cur = null;
  var lines = String(text || '').split(/\r?\n/);
  function flush() {
    if (cur && cur.app_id) {
      apps.push({
        id: cur.app_id,
        title: cur.app_title || cur.app_runtitle || cur.app_id,
        // null when the TV did not say (missing or not a number): the page must not take that for «never used».
        useCount: /^\s*\d+\s*$/.test(cur.use_count || '') ? parseInt(cur.use_count, 10) : null,
        removable: cur.is_removable === '1',
        tizenId: cur.app_tizen_id || '',
        packageId: cur.app_package_name || '',
        hidden: cur.is_hidden === '1',
        size: parseInt(cur.app_size, 10) || 0,
        installed: cur.install_date || ''
      });
    }
    cur = null;
  }
  for (var i = 0; i < lines.length; i++) {
    var l = lines[i];
    if (/^-{40,}\s*$/.test(l)) { flush(); continue; }
    var m = LINE.exec(l);
    if (!m) continue;
    if (!cur) cur = {};
    cur[m[1].replace(/\(.*$/, '')] = m[2];
  }
  flush();
  return apps;
}

module.exports = { parse: parse };

};
defs["./apps.js"] = function (module, exports, require) {
// The TV's app list (what Smart Hub lists, with use counts) and removal of the apps the person ticks.
// Removal is refused for anything not in the current list, not removable, or ReTV / TizenBrew itself.
// Before the first removal the whole list is kept as «как было» (store key "appsBefore").
'use strict';
var applist = require('./applist.js');
var compat = require('./compat.js');

// Package ids that are never removed: ReTV Home itself and TizenBrew.
var PROTECTED_PACKAGES = ['RetvHome00', 'xvvl3S1bvH'];
var SAFE_ID = /^[A-Za-z0-9._-]{1,64}$/;

function isProtected(app, extra) {
  var pkgs = PROTECTED_PACKAGES.concat(extra || []);
  for (var i = 0; i < pkgs.length; i++) {
    var p = pkgs[i];
    if (app.id === p || app.packageId === p || app.tizenId.indexOf(p + '.') === 0) return true;
  }
  return /tizenbrew/i.test(app.title) || /tizenbrew/i.test(app.tizenId);
}

/**
 * Apps({ sdbRun(service) -> Promise<string>, store, protectedPackages?, now?() })
 *   list()          -> [{ id, title, useCount, removable, tizenId, packageId, hidden, size, installed, protected }]
 *   uninstall(id)   -> { removed: id, title }; errors NOT_IN_LIST, NOT_REMOVABLE, PROTECTED, BAD_ID, UNINSTALL_FAILED
 *   before()        -> { at, apps } or null
 * sdb calls are serialized: sdbd serves one client well, not several at once.
 */
function Apps(opts) {
  this.sdbRun = opts.sdbRun;
  this.store = opts.store;
  this.extra = opts.protectedPackages || [];
  this.now = opts.now || function () { return new Date().toISOString(); };
  this._chain = Promise.resolve();
}

Apps.prototype._serial = function (fn) {
  var next = this._chain.then(fn, fn);
  this._chain = next.then(function () {}, function () {});
  return next;
};

Apps.prototype._list = function () {
  var self = this;
  return this.sdbRun('shell:0 vd_applist').then(function (text) {
    var apps = applist.parse(text);
    for (var i = 0; i < apps.length; i++) apps[i].protected = isProtected(apps[i], self.extra);
    return apps;
  });
};

Apps.prototype.list = function () {
  var self = this;
  return this._serial(function () { return self._list(); });
};

Apps.prototype.before = function () {
  return Promise.resolve(this.store.get('appsBefore') || null);
};

Apps.prototype.uninstall = function (id) {
  var self = this;
  return this._serial(function () {
    if (typeof id !== 'string' || !SAFE_ID.test(id)) throw compat.fail('BAD_ID', 'not an app id: ' + id);
    return self._list().then(function (apps) {
      var app = null;
      for (var i = 0; i < apps.length; i++) if (apps[i].id === id) app = apps[i];
      if (!app) throw compat.fail('NOT_IN_LIST', id + ' is not in the TV\'s app list');
      if (app.protected) throw compat.fail('PROTECTED', app.title + ' is never removed');
      if (!app.removable) throw compat.fail('NOT_REMOVABLE', app.title + ' cannot be removed');
      if (!self.store.get('appsBefore')) self.store.set('appsBefore', { at: self.now(), apps: apps });
      return self.sdbRun('shell:0 vd_appuninstall ' + id).then(function (out) {
        if (!/uninstall completed/.test(out)) {
          var tail = String(out).trim().split('\n').slice(-3).join(' | ');
          throw compat.fail('UNINSTALL_FAILED', 'the TV did not remove ' + app.title + (tail ? ': ' + tail : ''));
        }
        return { removed: id, title: app.title };
      });
    });
  });
};

module.exports = { Apps: Apps, isProtected: isProtected, PROTECTED_PACKAGES: PROTECTED_PACKAGES };

};
defs["./setup.js"] = function (module, exports, require) {
// Whether the TV is set up for the on-TV service: Developer Mode on, Host PC IP 127.0.0.1, sdbd answering.
// Reads the TV's own REST API (http://127.0.0.1:8001/api/v2/, no pairing needed), then probes sdb.
'use strict';
var http = require('http');

var LOOPBACK = ['127.0.0.1', '1.0.0.127'];   // some firmware reports the address byte-reversed (seen by TizenBrew)

// state -> what the person has to do, in words the page can show or map to its own texts.
var STATES = {
  READY: 'ready',
  TV_API_DOWN: 'the TV\'s own API on 127.0.0.1:8001 does not answer',
  DEV_MODE_OFF: 'Developer Mode is off: Apps, type 1 2 3 4 5, turn Developer Mode on',
  NEEDS_HOST_IP: 'Developer Mode\'s Host PC IP must be 127.0.0.1, then restart the TV',
  NEEDS_RESTART: 'Host PC IP is 127.0.0.1 but sdb does not answer: restart the TV (hold Power until it restarts)'
};

function getJson(url, timeoutMs) {
  return new Promise(function (resolve, reject) {
    var req = http.get(url, function (res) {
      var body = '';
      res.setEncoding('utf8');
      res.on('data', function (d) { body += d; });
      res.on('end', function () { try { resolve(JSON.parse(body)); } catch (e) { reject(e); } });
    });
    req.setTimeout(timeoutMs || 4000, function () { req.abort(); });
    req.on('error', reject);
  });
}

/**
 * check({ getJson?(url), sdbRun?(service) }) -> { state, message, developerMode, developerIP, duid?, model?, firmware? }
 */
function check(deps) {
  deps = deps || {};
  var get = deps.getJson || getJson;
  var info = { state: 'TV_API_DOWN', developerMode: null, developerIP: null };
  function done(state, extra) {
    info.state = state;
    info.message = STATES[state];
    if (extra) for (var k in extra) info[k] = extra[k];
    return info;
  }
  return get('http://127.0.0.1:8001/api/v2/').then(function (json) {
    var d = (json && json.device) || {};
    info.developerMode = d.developerMode === '1';
    info.developerIP = d.developerIP || '';
    info.model = d.modelName || '';
    info.firmware = d.firmwareVersion || '';
    if (!info.developerMode) return done('DEV_MODE_OFF');
    if (LOOPBACK.indexOf(info.developerIP) < 0) return done('NEEDS_HOST_IP');
    if (!deps.sdbRun) return done('READY');
    return deps.sdbRun('shell:0 getduid').then(function (out) {
      var duid = String(out).trim().split('\n').pop();
      return done('READY', { duid: duid });
    }, function (e) {
      return done('NEEDS_RESTART', { sdbError: e.code || e.message });
    });
  }, function () { return done('TV_API_DOWN'); });
}

module.exports = { check: check, getJson: getJson, STATES: STATES };

};
defs["./ws.js"] = function (module, exports, require) {
// A minimal WebSocket (RFC 6455) client and server in ES5 with no dependencies, so the service runs on the
// TV's Node 4.4.3 as well as newer ones without bundling the `ws` package. Text frames, ping/pong, close,
// fragmented messages; no extensions. Client: the TV's remote-control channel on wss://127.0.0.1:8002.
// Server: the channel the ReTV Home page talks to on ws://127.0.0.1:<port>/rpc.
'use strict';
var crypto = require('crypto');
var EventEmitter = require('events').EventEmitter;
var net = require('net');
var tls = require('tls');
var urlMod = require('url');
var util = require('util');
var compat = require('./compat.js');

var GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function acceptKey(key) {
  return crypto.createHash('sha1').update(key + GUID).digest('base64');
}

function encodeFrame(opcode, payload, masked) {
  var len = payload.length;
  var head;
  if (len < 126) { head = compat.alloc(2); head[1] = len; }
  else if (len < 65536) { head = compat.alloc(4); head[1] = 126; head.writeUInt16BE(len, 2); }
  else { head = compat.alloc(10); head[1] = 127; head.writeUInt32BE(0, 2); head.writeUInt32BE(len, 6); }
  head[0] = 0x80 | opcode;
  if (!masked) return Buffer.concat([head, payload]);
  head[1] |= 0x80;
  var mask = crypto.randomBytes(4);
  var body = compat.alloc(len);
  for (var i = 0; i < len; i++) body[i] = payload[i] ^ mask[i & 3];
  return Buffer.concat([head, mask, body]);
}

/** Splits bytes into frames: push(bytes) -> [{ fin, opcode, payload }]; a frame over maxBytes throws TOO_BIG. */
function FrameReader(maxBytes) { this.buf = compat.alloc(0); this.max = maxBytes || 1024 * 1024; }
FrameReader.prototype.push = function (d) {
  this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
  var out = [];
  for (;;) {
    var b = this.buf;
    if (b.length < 2) break;
    var masked = (b[1] & 0x80) !== 0, len = b[1] & 0x7f, off = 2;
    if (len === 126) { if (b.length < 4) break; len = b.readUInt16BE(2); off = 4; }
    else if (len === 127) { if (b.length < 10) break; len = b.readUInt32BE(2) * 4294967296 + b.readUInt32BE(6); off = 10; }
    if (len > this.max) throw compat.fail('TOO_BIG', 'a WebSocket frame of ' + len + ' bytes');
    var maskAt = off;
    if (masked) off += 4;
    if (b.length < off + len) break;
    var payload = b.slice(off, off + len);
    if (masked) {
      var copy = compat.alloc(len);
      for (var i = 0; i < len; i++) copy[i] = payload[i] ^ b[maskAt + (i & 3)];
      payload = copy;
    }
    out.push({ fin: (b[0] & 0x80) !== 0, opcode: b[0] & 0x0f, payload: payload });
    this.buf = b.slice(off + len);
  }
  return out;
};

/**
 * One open connection. Events: 'message' (string), 'close' (code), 'error' (Error), 'frame' (opcode).
 * Frames or messages over maxBytes (default 1 MiB) close it with 1009.
 */
function Conn(socket, isClient, head, maxBytes) {
  EventEmitter.call(this);
  var self = this;
  this.socket = socket;
  this.isClient = isClient;
  this.open = true;
  var max = maxBytes || 1024 * 1024;
  var reader = new FrameReader(max);
  var parts = [], size = 0;
  function onData(d) {
    if (!self.open) return;
    var frames;
    try { frames = reader.push(d); } catch (e) { return self.close(1009); }
    for (var i = 0; i < frames.length; i++) {
      var f = frames[i];
      if (self.listeners('frame').length) self.emit('frame', f.opcode, f.payload.length);
      if (f.opcode === 0x8) {
        var code = f.payload.length >= 2 ? f.payload.readUInt16BE(0) : 1005;
        self._write(0x8, f.payload.slice(0, 2));
        self._end(code);
        return;
      }
      if (f.opcode === 0x9) { self._write(0xA, f.payload); continue; }
      if (f.opcode === 0xA) continue;
      if (f.opcode === 0x1 || f.opcode === 0x2 || f.opcode === 0x0) {
        parts.push(f.payload);
        size += f.payload.length;
        if (size > max) return self.close(1009);
        if (f.fin) { var msg = Buffer.concat(parts).toString('utf8'); parts = []; size = 0; self.emit('message', msg); }
      }
    }
  }
  socket.on('data', onData);
  // A socket error (a reset, say) ends the connection; it is reported only to whoever listens for it.
  socket.on('error', function (e) {
    if (self.listeners('error').length) self.emit('error', e);
    self._end(1006);
  });
  socket.on('close', function () { self._end(1006); });
  // Bytes that came with the handshake are handled once the caller has attached its listeners; the socket
  // waits until then so frames stay in order.
  socket.pause();
  setImmediate(function () {
    if (head && head.length) onData(head);
    socket.resume();
  });
}
util.inherits(Conn, EventEmitter);

Conn.prototype._write = function (opcode, payload) {
  if (!this.open) return;
  try { this.socket.write(encodeFrame(opcode, payload, this.isClient)); } catch (e) {}
};
Conn.prototype._end = function (code) {
  if (!this.open) return;
  this.open = false;
  try { this.socket.end(); } catch (e) {}
  this.emit('close', code);
};
Conn.prototype.send = function (text) { this._write(0x1, compat.from(String(text), 'utf8')); };
Conn.prototype.close = function (code) {
  var p = compat.alloc(2); p.writeUInt16BE(code || 1000, 0);
  this._write(0x8, p);
  this._end(code || 1000);
};

/**
 * connect(url, { rejectUnauthorized = true, timeoutMs = 10000, headers?, maxBytes? }) -> Promise<Conn>
 * The TV's wss on 8002 has a self-signed certificate: the caller passes rejectUnauthorized: false.
 */
function connect(url, opts) {
  opts = opts || {};
  var u = urlMod.parse(url);
  var secure = u.protocol === 'wss:';
  var port = Number(u.port) || (secure ? 443 : 80);
  return new Promise(function (resolve, reject) {
    var key = crypto.randomBytes(16).toString('base64');
    var settled = false;
    var socket;
    function fail(e) {
      if (settled) return;
      settled = true;
      try { socket.destroy(); } catch (x) {}
      reject(e);
    }
    function onConnect() {
      var extra = '';
      for (var h in opts.headers || {}) extra += h + ': ' + String(opts.headers[h]).replace(/[\r\n]/g, '') + '\r\n';
      // No Sec-WebSocket-Extensions: frames stay plain (no permessage-deflate).
      socket.write('GET ' + (u.path || '/') + ' HTTP/1.1\r\nHost: ' + u.hostname + ':' + port + '\r\nUpgrade: websocket\r\n' +
        'Connection: Upgrade\r\nSec-WebSocket-Key: ' + key + '\r\nSec-WebSocket-Version: 13\r\n' + extra + '\r\n');
    }
    if (secure) socket = tls.connect({ host: u.hostname, port: port, rejectUnauthorized: opts.rejectUnauthorized !== false }, onConnect);
    else socket = net.createConnection({ host: u.hostname, port: port }, onConnect);
    socket.setTimeout(opts.timeoutMs || 10000, function () { fail(compat.fail('WS_TIMEOUT', 'no WebSocket handshake from ' + u.host)); });
    var buf = compat.alloc(0);
    function onData(d) {
      buf = Buffer.concat([buf, d]);
      var end = buf.toString('binary').indexOf('\r\n\r\n');
      if (end < 0) return;
      socket.removeListener('data', onData);
      socket.setTimeout(0);
      var head = buf.slice(0, end).toString('utf8');
      var rest = buf.slice(end + 4);
      if (!/^HTTP\/1\.[01] 101/.test(head)) return fail(compat.fail('WS_REFUSED', 'WebSocket refused: ' + head.split('\r\n')[0]));
      var m = /\r\nsec-websocket-accept:\s*(\S+)/i.exec(head);
      if (!m || m[1] !== acceptKey(key)) return fail(compat.fail('WS_REFUSED', 'bad WebSocket accept key'));
      // We ask for no extensions (no permessage-deflate); a server that picks one anyway cannot be read.
      if (/\r\nsec-websocket-extensions:/i.test(head)) return fail(compat.fail('WS_REFUSED', 'the server chose a WebSocket extension'));
      settled = true;
      resolve(new Conn(socket, true, rest, opts.maxBytes));
    }
    socket.on('data', onData);
    socket.on('error', function (e) { fail(compat.fail('WS_ERROR', (e.code || e.message) + ' connecting to ' + u.host)); });
    socket.on('close', function () { fail(compat.fail('WS_ERROR', 'closed before the handshake')); });
  });
}

/**
 * Accepts WebSocket upgrades for [path] on an http.Server; onConn(conn, req).
 * opts: { verify?(req) -> bool (false answers 403), maxBytes? }. Extensions asked for are ignored (none offered).
 */
function attach(server, path, onConn, opts) {
  opts = opts || {};
  server.on('upgrade', function (req, socket, head) {
    socket.on('error', function () {});
    var key = req.headers['sec-websocket-key'];
    if (urlMod.parse(req.url).pathname !== path || !key || String(req.headers.upgrade).toLowerCase() !== 'websocket') {
      socket.end('HTTP/1.1 400 Bad Request\r\n\r\n');
      return;
    }
    if (opts.verify && !opts.verify(req)) {
      socket.end('HTTP/1.1 403 Forbidden\r\n\r\n');
      return;
    }
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' +
      acceptKey(key) + '\r\n\r\n');
    onConn(new Conn(socket, false, head, opts.maxBytes), req);
  });
}

module.exports = { connect: connect, attach: attach, encodeFrame: encodeFrame, FrameReader: FrameReader, acceptKey: acceptKey };

};
defs["./remote.js"] = function (module, exports, require) {
// Sends remote-control keys to the TV through its own remote API: wss://127.0.0.1:8002/api/v2/channels/
// samsung.remote.control?name=<base64 name>&token=<token>. The first connection makes the TV ask the person
// to Allow «ReTV Home»; the TV then returns a token in ms.channel.connect, which is stored so later
// connections need no prompt. (From the page itself ws://127.0.0.1:8001 always ends in ms.channel.timeOut on
// a 2021 set even after Allow, which is why keys go through the service.)
'use strict';
var compat = require('./compat.js');
var ws = require('./ws.js');

// Only navigation keys: no HOME, POWER, MENU, SOURCE, volume, channel or number keys.
var ALLOWED = ['KEY_UP', 'KEY_DOWN', 'KEY_LEFT', 'KEY_RIGHT', 'KEY_ENTER', 'KEY_RETURN'];
var MAX_KEYS = 20;       // per sequence
var MAX_QUEUE = 3;       // sequences waiting

function keyName(k) {
  var s = String(k || '').toUpperCase();
  if (s.indexOf('KEY_') !== 0) s = 'KEY_' + s;
  if (ALLOWED.indexOf(s) < 0) throw compat.fail('BAD_KEY', 'not an allowed key: ' + k + ' (allowed: ' + ALLOWED.join(', ') + ')');
  return s;
}

function keyMessage(key) {
  return JSON.stringify({ method: 'ms.remote.control',
    params: { Cmd: 'Click', DataOfCmd: key, Option: 'false', TypeOfRemote: 'SendRemoteKey' } });
}

/**
 * Remote({ store, open?(url, opts) -> Promise<Conn>, url? = 'wss://127.0.0.1:8002', name? = 'ReTV Home',
 *          allowTimeoutMs? = 30000, wait?(ms), log?(m) })
 *   sendKeys(keys, gapMs = 400, owner?) -> { sent }
 *     errors BAD_KEY, REMOTE_BUSY (queue full, or locked by another owner), CANCELLED, REMOTE_DENIED,
 *     REMOTE_TIMEOUT, REMOTE_DOWN
 *   lock(owner) / unlock(owner): while locked only that owner's sequences run (a privacy pass is atomic).
 *   cancel() stops the running sequence before its next key and drops the queued ones.
 *   status() -> { paired, connected, queued, locked }      forget() drops the stored token.
 */
function Remote(opts) {
  this.store = opts.store;
  this.open = opts.open || function (url, o) { return ws.connect(url, o); };
  this.base = opts.url || 'wss://127.0.0.1:8002';
  this.name = opts.name || 'ReTV Home';
  this.allowTimeoutMs = opts.allowTimeoutMs || 30000;
  this.wait = opts.wait || compat.wait;
  this.log = opts.log || function () {};
  this.owner = null;
  this.queued = 0;
  this.generation = 0;
  this.conn = null;
  this._connecting = null;
  this._chain = Promise.resolve();
}

Remote.prototype.url = function () {
  var token = this.store.get('remoteToken');
  return this.base + '/api/v2/channels/samsung.remote.control?name=' +
    encodeURIComponent(compat.from(this.name, 'utf8').toString('base64')) + (token ? '&token=' + encodeURIComponent(token) : '');
};

Remote.prototype._connect = function () {
  var self = this;
  if (this.conn && this.conn.open) return Promise.resolve(this.conn);
  if (this._connecting) return this._connecting;
  this.log('remote: connecting to ' + this.base + (this.store.get('remoteToken') ? ' with a token' : ' without a token (the TV asks to Allow)'));
  this._connecting = this.open(this.url(), { rejectUnauthorized: false, timeoutMs: 10000 }).then(function (conn) {
    self.log('remote: socket open, waiting for ms.channel.connect');
    conn.on('frame', function (op, len) { if (op !== 0x1) self.log('remote: frame opcode ' + op + ', ' + len + ' bytes'); });
    conn.on('close', function (code) { self.log('remote: closed (' + code + ')'); });
    return new Promise(function (resolve, reject) {
      var settled = false;
      // Settle first, then close: closing emits 'close' at once, which must not win with REMOTE_DOWN.
      function fail(code, message) {
        if (settled) return;
        settled = true; clearTimeout(timer);
        reject(compat.fail(code, message));
        conn.close();
      }
      var timer = setTimeout(function () { fail('REMOTE_TIMEOUT', 'the TV did not answer; was «Allow» pressed?'); }, self.allowTimeoutMs);
      function onMessage(text) {
        var msg;
        try { msg = JSON.parse(text); } catch (e) { self.log('remote: a frame that is not JSON (' + String(text).length + ' chars)'); return; }
        self.log('remote: event ' + (msg && msg.event));
        if (settled) return;
        if (msg.event === 'ms.channel.connect') {
          settled = true; clearTimeout(timer);
          conn.removeListener('message', onMessage);
          if (msg.data && msg.data.token) self.store.set('remoteToken', String(msg.data.token));
          self.conn = conn;
          conn.on('close', function () { if (self.conn === conn) self.conn = null; });
          resolve(conn);
        } else if (msg.event === 'ms.channel.unauthorized') {
          self.store.set('remoteToken', undefined);
          fail('REMOTE_DENIED', 'the TV refused remote control (Deny pressed, or the device is blocked in Device Connection Manager)');
        } else if (msg.event === 'ms.channel.timeOut') {
          fail('REMOTE_TIMEOUT', 'the TV\'s Allow prompt timed out');
        }
      }
      conn.on('message', onMessage);
      conn.on('close', function () { fail('REMOTE_DOWN', 'the TV closed the remote channel'); });
    });
  }, function (e) {
    self.log('remote: cannot connect: ' + e.message);
    throw compat.fail('REMOTE_DOWN', 'cannot reach the TV\'s remote API: ' + e.message);
  });
  var clear = function () { self._connecting = null; };
  this._connecting.then(clear, clear);
  return this._connecting;
};

Remote.prototype.sendKeys = function (keys, gapMs, owner) {
  var self = this;
  var gap = gapMs === undefined ? 400 : Math.max(0, Math.min(5000, Number(gapMs) || 0));
  var list;
  try {
    if (!keys || !keys.length || keys.length > MAX_KEYS) throw compat.fail('BAD_KEY', 'keys: 1 to ' + MAX_KEYS + ' keys');
    list = [];
    for (var i = 0; i < keys.length; i++) list.push(keyName(keys[i]));
  } catch (e) { return Promise.reject(e); }
  if (this.owner && this.owner !== owner) return Promise.reject(compat.fail('REMOTE_BUSY', 'the remote is in use by ' + this.owner));
  if (this.queued >= MAX_QUEUE) return Promise.reject(compat.fail('REMOTE_BUSY', 'too many key sequences waiting'));
  this.queued++;
  var gen = this.generation;
  // One key sequence at a time, so two callers never interleave their presses.
  var run = function () {
    self.queued--;
    if (gen !== self.generation) throw compat.fail('CANCELLED', 'cancelled');
    if (self.owner && self.owner !== owner) throw compat.fail('REMOTE_BUSY', 'the remote is in use by ' + self.owner);
    return self._connect().then(function (conn) {
      var sent = 0;
      function next() {
        if (sent === list.length) return { sent: sent };
        if (gen !== self.generation) throw compat.fail('CANCELLED', 'cancelled after ' + sent + ' keys');
        if (!conn.open) throw compat.fail('REMOTE_DOWN', 'the remote channel closed after ' + sent + ' keys');
        conn.send(keyMessage(list[sent]));
        sent++;
        return (sent < list.length ? self.wait(gap) : Promise.resolve()).then(next);
      }
      return next();
    });
  };
  var p = this._chain.then(run, run);
  this._chain = p.then(function () {}, function () {});
  return p;
};

Remote.prototype.lock = function (owner) {
  if (this.owner && this.owner !== owner) throw compat.fail('REMOTE_BUSY', 'the remote is in use by ' + this.owner);
  this.owner = owner;
};
Remote.prototype.unlock = function (owner) { if (this.owner === owner) this.owner = null; };

Remote.prototype.cancel = function () {
  this.generation++;
  this.log('remote: cancelled');
  return this.status();
};

Remote.prototype.status = function () {
  return { paired: !!this.store.get('remoteToken'), connected: !!(this.conn && this.conn.open), queued: this.queued, locked: this.owner };
};

Remote.prototype.forget = function () {
  this.store.set('remoteToken', undefined);
  if (this.conn) this.conn.close();
};

module.exports = { Remote: Remote, keyName: keyName, keyMessage: keyMessage, ALLOWED: ALLOWED };

};
defs["./privacy.js"] = function (module, exports, require) {
// Turns off Samsung's interest-based ads / viewing information through Privacy Choices, state-safe:
// it reads the state first and toggles only when ads are on, reads it again after every pass, and stops
// after two passes («agree to all» may go from some-on to all-on on the first pass).
//
// Key path, verified on a 2021 QE55Q67AA: Privacy Choices (tv6pbVMQFF.PrivacyChoices) opens with focus on
// «Terms & Conditions, Privacy Policy»; ENTER opens the terms screen (about 6 s to load) with focus on
// «agree to all»; ENTER toggles it; DOWN reaches OK; ENTER on OK saves and returns.
//
// Keys go only while Privacy Choices is in front: screen() is asked before the first key and before the
// save keys, and the run stops (LEFT_SCREEN) when something else is in front. Each pass holds the remote
// lock, so no other caller's keys land between its presses. Timings are fixed here (tests may pass their
// own through the constructor, clamped); callers of privacy.run cannot change them.
//
// Injected: page.openPrivacy() launches the screen, page.readAdsOff() answers webapis.adinfo.isLATEnabled()
// (true = ads off); screen() -> 'privacy' | 'home' | 'other' | 'unknown'; sendKeys(keys, gap, owner);
// lock(owner) / unlock(owner).
'use strict';
var compat = require('./compat.js');

var OWNER = 'privacy';
var MAX_PASSES = 2;

// name: [default, min, max] in ms
var TIMINGS = {
  openTimeoutMs: [10000, 3000, 20000],   // Privacy Choices in front after launch
  settleMs: [1500, 500, 5000],           // its focus is ready
  termsLoadMs: [6000, 3000, 15000],      // the terms screen after ENTER
  keyGapMs: [700, 300, 2000],            // between ENTER (agree to all), DOWN, ENTER (OK)
  returnTimeoutMs: [10000, 2000, 20000], // the TV saved and left Privacy Choices
  pollMs: [500, 10, 2000]
};

var CODES = {
  ALREADY_OFF: 'ads were already off; nothing was pressed',
  TURNED_OFF: 'ads are off now',
  STILL_ON: 'ads are still on after the last pass',
  READ_FAILED: 'the page could not read the ad state',
  OPEN_FAILED: 'Privacy Choices did not open or did not come to the front',
  LEFT_SCREEN: 'something else came to the front; the keys stopped',
  KEYS_FAILED: 'the remote keys could not be sent',
  BUSY: 'a privacy run is already going'
};

function clampTimings(t) {
  var out = {};
  for (var k in TIMINGS) {
    var d = TIMINGS[k], v = t && typeof t[k] === 'number' && isFinite(t[k]) ? t[k] : d[0];
    out[k] = Math.max(d[1], Math.min(d[2], v));
  }
  return out;
}

function tag(e, code) {
  if (!(e instanceof Error)) e = new Error(String(e));
  if (!e.outcome) e.outcome = code;
  return e;
}

/**
 * Privacy({ page, screen?, sendKeys, lock?, unlock?, wait?, timings?, onStep?, log? })
 *   run() -> Promise<{ code, passes, adsOff, before, error? }>   never rejects
 */
function Privacy(deps) {
  this.page = deps.page;
  this.screen = deps.screen || function () { return Promise.resolve('unknown'); };
  this.sendKeys = deps.sendKeys;
  this.lock = deps.lock || function () {};
  this.unlock = deps.unlock || function () {};
  this.wait = deps.wait || compat.wait;
  this.t = clampTimings(deps.timings);
  this.onStep = deps.onStep || function () {};
  this.log = deps.log || function () {};
  this.running = false;
}

/** Polls screen() until want(s) or the timeout; resolves with the last answer. */
Privacy.prototype._until = function (want, timeoutMs) {
  var self = this, spent = 0;
  function poll() {
    return self.screen().then(null, function () { return 'unknown'; }).then(function (s) {
      if (want(s) || s === 'unknown' || spent >= timeoutMs) return s;
      spent += self.t.pollMs;
      return self.wait(self.t.pollMs).then(poll);
    });
  }
  return poll();
};

Privacy.prototype.run = function () {
  var self = this, t = this.t;
  if (this.running) return Promise.resolve({ code: 'BUSY', passes: 0, adsOff: null, before: null });
  this.running = true;
  var result = { code: null, passes: 0, adsOff: null, before: null };
  function step(name) { self.onStep(name, { pass: result.passes }); }
  function stop(code, err) {
    result.code = code;
    if (err) result.error = (err.code ? err.code + ': ' : '') + (err.message || String(err));
    return result;
  }
  function read(isBefore) {
    step(isBefore ? 'read-before' : 'read-after');
    return self.page.readAdsOff().then(function (off) {
      if (typeof off !== 'boolean') throw compat.fail('READ_FAILED', 'adinfo answered ' + off);
      result.adsOff = off;
      if (isBefore) result.before = off;
      return off;
    }, function (e) { throw tag(e, 'READ_FAILED'); });
  }
  function keys(list, gap) {
    return self.sendKeys(list, gap, OWNER).then(null, function (e) { throw tag(e, 'KEYS_FAILED'); });
  }
  function inFront(s) {
    if (s === 'privacy') return;
    if (s === 'unknown') { self.log('privacy: cannot tell what is in front; going on by time'); return; }
    throw tag(compat.fail('NOT_IN_FRONT', 'in front: ' + s), 'LEFT_SCREEN');
  }
  function pass() {
    result.passes++;
    step('open');
    try { self.lock(OWNER); } catch (e) { return Promise.reject(tag(e, 'KEYS_FAILED')); }
    return self.page.openPrivacy().then(null, function (e) { throw tag(e, 'OPEN_FAILED'); }).then(function () {
      return self._until(function (s) { return s === 'privacy'; }, t.openTimeoutMs);
    }).then(function (s) {
      if (s !== 'privacy' && s !== 'unknown') throw tag(compat.fail('NOT_IN_FRONT', 'Privacy Choices is not in front (' + s + ')'), 'OPEN_FAILED');
      return self.wait(s === 'unknown' ? t.settleMs * 2 : t.settleMs);
    }).then(function () {
      return self.screen().then(null, function () { return 'unknown'; });
    }).then(function (s) {
      inFront(s);
      step('open-terms');
      return keys(['KEY_ENTER'], 0);
    }).then(function () {
      return self.wait(t.termsLoadMs);
    }).then(function () {
      return self.screen().then(null, function () { return 'unknown'; });
    }).then(function (s) {
      inFront(s);
      step('toggle-and-save');
      return keys(['KEY_ENTER', 'KEY_DOWN', 'KEY_ENTER'], t.keyGapMs);
    }).then(function () {
      self.unlock(OWNER);
      return self._until(function (s) { return s !== 'privacy'; }, t.returnTimeoutMs);
    }).then(function () {
      return read(false);
    }).then(function (off) {
      if (off) return stop('TURNED_OFF');
      if (result.passes >= MAX_PASSES) return stop('STILL_ON');
      return pass();
    }, function (e) { self.unlock(OWNER); throw e; });
  }
  return read(true).then(function (off) {
    if (off) return stop('ALREADY_OFF');
    return pass();
  }).then(null, function (e) {
    e = e instanceof Error ? e : new Error(String(e));
    return stop(e.outcome || 'READ_FAILED', e);
  }).then(function (r) {
    self.unlock(OWNER);
    self.running = false;
    step('done');
    return r;
  });
};

module.exports = { Privacy: Privacy, TIMINGS: TIMINGS, CODES: CODES, clampTimings: clampTimings, MAX_PASSES: MAX_PASSES };

};
defs["./relay.js"] = function (module, exports, require) {
// sdb relay: listens on the LAN (default 26201) and forwards each connection to the TV's own sdbd on
// 127.0.0.1:26101. Once Developer Mode's Host PC IP is 127.0.0.1, sdbd only answers loopback clients; the
// relay lets the Mac's ReTV Bridge still reach it. Idea from Apps2Samsung TV-Store's relay (see NOTICE);
// this code is ReTV's own. Off by default; only the IPs it was told to allow get through (an empty allow
// list lets nobody in), with a cap on live sessions and an idle timeout so sdbd's slots are not held.
'use strict';
var net = require('net');

var IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;

/** A comma string or array of IPv4 addresses -> clean array; throws BAD_ALLOW on anything else. */
function normAllow(allow) {
  if (!allow) return [];
  if (typeof allow === 'string') allow = allow.split(',');
  if (!Array.isArray(allow)) { var e = new Error('allow: a list of IPv4 addresses'); e.code = 'BAD_ALLOW'; throw e; }
  var out = [];
  for (var i = 0; i < allow.length; i++) {
    var ip = String(allow[i]).trim();
    if (!ip) continue;
    if (!IPV4.test(ip)) { var x = new Error('not an IPv4 address: ' + ip); x.code = 'BAD_ALLOW'; throw x; }
    if (out.indexOf(ip) < 0) out.push(ip);
  }
  return out;
}

function peerIp(socket) {
  return String((socket && socket.remoteAddress) || '').replace(/^::ffff:/, '');
}

/** Relay({ bindHost = '0.0.0.0', sdbHost = '127.0.0.1', sdbPort = 26101, maxSessions = 8, idleMs = 30 min, log? }) */
function Relay(opts) {
  opts = opts || {};
  this.bindHost = opts.bindHost || '0.0.0.0';
  this.sdbHost = opts.sdbHost || '127.0.0.1';
  this.sdbPort = opts.sdbPort || 26101;
  this.maxSessions = opts.maxSessions || 8;
  this.idleMs = opts.idleMs === undefined ? 30 * 60 * 1000 : opts.idleMs;
  this.log = opts.log || function () {};
  this.allow = [];
  this.server = null;
  this.port = null;
  this.sessions = [];
  this.refused = 0;
  this.served = 0;
  this.lastError = null;
}

Relay.prototype._accept = function (client) {
  var self = this;
  var ip = peerIp(client);
  if (this.allow.indexOf(ip) < 0) {
    this.refused++;
    this.log('relay: refused ' + ip + ' (not allowed)');
    return client.destroy();
  }
  if (this.sessions.length >= this.maxSessions) {
    this.refused++;
    this.log('relay: refused ' + ip + ' (too many sessions)');
    return client.destroy();
  }
  var upstream = net.createConnection({ host: this.sdbHost, port: this.sdbPort });
  var s = { client: client, upstream: upstream, ip: ip };
  this.sessions.push(s);
  this.served++;
  var gone = false;
  function end() {
    if (gone) return;
    gone = true;
    try { client.destroy(); } catch (e) {}
    try { upstream.destroy(); } catch (e) {}
    var i = self.sessions.indexOf(s);
    if (i >= 0) self.sessions.splice(i, 1);
  }
  client.pipe(upstream);
  upstream.pipe(client);
  client.on('error', end); client.on('close', end);
  upstream.on('error', function (e) { self.lastError = (e.code || e.message) + ' from sdbd'; end(); });
  upstream.on('close', end);
  if (this.idleMs > 0) client.setTimeout(this.idleMs, end);
};

/** start(port, allow) -> Promise<status>. Restarts when already listening (settings changed). */
Relay.prototype.start = function (port, allow) {
  var self = this;
  var list = normAllow(allow);
  this.stop();
  this.allow = list;
  return new Promise(function (resolve, reject) {
    var server = net.createServer(function (c) { self._accept(c); });
    server.once('error', function (e) {
      self.server = null;
      self.lastError = 'cannot listen on ' + port + ': ' + (e.code || e.message);
      e.code = 'RELAY_LISTEN'; reject(e);
    });
    server.listen(port, self.bindHost, function () {
      self.server = server;
      self.port = server.address().port;
      self.log('relay: ' + self.bindHost + ':' + self.port + ' -> ' + self.sdbHost + ':' + self.sdbPort + ', allow ' + (list.join(', ') || 'nobody'));
      resolve(self.status());
    });
  });
};

Relay.prototype.stop = function () {
  var live = this.sessions.slice();
  for (var i = 0; i < live.length; i++) { try { live[i].client.destroy(); live[i].upstream.destroy(); } catch (e) {} }
  this.sessions = [];
  if (this.server) { try { this.server.close(); } catch (e) {} }
  this.server = null;
  this.port = null;
};

Relay.prototype.status = function () {
  return { listening: !!this.server, port: this.port, allow: this.allow.slice(), sessions: this.sessions.length,
    served: this.served, refused: this.refused, lastError: this.lastError };
};

module.exports = { Relay: Relay, normAllow: normAllow, peerIp: peerIp };

};
defs["./returnwatch.js"] = function (module, exports, require) {
// «Return to ReTV»: after ReTV Home opened an app, bring ReTV back when the person leaves that app (Back or
// Exit lands on Samsung's Smart Hub otherwise). Without fighting the person:
//   - it waits until the app has been seen in front (gives up after launchTimeoutMs), and notes what was
//     running at that moment;
//   - it acts only when the app has been out of the front for hiddenMs and the person did not move on:
//     a system screen (Live TV, a source / HDMI, Settings, ...) that came up or is in front, another Smart
//     Hub app in front, or any app started since the watched one was seen all mean «moved on»: it stops;
//   - when it cannot see what runs (no getAppsContext) it does nothing (NO_CONTEXT);
//   - ReTV in front after the app was seen ends the watch; a newer watchReturn replaces the older one;
//     the setting can turn it off.
// Many apps stay running in the background after Back (multitasking), so "out of the front" is what counts.
// A Home press inside the app also leaves it out of the front; then ReTV comes back too.
'use strict';

var HOME = 'RetvHome00.ReTVHome';

// Best guesses for Samsung's system screens (Tizen ids); verify on the TV with returnDebug and correct
// them at run time with setReturnLists. Anything that starts while the app is in front counts as well.
var SYSTEM_SCREENS = ['org.tizen.tv-viewer', 'org.tizen.menu', 'org.tizen.source-manager', 'org.tizen.volt.source',
  'com.samsung.tv.source', 'com.samsung.tv.settings', 'org.tizen.screen-mirroring', 'com.samsung.tv.channel-list'];

/**
 * ReturnWatch({ status(tizenId) -> Promise<{ running, visible } | null>, running() -> Promise<[tizenId]>,
 *               userApps() -> Promise<[tizenId]> (Smart Hub list), launchHome() -> Promise, store,
 *               now?(), log?(m), homeId?, intervalMs? = 700, hiddenMs? = 1400, launchTimeoutMs? = 20000,
 *               maxMs? = 6 h, timers? = true })
 *   watch(appId) -> status        setOn(bool) -> status        status() -> { on, watching, appId, last }
 *   setLists({ systemScreens?, ignore? }) -> lists     lists() -> { systemScreens, ignore }
 *   tick() -> Promise             one poll (the timer calls it; tests call it directly)
 *   last.code: RETURNED, OTHER_APP, MOVED_ON, HOME_IN_FRONT, NOT_SEEN, NO_CONTEXT, EXPIRED, REPLACED,
 *              TURNED_OFF, STOPPED, LAUNCH_FAILED
 */
function ReturnWatch(o) {
  this.o = o;
  this.homeId = o.homeId || HOME;
  this.now = o.now || function () { return Date.now(); };
  this.log = o.log || function () {};
  this.intervalMs = o.intervalMs || 700;
  this.hiddenMs = o.hiddenMs === undefined ? 1400 : o.hiddenMs;
  this.launchTimeoutMs = o.launchTimeoutMs || 20000;
  this.maxMs = o.maxMs || 6 * 3600 * 1000;
  this.w = null;          // { appId, since, seen, hiddenSince, before }
  this.last = null;
  this.timer = null;
  this.busy = false;
  this.ticks = [];
}

ReturnWatch.prototype.isOn = function () { return this.o.store.get('returnToReTV') !== false; };

ReturnWatch.prototype.lists = function () {
  var l = this.o.store.get('returnLists') || {};
  return { systemScreens: l.systemScreens || SYSTEM_SCREENS.slice(), ignore: l.ignore || [] };
};

ReturnWatch.prototype.setLists = function (a) {
  var cur = this.lists();
  function clean(v) {
    if (!Array.isArray(v) || v.length > 100) { var e = new Error('a list of up to 100 app ids'); e.code = 'BAD_LIST'; throw e; }
    return v.map(String).filter(function (id) { return /^[A-Za-z0-9._-]{1,128}$/.test(id); });
  }
  if (a.systemScreens !== undefined) cur.systemScreens = clean(a.systemScreens);
  if (a.ignore !== undefined) cur.ignore = clean(a.ignore);
  this.o.store.set('returnLists', cur);
  return cur;
};

ReturnWatch.prototype.status = function () {
  return { on: this.isOn(), watching: !!this.w, appId: this.w ? this.w.appId : null, last: this.last };
};

ReturnWatch.prototype._stop = function (code, detail) {
  if (this.w) {
    this.last = { code: code, appId: this.w.appId, at: new Date(this.now()).toISOString() };
    if (detail) this.last.detail = detail;
    this.log('return: ' + code + ' ' + this.w.appId + (detail ? ' (' + detail + ')' : ''));
  }
  this.w = null;
  if (this.timer) { clearInterval(this.timer); this.timer = null; }
};

ReturnWatch.prototype.watch = function (appId) {
  if (typeof appId !== 'string' || !/^[A-Za-z0-9._-]{1,128}$/.test(appId)) {
    var e = new Error('not an app id: ' + appId); e.code = 'BAD_ID'; throw e;
  }
  if (!this.isOn() || appId === this.homeId) return this.status();
  if (this.w) this._stop('REPLACED');
  this.w = { appId: appId, since: this.now(), seen: false, hiddenSince: null, before: null };
  var self = this;
  if (this.o.timers !== false) this.timer = setInterval(function () { self.tick(); }, this.intervalMs);
  return this.status();
};

ReturnWatch.prototype.setOn = function (on) {
  this.o.store.set('returnToReTV', !!on);
  if (!on && this.w) this._stop('TURNED_OFF');
  return this.status();
};

// Why not to go back, or null: the person moved on to something else.
ReturnWatch.prototype._movedOn = function (w, running) {
  var self = this, lists = this.lists();
  var skip = [w.appId, this.homeId].concat(lists.ignore);
  var others = running.filter(function (id) { return skip.indexOf(id) < 0; });
  var system = others.filter(function (id) { return lists.systemScreens.indexOf(id) >= 0; });
  var fresh = others.filter(function (id) { return w.before.indexOf(id) < 0; });
  return this.o.userApps().then(null, function () { return []; }).then(function (users) {
    var freshSystem = fresh.filter(function (id) { return system.indexOf(id) >= 0; });
    if (freshSystem.length) return ['MOVED_ON', freshSystem[0]];
    var freshUser = fresh.filter(function (id) { return users.indexOf(id) >= 0; });
    if (freshUser.length) return ['OTHER_APP', freshUser[0]];
    if (fresh.length) return ['MOVED_ON', fresh[0]];
    // Already-running screens: ask whether one of them is in front now.
    var check = system.concat(others.filter(function (id) { return users.indexOf(id) >= 0 && system.indexOf(id) < 0; })).slice(0, 12);
    return Promise.all(check.map(function (id) {
      return self.o.status(id).then(function (s) { return s && s.visible ? id : null; }, function () { return null; });
    })).then(function (vis) {
      for (var i = 0; i < vis.length; i++) if (vis[i]) return [system.indexOf(vis[i]) >= 0 ? 'MOVED_ON' : 'OTHER_APP', vis[i]];
      return null;
    });
  });
};

// The last ticks, for returnDebug: { at, appId, running, app, home, decision } (app/home: REST visible or null).
ReturnWatch.prototype.history = function () { return this.ticks.slice(); };

ReturnWatch.prototype._record = function (h) {
  this.ticks.push(h);
  if (this.ticks.length > 30) this.ticks.shift();
};

// One poll. The app counts as there when the REST API says it is visible, or, when the REST API does not
// know its id, while its context is in getAppsContext (then «gone» means the person exited it).
ReturnWatch.prototype.tick = function () {
  var self = this, w = this.w;
  if (!w || this.busy) return Promise.resolve();
  if (!this.isOn()) { this._stop('TURNED_OFF'); return Promise.resolve(); }
  var t = this.now();
  if (t - w.since > this.maxMs) { this._stop('EXPIRED'); return Promise.resolve(); }
  this.busy = true;
  var h = { at: new Date(t).toISOString(), appId: w.appId, running: null, app: null, home: null, decision: '' };
  function stop(code, detail) { h.decision = code; self._stop(code, detail); }
  function done() { self.busy = false; self._record(h); }
  return Promise.all([
    this.o.status(w.appId).then(null, function () { return null; }),
    this.o.status(this.homeId).then(null, function () { return null; }),
    this.o.running().then(null, function () { return null; })
  ]).then(function (r) {
    if (self.w !== w) { h.decision = 'replaced'; return; }
    var app = r[0], home = r[1], ids = r[2];
    h.running = ids; h.app = app ? app.visible : null; h.home = home ? home.visible : null;
    // ReTV in front once the app has run: the person is back (also while the app still runs behind).
    if (w.seen && home && home.visible) return stop('HOME_IN_FRONT');
    var present = app ? app.visible : !!(ids && ids.indexOf(w.appId) >= 0);
    if (present) {
      w.hiddenSince = null;
      if (w.seen) { h.decision = app ? 'in-front' : 'running'; return; }
      w.seen = true; w.before = ids;
      h.decision = 'seen';
      return;
    }
    if (!w.seen) {
      if (t - w.since > self.launchTimeoutMs) return stop('NOT_SEEN');
      h.decision = 'waiting';
      return;
    }
    if (w.hiddenSince === null) w.hiddenSince = t;
    if (t - w.hiddenSince < self.hiddenMs) { h.decision = app ? 'hidden' : 'exited'; return; }
    if (!w.before || !ids) return stop('NO_CONTEXT', 'cannot list running apps');
    return self._movedOn(w, ids).then(function (why) {
      if (self.w !== w) return;
      if (why) return stop(why[0], why[1]);
      stop('RETURNED');
      return self.o.launchHome().then(null, function (e) {
        self.last.code = 'LAUNCH_FAILED'; self.last.error = (e && e.message) || String(e);
        h.decision = 'LAUNCH_FAILED';
      });
    });
  }).then(done, function (e) { h.decision = 'error: ' + (e && e.message); done(); });
};

module.exports = { ReturnWatch: ReturnWatch, HOME: HOME, SYSTEM_SCREENS: SYSTEM_SCREENS };

};
defs["./tvstate.js"] = function (module, exports, require) {
// What is running and what is in front, from the service:
//  - running apps: tizen.application.getAppsContext() (Tizen app ids);
//  - in front: the TV's REST API, GET http://127.0.0.1:8001/api/v2/applications/<id> -> { running, visible }.
//    The REST API knows Smart Hub apps by their store app_id (3201907018807 for Netflix, RetvHome00 for
//    ReTV), so Tizen ids are mapped through the vd_applist ids first; an id it does not know answers null.
//    Without sdb (Host PC IP not yet 127.0.0.1) the last vd_applist mapping kept in the store is used, then a
//    built-in table of well-known apps.
'use strict';

// Tizen id -> store app_id, verified on the 2021 QE55Q67AA (region PL).
var KNOWN = {
  'org.tizen.netflix-app': '3201907018807',
  '9Ur5IzDKqV.TizenYouTube': '111299001912',
  'org.tizen.primevideo': '3201910019365',
  'vbUQClczfR.Wuakitv': '3201511006428',
  'com.samsung.tv.gallery': '3201710015037',
  'EUVXZqucqz.makoTV': '3201506003716',
  'org.tizen.browser': '3202010022079',
  'MCmYXNxgcu.DisneyPlus': '3201901017640',
  'niYSnzL6h1.Viaplay': '11111300404',
  'rJeHak5zRg.Spotify': '3201606009684',
  'com.samsung.tv.aria-video': '3201807016597'
};

/**
 * TvState({ tizen?, getJson(url), appList?() -> Promise<[{ id, tizenId }]>, store?, now?() })
 *   running() -> Promise<[tizenId]>     (rejects without getAppsContext)
 *   status(tizenId) -> Promise<{ running, visible } | null>
 *   restId(tizenId) -> Promise<string>
 */
function TvState(o) {
  this.tizen = o.tizen;
  this.getJson = o.getJson;
  this.appList = o.appList || function () { return Promise.resolve([]); };
  this.store = o.store || null;
  this.ids = null;          // tizenId -> REST id, from vd_applist
  this.idsAt = 0;
  this.failedAt = null;     // when vd_applist last failed: not asked again for a minute
  this.now = o.now || function () { return Date.now(); };
}

TvState.prototype.running = function () {
  var app = this.tizen && this.tizen.application;
  if (!app || typeof app.getAppsContext !== 'function') return Promise.reject(new Error('no getAppsContext'));
  return new Promise(function (resolve, reject) {
    try {
      app.getAppsContext(function (cs) { resolve(cs.map(function (c) { return c.appId; })); }, reject);
    } catch (e) { reject(e); }
  });
};

TvState.prototype._map = function () {
  var self = this, t = this.now();
  if (this.ids && t - this.idsAt < 10 * 60 * 1000) return Promise.resolve(this.ids);
  if (this.failedAt !== null && t - this.failedAt < 60 * 1000) return Promise.resolve(this.ids || {});
  return this.appList().then(function (list) {
    var m = {};
    for (var i = 0; i < list.length; i++) if (list[i].tizenId) m[list[i].tizenId] = list[i].id;
    self.ids = m; self.idsAt = self.now(); self.failedAt = null;
    if (self.store) self.store.set('appIdMap', m);
    return m;
  }, function () { self.failedAt = self.now(); return self.ids || {}; });
};

TvState.prototype.restId = function (tizenId) {
  var self = this;
  // ReTV Home's REST id is its package id (vd_applist app_id RetvHome00).
  if (/^RetvHome00\./.test(tizenId)) return Promise.resolve('RetvHome00');
  return this._map().then(function (m) {
    var kept = (self.store && self.store.get('appIdMap')) || {};
    return m[tizenId] || kept[tizenId] || KNOWN[tizenId] || tizenId;
  });
};

TvState.prototype.status = function (tizenId) {
  var self = this;
  return this.restId(tizenId).then(function (id) {
    return self.getJson('http://127.0.0.1:8001/api/v2/applications/' + encodeURIComponent(id));
  }).then(function (j) {
    if (!j || typeof j.visible !== 'boolean') return null;
    return { running: !!j.running, visible: j.visible };
  }, function () { return null; });
};

module.exports = { TvState: TvState, KNOWN: KNOWN };

};
defs["./auth.js"] = function (module, exports, require) {
// Who may talk to the service. Every other app (and any web page) on the TV can open 127.0.0.1:8095, so:
//  1. a per-install secret: made on first start, kept in the service's private data folder, and handed out
//     only over a *trusted* Tizen message port (only apps signed with ReTV's certificate can use one), and
//     only to ReTV Home's own app id. The page shows it in the first WebSocket message.
//  2. the WebSocket Origin as a second layer: web origins (http, https, extensions) are refused.
'use strict';
var crypto = require('crypto');

var PORT_NAME = 'retv.auth';
var HOME = 'RetvHome00.ReTVHome';

/** The install's secret (48 hex chars), made and stored on first use. */
function secret(store) {
  var s = store.get('secret');
  if (typeof s !== 'string' || s.length < 32) {
    s = crypto.randomBytes(24).toString('hex');
    store.set('secret', s);
  }
  return s;
}

/** Constant-time comparison of two strings. */
function same(a, b) {
  if (typeof a !== 'string' || typeof b !== 'string' || a.length !== b.length) return false;
  var d = 0;
  for (var i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return d === 0;
}

/**
 * Whether a WebSocket Origin header may connect. A packaged Tizen web app sends file:// (or a null /
 * app-scheme origin); a web page or a hosted app sends http(s)://. A native client may send none; the
 * secret still stands in its way.
 */
function originOk(origin) {
  if (origin === undefined || origin === null || origin === '' || origin === 'null') return true;
  var o = String(origin).toLowerCase();
  return o.indexOf('file:') === 0 || o.indexOf('app:') === 0 || o.indexOf('wgt:') === 0;
}

/**
 * Hands the secret to ReTV Home over a trusted local message port. The page sends any message with its
 * own trusted local port as the reply port; the reply goes only to HOME on a trusted port.
 * serve({ tizen, getSecret(), homeId?, log? }) -> true when listening, false when message ports are missing.
 */
function serve(o) {
  var log = o.log || function () {};
  var homeId = o.homeId || HOME;
  var mp = o.tizen && o.tizen.messageport;
  if (!mp || typeof mp.requestTrustedLocalMessagePort !== 'function') { log('auth: no message ports'); return false; }
  try {
    var port = mp.requestTrustedLocalMessagePort(PORT_NAME);
    port.addMessagePortListener(function (data, reply) {
      if (!reply || reply.appId !== homeId || reply.isTrusted === false) {
        log('auth: refused a secret request from ' + (reply ? reply.appId + (reply.isTrusted ? '' : ' (untrusted)') : 'an app without a reply port'));
        return;
      }
      try { reply.sendMessage([{ key: 'secret', value: o.getSecret() }]); log('auth: secret sent to ' + reply.appId); }
      catch (e) { log('auth: cannot reply: ' + e.message); }
    });
    log('auth: trusted message port ' + PORT_NAME + ' ready');
    return true;
  } catch (e) { log('auth: message port failed: ' + e.message); return false; }
}

module.exports = { secret: secret, same: same, originOk: originOk, serve: serve, PORT_NAME: PORT_NAME, HOME: HOME };

};
defs["./rpc.js"] = function (module, exports, require) {
// The JSON protocol between the ReTV Home page and the service (see ../PROTOCOL.md).
//   first message: { id, cmd: "auth", args: { secret } }  -> { id, ok: true, result: { version } }, else closed
//   page -> service: { id, cmd, args }            service -> page: { id, ok: true, result } | { id, ok: false, error: { code, message } }
//   service -> page: { id: "s<n>", cmd, args }    page -> service: { id: "s<n>", ok, result | error }   (page.* calls)
//   service -> page: { event, data }              progress, no answer expected
// Only authenticated connections get answers, page.* requests and events.
'use strict';
var compat = require('./compat.js');
var auth = require('./auth.js');

/** Hub({ handlers, secret() -> string, version?, pageTimeoutMs = 60000, authTimeoutMs = 5000, log? }) */
function Hub(opts) {
  this.handlers = opts.handlers || {};
  this.secret = opts.secret;
  this.version = opts.version || null;
  this.pageTimeoutMs = opts.pageTimeoutMs || 60000;
  this.authTimeoutMs = opts.authTimeoutMs || 5000;
  this.log = opts.log || function () {};
  this.conns = [];           // authenticated connections only
  this.page = null;          // the latest authenticated connection: page.* requests go there
  this.pending = {};         // id -> { msg, resolve, reject, timer }
  this.seq = 0;
}

function errorOf(e) {
  return { code: (e && e.code) || 'ERROR', message: (e && e.message) || String(e) };
}

/** Answers one request object from an authenticated connection; resolves with the reply (never rejects). */
Hub.prototype.handle = function (msg) {
  var self = this;
  var id = msg && msg.id !== undefined ? msg.id : null;
  return new Promise(function (resolve) {
    if (!msg || typeof msg.cmd !== 'string') throw compat.fail('BAD_REQUEST', 'a request needs { id, cmd, args }');
    var h = self.handlers[msg.cmd];
    if (!Object.prototype.hasOwnProperty.call(self.handlers, msg.cmd) || typeof h !== 'function') throw compat.fail('UNKNOWN_CMD', 'no command ' + msg.cmd);
    var args = msg.args && typeof msg.args === 'object' ? msg.args : {};
    resolve(h(args));
  }).then(function (result) {
    return { id: id, ok: true, result: result === undefined ? null : result };
  }, function (e) {
    return { id: id, ok: false, error: errorOf(e) };
  });
};

function reply(conn, obj) { if (conn.open !== false) conn.send(JSON.stringify(obj)); }

/** Takes a Conn (send(text), close(), 'message', 'close'); it must authenticate first. */
Hub.prototype.add = function (conn) {
  var self = this;
  var authed = false;
  var timer = setTimeout(function () { if (!authed) { self.log('rpc: no auth in time, closed'); conn.close(4401); } }, this.authTimeoutMs);
  conn.on('message', function (text) {
    var msg;
    try { msg = JSON.parse(text); } catch (e) { msg = null; }
    if (!authed) {
      var ok = msg && msg.cmd === 'auth' && msg.args && auth.same(msg.args.secret, self.secret());
      if (!ok) {
        self.log('rpc: refused a connection (' + (msg && msg.cmd === 'auth' ? 'wrong secret' : 'no auth') + ')');
        reply(conn, { id: msg && msg.id !== undefined ? msg.id : null, ok: false, error: { code: 'AUTH_FAILED', message: 'not allowed' } });
        clearTimeout(timer);
        return conn.close(4401);
      }
      authed = true;
      clearTimeout(timer);
      reply(conn, { id: msg.id, ok: true, result: { version: self.version } });
      self.conns.push(conn);
      self._setPage(conn);
      return;
    }
    if (!msg) return reply(conn, { id: null, ok: false, error: { code: 'BAD_JSON', message: 'not JSON' } });
    if (typeof msg.cmd !== 'string' && typeof msg.ok === 'boolean') {
      // An answer to a page.* request; a late or repeated one (the request was re-sent) is dropped.
      if (conn === self.page && typeof msg.id === 'string' && Object.prototype.hasOwnProperty.call(self.pending, msg.id)) self._answer(msg);
      return;
    }
    self.handle(msg).then(function (r) { reply(conn, r); });
  });
  conn.on('close', function () {
    clearTimeout(timer);
    var i = self.conns.indexOf(conn);
    if (i >= 0) self.conns.splice(i, 1);
    if (self.page === conn) self._setPage(self.conns.length ? self.conns[self.conns.length - 1] : null);
  });
};

Hub.prototype._setPage = function (conn) {
  this.page = conn;
  if (!conn) return;
  // Requests not answered yet go to the new page connection: the page may have reconnected after the TV
  // put it in the background while another screen was open.
  for (var id in this.pending) reply(conn, this.pending[id].msg);
};

Hub.prototype._answer = function (msg) {
  var p = this.pending[msg.id];
  delete this.pending[msg.id];
  clearTimeout(p.timer);
  if (msg.ok) p.resolve(msg.result);
  else p.reject(compat.fail((msg.error && msg.error.code) || 'PAGE_ERROR', (msg.error && msg.error.message) || 'the page failed'));
};

/** Asks the page to do something only it can (launch an app, read webapis). */
Hub.prototype.request = function (cmd, args, timeoutMs) {
  var self = this;
  var id = 's' + (++this.seq);
  var msg = { id: id, cmd: cmd, args: args || {} };
  return new Promise(function (resolve, reject) {
    var timer = setTimeout(function () {
      delete self.pending[id];
      reject(compat.fail('PAGE_NO_ANSWER', 'ReTV Home did not answer ' + cmd));
    }, timeoutMs || self.pageTimeoutMs);
    self.pending[id] = { msg: msg, resolve: resolve, reject: reject, timer: timer };
    if (self.page) reply(self.page, msg);
  });
};

Hub.prototype.event = function (name, data) {
  var obj = { event: name, data: data === undefined ? null : data };
  for (var i = 0; i < this.conns.length; i++) reply(this.conns[i], obj);
};

module.exports = { Hub: Hub };

};
defs["./server.js"] = function (module, exports, require) {
// The local port the ReTV Home page talks to: WebSocket only, at ws://127.0.0.1:<port>/rpc, bound to
// 127.0.0.1. No HTTP API and no CORS: plain HTTP requests get 404. Web origins are refused at the upgrade
// (auth.originOk); every connection then has to show the install's secret first (rpc.js).
'use strict';
var http = require('http');
var ws = require('./ws.js');
var auth = require('./auth.js');

var MAX_MESSAGE = 64 * 1024;

/** listen(hub, { port, host = '127.0.0.1', log? }) -> Promise<http.Server> */
function listen(hub, opts) {
  var log = opts.log || function () {};
  var server = http.createServer(function (req, res) {
    res.writeHead(404, { 'Content-Type': 'text/plain' });
    res.end('not found');
  });
  ws.attach(server, '/rpc', function (conn) { hub.add(conn); }, {
    maxBytes: MAX_MESSAGE,
    verify: function (req) {
      var origin = req.headers.origin;
      var ok = auth.originOk(origin);
      log('rpc: connection from origin ' + (origin === undefined ? '(none)' : origin) + (ok ? '' : ', refused'));
      return ok;
    }
  });
  return new Promise(function (resolve, reject) {
    server.once('error', reject);
    server.listen(opts.port, opts.host || '127.0.0.1', function () { resolve(server); });
  });
}

module.exports = { listen: listen, MAX_MESSAGE: MAX_MESSAGE };

};
defs["./index.js"] = function (module, exports, require) {
// ReTV Home for Samsung: the on-TV background service (tizen:service RetvHome00.Service).
// It talks sdb over loopback (app list, removal), sends remote keys through the TV's own remote API,
// runs the privacy routine, and optionally relays sdb for the Mac. The page talks to it on
// ws://127.0.0.1:8095/rpc (see ../PROTOCOL.md). ES5 + Promise: the TV's Node may be 4.4.3.
'use strict';
var path = require('path');
var compat = require('./compat.js');
var sdb = require('./sdb.js');
var storeMod = require('./store.js');
var appsMod = require('./apps.js');
var setup = require('./setup.js');
var remoteMod = require('./remote.js');
var privacyMod = require('./privacy.js');
var relayMod = require('./relay.js');
var returnMod = require('./returnwatch.js');
var tvstateMod = require('./tvstate.js');
var rpc = require('./rpc.js');
var server = require('./server.js');
var auth = require('./auth.js');

var VERSION = '0.2.1';
var RPC_PORT = 8095;
var RELAY_PORT = 26201;
var PRIVACY_APP = 'tv6pbVMQFF.PrivacyChoices';

function packageId() {
  try { return tizen.application.getCurrentApplication().appInfo.packageId; } catch (e) {}
  return 'RetvHome00';
}

/**
 * createService(opts) wires the modules; every outside dependency can be replaced for tests:
 * { store, tizen, sdbRun, getJson, remoteOpen, remoteUrl, wait, rpcPort, sdbPort, log, tvState, launchHome,
 *   privacyTimings, returnTimers }
 */
function createService(opts) {
  opts = opts || {};
  var logs = [];
  function log(m) {
    var line = new Date().toISOString() + ' ' + m;
    logs.push(line); if (logs.length > 200) logs.shift();
    try { console.log(line); } catch (e) {}
    if (opts.log) opts.log(line);
  }
  var store = opts.store;
  if (!store) {
    var dir = storeMod.pickDir(storeMod.dataDirs(packageId()));
    store = new storeMod.Store(dir ? path.join(dir, 'retv-service.json') : null);
    log('data ' + (dir || 'in memory only'));
  }
  var sdbRun = opts.sdbRun || function (service) { return sdb.run(service, { port: opts.sdbPort }); };
  var apps = new appsMod.Apps({ sdbRun: sdbRun, store: store });
  var remote = new remoteMod.Remote({ store: store, open: opts.remoteOpen, url: opts.remoteUrl, wait: opts.wait, log: log });
  var relay = new relayMod.Relay({ sdbPort: opts.sdbPort, log: log });
  var hub;
  var getJson = opts.getJson || setup.getJson;
  var tz = opts.tizen !== undefined ? opts.tizen : (typeof tizen === 'object' ? tizen : null);
  var tv = opts.tvState || new tvstateMod.TvState({ tizen: tz, getJson: getJson, store: store, appList: function () { return apps.list(); } });

  // What is in front, for the privacy routine: ReTV, Privacy Choices, something else, or cannot tell.
  function privacyScreen() {
    return Promise.all([tv.status(returnMod.HOME), tv.running().then(null, function () { return null; })]).then(function (r) {
      if (r[0] && r[0].visible) return 'home';
      if (!r[1]) return 'unknown';
      return r[1].indexOf(PRIVACY_APP) >= 0 ? 'privacy' : 'other';
    });
  }
  var privacy = new privacyMod.Privacy({
    page: {
      openPrivacy: function () { return hub.request('page.openPrivacy', { appId: PRIVACY_APP }, 15000); },
      readAdsOff: function () { return hub.request('page.readAdsOff', {}, 60000); }
    },
    screen: privacyScreen,
    sendKeys: function (keys, gap, owner) { return remote.sendKeys(keys, gap, owner); },
    lock: function (o) { remote.lock(o); },
    unlock: function (o) { remote.unlock(o); },
    wait: opts.wait,
    timings: opts.privacyTimings,
    log: log,
    onStep: function (name, info) { log('privacy: ' + name + ' pass ' + info.pass); hub.event('privacy.step', { step: name, pass: info.pass }); }
  });

  var returnWatch = new returnMod.ReturnWatch({
    store: store, log: log, timers: opts.returnTimers,
    status: function (id) { return tv.status(id); },
    running: function () { return tv.running(); },
    userApps: function () { return apps.list().then(function (l) { return l.map(function (a) { return a.tizenId; }); }); },
    launchHome: opts.launchHome || function () {
      return new Promise(function (resolve, reject) { tz.application.launch(returnMod.HOME, resolve, reject); });
    }
  });

  function relaySettings() {
    var s = store.get('relay') || {};
    return { enabled: !!s.enabled, port: s.port || RELAY_PORT, allow: s.allow || [] };
  }
  function applyRelay() {
    var s = relaySettings();
    if (!s.enabled) { relay.stop(); return Promise.resolve(relayStatus()); }
    return relay.start(s.port, s.allow).then(relayStatus, function (e) { log('relay: ' + e.message); return relayStatus(); });
  }
  function relayStatus() {
    var s = relaySettings(), st = relay.status();
    st.enabled = s.enabled; st.configuredPort = s.port;
    return st;
  }

  var handlers = {
    ping: function () { return { version: VERSION, node: process.version }; },
    'setup.check': function () { return setup.check({ getJson: getJson, sdbRun: sdbRun }); },
    'apps.list': function () { return apps.list(); },
    'apps.uninstall': function (a) {
      return apps.uninstall(a.id).then(function (r) { log('removed ' + r.removed + ' ' + r.title); return r; });
    },
    'apps.before': function () { return apps.before(); },
    'remote.keys': function (a) { return remote.sendKeys(a.keys, a.gapMs); },
    'remote.status': function () { return remote.status(); },
    'remote.cancel': function () { return remote.cancel(); },
    'remote.forget': function () { remote.forget(); return remote.status(); },
    // The answer may go to a socket the TV closed while Privacy Choices was on screen, so the result is
    // also sent as the event privacy.done and kept for privacy.last.
    // No settings from the caller: timings and passes are fixed in privacy.js.
    'privacy.run': function () {
      return privacy.run().then(function (r) {
        if (r.code !== 'BUSY') { r.at = new Date().toISOString(); store.set('privacyLast', r); }
        log('privacy: ' + r.code + ' after ' + r.passes + ' passes' + (r.error ? ' (' + r.error + ')' : ''));
        hub.event('privacy.done', r);
        return r;
      });
    },
    'privacy.last': function () { return store.get('privacyLast') || null; },
    'relay.status': function () { return relayStatus(); },
    'relay.set': function (a) {
      var s = relaySettings();
      if (a.allow !== undefined) s.allow = relayMod.normAllow(a.allow);
      if (a.port !== undefined) {
        var p = Number(a.port);
        if (!(p >= 1024 && p <= 65535) || p === RPC_PORT) throw compat.fail('BAD_PORT', 'port: 1024 to 65535');
        s.port = p;
      }
      if (a.enabled !== undefined) s.enabled = !!a.enabled;
      if (s.enabled && !s.allow.length) throw compat.fail('BAD_ALLOW', 'the relay needs at least one allowed IP');
      store.set('relay', s);
      return applyRelay();
    },
    // Return to ReTV: the page calls watchReturn right after it launched an app.
    watchReturn: function (a) { return returnWatch.watch(a.appId); },
    setReturnToReTV: function (a) { return returnWatch.setOn(a.on !== false); },
    returnStatus: function () { return returnWatch.status(); },
    setReturnLists: function (a) { return returnWatch.setLists(a); },
    // What runs now and what the REST API says is in front: to learn the system screens' ids on a TV.
    returnDebug: function () {
      return tv.running().then(function (ids) {
        return Promise.all(ids.slice(0, 40).map(function (id) {
          return tv.status(id).then(function (st) { return { appId: id, visible: st ? st.visible : null }; });
        }));
      }, function (e) { return { error: e.message }; }).then(function (running) {
        return { running: running, lists: returnWatch.lists(), status: returnWatch.status(), ticks: returnWatch.history() };
      });
    },
    'log.tail': function () { return logs.slice(-100); }
  };
  var getSecret = function () { return auth.secret(store); };
  hub = new rpc.Hub({ handlers: handlers, secret: getSecret, version: VERSION, log: log });

  var http = null;
  return {
    hub: hub, store: store, relay: relay, returnWatch: returnWatch,
    start: function () {
      if (http) return Promise.resolve(http);
      var port = opts.rpcPort === undefined ? RPC_PORT : opts.rpcPort;
      getSecret();
      auth.serve({ tizen: tz, getSecret: getSecret, log: log });
      http = server.listen(hub, { port: port, log: log });
      return http.then(function (s) {
        log('ReTV service ' + VERSION + ' on 127.0.0.1:' + s.address().port + ', node ' + process.version);
        applyRelay();
        return s;
      }, function (e) { http = null; log('cannot listen: ' + e.message); throw e; });
    },
    stop: function () {
      relay.stop();
      returnWatch._stop('STOPPED');
      if (http) http.then(function (s) { s.close(); });
      http = null;
    }
  };
}

var service = null;
function start() {
  if (!service) service = createService();
  service.start().then(null, function () {});
}

module.exports = {
  createService: createService,
  VERSION: VERSION, RPC_PORT: RPC_PORT,
  // Tizen calls these on the service file's exports.
  onStart: start,
  onRequest: start,
  onExit: function () { if (service) service.stop(); }
};

};
var api = load("./index.js");
if (hostModule && hostModule.exports) for (var k in api) hostModule.exports[k] = api[k];
})(typeof require === "function" ? require : null, typeof module === "object" ? module : null);

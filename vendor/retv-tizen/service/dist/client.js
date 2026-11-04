// Page side of the ReTV Home service protocol (PROTOCOL.md). Load it in index.html as
// <script src="service/dist/client.js"></script>, then:
//   var svc = RetvService.create();
//   svc.call('apps.list').then(function (apps) { ... });
//   svc.on('privacy.step', function (d) { ... });
// It gets the install's secret from the service over a trusted message port, starts the service when
// nothing answers, authenticates every connection, reconnects after the TV paused the page, and answers
// the service's page.* requests (open Privacy Choices, read the ad-tracking state). ES5, no dependencies.
(function (root) {
  'use strict';

  function defaultPageHandlers() {
    return {
      'page.openPrivacy': function (args) {
        return new Promise(function (resolve, reject) {
          tizen.application.launch(args.appId, function () { resolve(true); },
            function (e) { reject({ code: 'OPEN_FAILED', message: e && e.message }); });
        });
      },
      'page.readAdsOff': function () { return webapis.adinfo.isLATEnabled() === true; }
    };
  }

  function serviceIdDefault() {
    return tizen.application.getCurrentApplication().appInfo.packageId + '.Service';
  }

  function launchServiceDefault() {
    return new Promise(function (resolve, reject) {
      try {
        tizen.application.launchAppControl(new tizen.ApplicationControl('http://tizen.org/appcontrol/operation/service'),
          serviceIdDefault(), function () { resolve(); }, function (e) { reject(e); });
      } catch (e) { reject(e); }
    });
  }

  // Asks the service for the secret over trusted message ports (only apps signed with ReTV's certificate can
  // use them, and the service answers only ReTV Home's app id). Rejects when the service is not running.
  var replyPort = null;
  function secretByMessagePort() {
    return new Promise(function (resolve, reject) {
      var mp = tizen.messageport, wid = null, timer = null;
      try {
        if (!replyPort) replyPort = mp.requestTrustedLocalMessagePort('retv.auth.reply');
        wid = replyPort.addMessagePortListener(function (items) {
          for (var i = 0; i < items.length; i++) {
            if (items[i].key === 'secret') { clearTimeout(timer); replyPort.removeMessagePortListener(wid); return resolve(String(items[i].value)); }
          }
        });
        var remote = mp.requestTrustedRemoteMessagePort(serviceIdDefault(), 'retv.auth');
        remote.sendMessage([{ key: 'cmd', value: 'secret' }], replyPort);
        timer = setTimeout(function () { replyPort.removeMessagePortListener(wid); reject(new Error('no secret')); }, 3000);
      } catch (e) {
        if (wid !== null) try { replyPort.removeMessagePortListener(wid); } catch (x) {}
        reject(e);
      }
    });
  }

  function err(code, message) { var e = new Error(message); e.code = code; return e; }

  /**
   * create({ url = 'ws://127.0.0.1:8095/rpc', WebSocket?, getSecret?(), launchService?(), handlers?,
   *          timeoutMs = 120000, retryMs = 1500, maxTries = 8 })
   */
  function create(opts) {
    opts = opts || {};
    var url = opts.url || 'ws://127.0.0.1:8095/rpc';
    var WS = opts.WebSocket || root.WebSocket;
    var getSecret = opts.getSecret || secretByMessagePort;
    var launch = opts.launchService || launchServiceDefault;
    var handlers = opts.handlers || defaultPageHandlers();
    var timeoutMs = opts.timeoutMs || 120000;
    var retryMs = opts.retryMs || 1500;
    var maxTries = opts.maxTries || 8;
    var sock = null, ready = null, seq = 0, launched = false, secret = null;
    var waiting = {}, listeners = {};

    function settle(id, msg) {
      var w = waiting[id];
      if (!w) return;
      delete waiting[id];
      clearTimeout(w.timer);
      if (msg.ok) w.resolve(msg.result);
      else w.reject(err(msg.error && msg.error.code, msg.error && msg.error.message));
    }

    function onMessage(ev) {
      var msg;
      try { msg = JSON.parse(ev.data); } catch (e) { return; }
      if (msg.event) {
        var ls = listeners[msg.event] || [];
        for (var i = 0; i < ls.length; i++) try { ls[i](msg.data); } catch (e) {}
        return;
      }
      if (typeof msg.cmd === 'string') return answer(msg);
      settle(msg.id, msg);
    }

    function answer(msg) {
      var h = handlers[msg.cmd];
      new Promise(function (resolve) {
        if (!h) throw { code: 'UNKNOWN_CMD', message: 'the page has no ' + msg.cmd };
        resolve(h(msg.args || {}));
      }).then(function (result) {
        send({ id: msg.id, ok: true, result: result === undefined ? null : result });
      }, function (e) {
        send({ id: msg.id, ok: false, error: { code: (e && e.code) || 'PAGE_ERROR', message: (e && e.message) || String(e) } });
      });
    }

    function send(obj) { if (sock && sock.readyState === 1) sock.send(JSON.stringify(obj)); }

    // A dropped socket fails the calls waiting on it at once; the next call reconnects.
    function dropped(s) {
      if (sock !== s) return;
      if (!s.authed) { sock = null; return; }     // still inside connect(): its retry loop goes on
      sock = null; ready = null; launched = false;
      for (var id in waiting) settle(id, { ok: false, error: { code: 'DISCONNECTED', message: 'the service connection closed' } });
    }

    // Opens a socket and authenticates it with the secret.
    function open(sec) {
      return new Promise(function (resolve, reject) {
        var s = new WS(url);
        var done = false;
        s.onopen = function () {
          sock = s;
          var id = 'p' + (++seq);
          waiting[id] = { resolve: function () { done = true; s.authed = true; resolve(s); },
            reject: function (e) { done = true; reject(e); }, timer: setTimeout(function () { settle(id, { ok: false, error: { code: 'TIMEOUT', message: 'no auth answer' } }); }, 5000) };
          s.send(JSON.stringify({ id: id, cmd: 'auth', args: { secret: sec } }));
        };
        s.onerror = function () { if (!done) { done = true; reject(err('NO_SERVICE', 'no service')); } };
        s.onclose = function () {
          if (!done) { done = true; reject(err('NO_SERVICE', 'no service')); }
          dropped(s);
        };
        s.onmessage = onMessage;
      });
    }

    function connect() {
      if (ready) return ready;
      var tries = 0;
      ready = new Promise(function (resolve, reject) {
        function attempt() {
          (secret ? Promise.resolve(secret) : getSecret()).then(function (sec) {
            secret = sec;
            return open(sec);
          }).then(function (s) {
            resolve(s);
            // Every (re)connection: the page can push its settings so the service never disagrees with it.
            var ls = listeners.connect || [];
            for (var i = 0; i < ls.length; i++) try { ls[i](); } catch (x) {}
          }, function (e) {
            if (e && e.code === 'AUTH_FAILED') secret = null;     // a reinstall made a new secret
            tries++;
            if (tries >= maxTries) { ready = null; return reject(err('NO_SERVICE', 'the ReTV service does not start')); }
            var go = function () { setTimeout(attempt, retryMs); };
            if (!launched) { launched = true; launch().then(go, go); } else go();
          });
        }
        attempt();
      });
      return ready;
    }

    function call(cmd, args, ms) {
      return connect().then(function () {
        return new Promise(function (resolve, reject) {
          var id = 'p' + (++seq);
          var timer = setTimeout(function () {
            delete waiting[id];
            reject(err('TIMEOUT', 'no answer to ' + cmd));
          }, ms || timeoutMs);
          waiting[id] = { resolve: resolve, reject: reject, timer: timer };
          send({ id: id, cmd: cmd, args: args || {} });
        });
      });
    }

    function on(name, fn) { (listeners[name] = listeners[name] || []).push(fn); }

    // After the TV shows another app (Privacy Choices) the page may come back with a dead socket.
    if (root.document && root.document.addEventListener) {
      root.document.addEventListener('visibilitychange', function () {
        if (!root.document.hidden) connect().then(null, function () {});
      });
    }

    return { call: call, on: on, connect: connect };
  }

  var api = { create: create };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.RetvService = api;
})(typeof window !== 'undefined' ? window : this);

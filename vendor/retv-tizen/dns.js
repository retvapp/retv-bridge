// ReTV Home for Samsung: «DNS без рекламы» (Pro). The TV links its home network to the account and the person types
// the resolver's address in Samsung's network settings; when blocking is off (signed out or revoked, Pro ended, DNS
// off in the cabinet) but the TV still uses our resolver, only ReTV works, so the person is asked, here and on the
// home screen, to set DNS back to automatic.
//
// Linking, as ReTV Home on Android (DnsReportPolicy): every 15 min while Pro and DNS are on, never twice in 5 min.
// The on-TV service does it (service/src/dnsreport.js: also while ReTV Home is not on screen); the page hands it the
// account and asks it for each report, so the two never report twice, and only reports itself when the service does
// not answer (an old service, a TV without it). Samsung has one DNS field: an unlinked network gets only retv.app.
// The last answer is kept (retv.dnsLast: { status: linked|notLinked|later, ip, servers }), and the resolver's
// addresses also apart (retv.dnsServers), since they are needed most after signing out. ES2017 at most.
(function (root) {
  "use strict";

  /**
   * RetvDns(h): h = { L, O (opt strings), K, read(k, def), write(k, v), el(tag, cls, text), acct(), plan(),
   *   api(cell, path, body, token, cb), forget(), svcCall(cmd, args, ms), rerender(view), steps(list),
   *   optItem(title, sub, act), optBack(), siteOf(), openSettings() or null (Samsung's Settings cannot open here),
   *   changed() (the revert state may have changed) }
   */
  root.RetvDns = function (h) {
    var L = h.L, O = h.O, K = h.K, el = h.el;
    var triedAt = 0, last = h.read(K.dnsLast, null), view = { step: "idle" }, sentConfig = null;

    function ready() { var a = h.acct(); return !!a.token && L.dnsView(h.plan(), a.dns) === "ready"; }
    function servers() {
      var s = L.dnsServersOf(h.read(K.dnsServers, null));
      return s.length ? s : L.dnsServersOf(last && last.servers);
    }
    /** An answer ({ status, ip, servers }) from the server or the service: kept, shown. */
    function took(st, ip, list) {
      var s = L.dnsServersOf(list);
      if (s.length) h.write(K.dnsServers, s);
      last = { status: st, ip: ip || null, servers: s.length ? s : servers() };
      h.write(K.dnsLast, last);
      view = { step: "done" };
      h.rerender("dns");
      h.changed();
    }

    /** The account for the service's own reports; sent when it changed (and once per page life). */
    function configure() {
      var a = h.acct(), args = a.token ? { cell: a.cell, token: a.token, on: ready() } : { token: null };
      var sig = JSON.stringify(args);
      if (sig === sentConfig) return Promise.resolve(true);
      return h.svcCall("dns.config", args, 15000).then(function (r) {
        if (!r || typeof r.configured !== "boolean") return false;   // a service without dns.*
        sentConfig = sig;
        return true;
      });
    }
    /** A report now if one is due ([force]: the page is open; still not within 5 min of the last try). */
    function report(force) {
      var a = h.acct();
      if (!a.token) { if (sentConfig) configure().then(null, quiet); return; }
      if (!ready()) { configure().then(null, quiet); return; }
      var token = a.token;
      if (force) { view = { step: "linking" }; h.rerender("dns"); }
      configure().then(function (ok) {
        if (!ok) throw new Error("no dns.report");
        return h.svcCall("dns.report", { force: !!force }, 30000);
      }).then(function (r) {
        if (h.acct().token !== token) return;
        if (!r || typeof r.configured !== "boolean") throw new Error("no dns.report");
        if (r.revoked) { h.forget(); return; }
        if (typeof r.lastOk === "number") h.write(K.dnsAt, r.lastOk);
        var l = r.last;
        if (l && typeof l.status === "string") took(l.status, l.ip, l.servers);
        else if (view.step === "linking") { view = { step: "done" }; h.rerender("dns"); }
      }).then(null, function () { if (h.acct().token === token) reportSelf(force); });
    }
    /** The page's own report, when the service does not answer. */
    function reportSelf(force) {
      var a = h.acct();
      if (!ready()) return;
      var now = Date.now(), token = a.token;
      if (!L.dnsReportDue(true, now, h.read(K.dnsAt, null), triedAt, force)) {
        if (view.step === "linking") { view = { step: "done" }; h.rerender("dns"); }
        return;
      }
      triedAt = now;
      view = { step: "linking" };
      h.rerender("dns");
      h.api(a.cell, "/tv/dns", null, token, function (st, d) {
        if (h.acct().token !== token) return;
        if (st === 401 && d && d.error === "revoked") { h.forget(); return; }
        var s = L.dnsLinkState(st, d);
        if (s === "linked") h.write(K.dnsAt, now);
        took(s, d && typeof d.ip === "string" ? d.ip : null, d && d.servers);
      });
    }
    function quiet() {}
    /** Signed out: the last answer goes, the resolver's addresses stay (they tell whether DNS must go back). */
    function forgotten() { last = null; h.write(K.dnsLast, null); h.write(K.dnsAt, null); configure().then(null, quiet); }

    /** The TV's DNS server now (Samsung's network API), or null. */
    function tvDns() { try { return root.webapis.network.getDns(); } catch (e) { return null; } }
    function revertNow() { return L.dnsRevert(L.dnsView(h.plan(), h.acct().dns), tvDns(), servers()); }

    function page(f) {
      var v = L.dnsView(h.plan(), h.acct().dns), site = h.siteOf(), cabinet = site + "/account/dns";
      if (revertNow()) {
        f.appendChild(el("p", "lead", O.dnsRevertLead));
        f.appendChild(h.steps(O.dnsRevertSteps));
        if (h.openSettings) f.appendChild(h.optItem(O.openSamsung, "", h.openSettings));
      }
      if (v === "pro") {
        f.appendChild(el("p", "lead", O.dnsPro));
        f.appendChild(el("p", "", L.fmt(O.dnsProHow, site)));
      } else if (v === "proEnded") f.appendChild(el("p", "lead", L.fmt(O.dnsProEnded, site)));
      else if (v === "turnOn") f.appendChild(el("p", "lead", L.fmt(O.dnsTurnOn, cabinet)));
      else {
        var server = L.dnsServer(last && last.servers);
        f.appendChild(el("p", "lead", O.dnsLead));
        if (server) f.appendChild(el("div", "code", server));
        f.appendChild(h.steps([O.dnsSteps[0], O.dnsSteps[1], L.fmt(O.dnsSteps[2], server || L.fmt(O.dnsSite, cabinet))]));
        f.appendChild(el("p", "", O.dnsOneField));
        var msg = view.step === "linking" || !last ? O.dnsLinking
          : last.status === "linked" ? L.fmt(O.dnsLinked, last.ip) : last.status === "later" ? O.dnsLater : O.dnsNotLinked;
        f.appendChild(el("p", "msg", msg));
      }
      f.appendChild(h.optItem(O.back, "", h.optBack));
    }
    /** The DNS card's line in «Оптимизация». */
    function cardLine() {
      if (revertNow()) return O.dnsRevertCard;
      var v = L.dnsView(h.plan(), h.acct().dns);
      return v === "pro" || v === "proEnded" ? O.dnsPro : O.dnsCardOn;
    }
    function opened() { view = { step: "idle" }; }

    return { report: report, configure: configure, forgotten: forgotten, revertNow: revertNow, page: page, cardLine: cardLine, opened: opened };
  };
})(typeof window !== "undefined" ? window : this);

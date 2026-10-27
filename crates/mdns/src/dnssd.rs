//! macOS: mDNSResponder through dns_sd (libSystem). Browse a service type,
//! resolve each instance to host and port, then the host to IPv4 addresses.
//! Everything runs on the calling (blocking) thread: callbacks fire only
//! inside `DNSServiceProcessResult`, which is called here, so the context
//! they write to is never shared with another thread. The bridge's process
//! opens no mDNS socket; mDNSResponder does the network part.

use std::ffi::{c_char, c_void, CStr, CString};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use crate::os_resolver::{ipv4_from_sockaddr_bytes, OsAnswer};

type DNSServiceRef = *mut c_void;
type DNSServiceFlags = u32;
type DNSServiceErrorType = i32;

const K_FLAGS_ADD: DNSServiceFlags = 0x2;
const K_PROTOCOL_IPV4: u32 = 0x01;
const K_NO_ERROR: DNSServiceErrorType = 0;
/// Instances resolved per browse at most (each one holds two sockets).
const MAX_INSTANCES: usize = 32;

type BrowseReply = extern "C" fn(
    DNSServiceRef,
    DNSServiceFlags,
    u32,
    DNSServiceErrorType,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = extern "C" fn(
    DNSServiceRef,
    DNSServiceFlags,
    u32,
    DNSServiceErrorType,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
type AddrInfoReply = extern "C" fn(
    DNSServiceRef,
    DNSServiceFlags,
    u32,
    DNSServiceErrorType,
    *const c_char,
    *const c_void,
    u32,
    *mut c_void,
);

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
const POLLIN: i16 = 0x1;

extern "C" {
    fn DNSServiceBrowse(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        regtype: *const c_char,
        domain: *const c_char,
        callback: BrowseReply,
        context: *mut c_void,
    ) -> DNSServiceErrorType;
    fn DNSServiceResolve(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> DNSServiceErrorType;
    fn DNSServiceGetAddrInfo(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        protocol: u32,
        hostname: *const c_char,
        callback: AddrInfoReply,
        context: *mut c_void,
    ) -> DNSServiceErrorType;
    fn DNSServiceRefSockFD(sd_ref: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(sd_ref: DNSServiceRef) -> DNSServiceErrorType;
    fn DNSServiceRefDeallocate(sd_ref: DNSServiceRef);
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

/// What the callbacks hand back to the loop.
enum Event {
    Found {
        iface: u32,
        name: String,
        regtype: String,
        domain: String,
    },
    Resolved {
        instance: usize,
        iface: u32,
        host: String,
        port: u16,
    },
    Addr {
        instance: usize,
        ip: Ipv4Addr,
    },
}

/// A context for one request: the shared event list, and which instance a
/// resolve or address lookup belongs to.
struct Ctx {
    events: *mut Vec<Event>,
    instance: usize,
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: dns_sd passes NUL-terminated strings valid for the callback.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn push(ctx: *mut c_void, ev: impl FnOnce(&Ctx) -> Event) {
    // SAFETY: `ctx` is a `Ctx` boxed in `browse`, alive until every request
    // is deallocated, and only touched on this thread.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    let e = ev(ctx);
    unsafe { (*ctx.events).push(e) };
}

extern "C" fn on_browse(
    _r: DNSServiceRef,
    flags: DNSServiceFlags,
    iface: u32,
    err: DNSServiceErrorType,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    ctx: *mut c_void,
) {
    if err != K_NO_ERROR || flags & K_FLAGS_ADD == 0 {
        return;
    }
    push(ctx, |_| Event::Found {
        iface,
        name: text(name),
        regtype: text(regtype),
        domain: text(domain),
    });
}

extern "C" fn on_resolve(
    _r: DNSServiceRef,
    _flags: DNSServiceFlags,
    iface: u32,
    err: DNSServiceErrorType,
    _fullname: *const c_char,
    host: *const c_char,
    port_be: u16,
    _txt_len: u16,
    _txt: *const u8,
    ctx: *mut c_void,
) {
    if err != K_NO_ERROR {
        return;
    }
    push(ctx, |c| Event::Resolved {
        instance: c.instance,
        iface,
        host: text(host),
        port: u16::from_be(port_be),
    });
}

extern "C" fn on_addr(
    _r: DNSServiceRef,
    flags: DNSServiceFlags,
    _iface: u32,
    err: DNSServiceErrorType,
    _host: *const c_char,
    addr: *const c_void,
    _ttl: u32,
    ctx: *mut c_void,
) {
    if err != K_NO_ERROR || flags & K_FLAGS_ADD == 0 || addr.is_null() {
        return;
    }
    // SAFETY: a sockaddr; its first byte is its length (BSD), at most 128.
    let sa = unsafe {
        let len = (*(addr as *const u8)) as usize;
        std::slice::from_raw_parts(addr as *const u8, len.clamp(2, 128))
    };
    if let Some(ip) = ipv4_from_sockaddr_bytes(sa) {
        push(ctx, |c| Event::Addr {
            instance: c.instance,
            ip,
        });
    }
}

/// The open requests and what their callbacks write to. On drop the
/// requests are deallocated first, then their contexts and the events.
struct Requests {
    refs: Vec<DNSServiceRef>,
    // Boxed: dns_sd keeps each context's address, so it must not move.
    #[allow(clippy::vec_box)]
    ctxs: Vec<Box<Ctx>>,
    /// Only ever reached through this pointer (here and in callbacks).
    events: *mut Vec<Event>,
}

impl Drop for Requests {
    fn drop(&mut self) {
        for r in self.refs.drain(..) {
            // SAFETY: each ref came from a successful dns_sd call, once.
            unsafe { DNSServiceRefDeallocate(r) };
        }
        self.ctxs.clear();
        // SAFETY: from `Box::into_raw` in `new`; no callback can run now.
        drop(unsafe { Box::from_raw(self.events) });
    }
}

impl Requests {
    fn new() -> Requests {
        Requests {
            refs: Vec::new(),
            ctxs: Vec::new(),
            events: Box::into_raw(Box::default()),
        }
    }

    /// The events the callbacks have written so far.
    fn take_events(&mut self) -> Vec<Event> {
        // SAFETY: no callback runs outside `DNSServiceProcessResult`.
        std::mem::take(unsafe { &mut *self.events })
    }

    fn ctx(&mut self, instance: usize) -> *mut c_void {
        let events = self.events;
        let mut b = Box::new(Ctx { events, instance });
        let p = &mut *b as *mut Ctx as *mut c_void;
        self.ctxs.push(b);
        p
    }

    /// Deallocates one request and stops polling it.
    fn close(&mut self, r: DNSServiceRef) {
        if let Some(i) = self.refs.iter().position(|x| *x == r) {
            self.refs.swap_remove(i);
            // SAFETY: a ref from a successful dns_sd call, now out of `refs`.
            unsafe { DNSServiceRefDeallocate(r) };
        }
    }

    fn add(&mut self, err: DNSServiceErrorType, r: DNSServiceRef) -> io::Result<()> {
        if err != K_NO_ERROR || r.is_null() {
            return Err(io::Error::other(format!("dns_sd error {err}")));
        }
        self.refs.push(r);
        Ok(())
    }
}

struct Instance {
    name: String,
    regtype: String,
    port: Option<u16>,
    addrs: Vec<IpAddr>,
    resolved: bool,
}

/// Browses `regtype` (e.g. `_adb-tls-pairing._tcp`) in `local.` for
/// `timeout`, resolving everything found.
///
/// It always runs the whole `timeout`, never stopping at the first answer:
/// there is no "last answer" in mDNS, a TV can announce at any moment (the
/// S3 does about 2 s after its pairing dialog opens), and the querier runs
/// for the same window anyway, so stopping early would not make
/// `discover` return sooner.
pub fn browse(regtype: &str, timeout: Duration) -> io::Result<Vec<OsAnswer>> {
    let deadline = Instant::now() + timeout;
    let mut reqs = Requests::new();
    let regtype_c = CString::new(regtype).map_err(io::Error::other)?;
    let domain_c = CString::new("local.").expect("static");
    let mut r: DNSServiceRef = std::ptr::null_mut();
    let ctx = reqs.ctx(usize::MAX);
    // SAFETY: valid out-pointer, NUL-terminated strings, `ctx` outlives the ref.
    let err = unsafe {
        DNSServiceBrowse(
            &mut r,
            0,
            0,
            regtype_c.as_ptr(),
            domain_c.as_ptr(),
            on_browse,
            ctx,
        )
    };
    reqs.add(err, r)?;

    let mut instances: Vec<Instance> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds: Vec<PollFd> = reqs
            .refs
            .iter()
            .map(|r| PollFd {
                // SAFETY: a live ref.
                fd: unsafe { DNSServiceRefSockFD(*r) },
                events: POLLIN,
                revents: 0,
            })
            .collect();
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: `fds` is a valid array of `fds.len()` entries.
        let n = unsafe { poll(fds.as_mut_ptr(), fds.len() as u32, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        let ready: Vec<DNSServiceRef> = fds
            .iter()
            .zip(&reqs.refs)
            .filter(|(f, _)| f.revents != 0)
            .map(|(_, r)| *r)
            .collect();
        for r in ready {
            // SAFETY: a live ref with data waiting; callbacks run here.
            if unsafe { DNSServiceProcessResult(r) } != K_NO_ERROR {
                // Its socket is broken (e.g. mDNSResponder restarted): it
                // would poll readable forever, so it goes.
                reqs.close(r);
            }
        }
        if reqs.refs.is_empty() {
            break;
        }
        for ev in reqs.take_events() {
            match ev {
                Event::Found {
                    iface,
                    name,
                    regtype,
                    domain,
                } => {
                    if instances.len() >= MAX_INSTANCES
                        || instances
                            .iter()
                            .any(|i| i.name == name && i.regtype == regtype)
                    {
                        continue;
                    }
                    let idx = instances.len();
                    instances.push(Instance {
                        name: name.clone(),
                        regtype: regtype.clone(),
                        port: None,
                        addrs: Vec::new(),
                        resolved: false,
                    });
                    let (Ok(n), Ok(t), Ok(d)) = (
                        CString::new(name),
                        CString::new(regtype),
                        CString::new(domain),
                    ) else {
                        continue;
                    };
                    let ctx = reqs.ctx(idx);
                    let mut r: DNSServiceRef = std::ptr::null_mut();
                    // SAFETY: as for the browse above.
                    let err = unsafe {
                        DNSServiceResolve(
                            &mut r,
                            0,
                            iface,
                            n.as_ptr(),
                            t.as_ptr(),
                            d.as_ptr(),
                            on_resolve,
                            ctx,
                        )
                    };
                    let _ = reqs.add(err, r);
                }
                Event::Resolved {
                    instance,
                    iface,
                    host,
                    port,
                } => {
                    let Some(inst) = instances.get_mut(instance) else {
                        continue;
                    };
                    inst.port = Some(port);
                    if inst.resolved {
                        continue;
                    }
                    inst.resolved = true;
                    let Ok(h) = CString::new(host) else { continue };
                    let ctx = reqs.ctx(instance);
                    let mut r: DNSServiceRef = std::ptr::null_mut();
                    // SAFETY: as for the browse above.
                    let err = unsafe {
                        DNSServiceGetAddrInfo(
                            &mut r,
                            0,
                            iface,
                            K_PROTOCOL_IPV4,
                            h.as_ptr(),
                            on_addr,
                            ctx,
                        )
                    };
                    let _ = reqs.add(err, r);
                }
                Event::Addr { instance, ip } => {
                    if let Some(inst) = instances.get_mut(instance) {
                        if !inst.addrs.contains(&IpAddr::V4(ip)) {
                            inst.addrs.push(IpAddr::V4(ip));
                        }
                    }
                }
            }
        }
    }
    drop(reqs);
    Ok(instances
        .into_iter()
        .filter_map(|i| {
            Some(OsAnswer {
                instance: i.name,
                service_type: i.regtype,
                port: i.port?,
                addrs: i.addrs,
            })
        })
        .collect())
}

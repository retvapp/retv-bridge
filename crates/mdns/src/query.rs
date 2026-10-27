//! The query packet and the sockets that carry it. Sockets are bound to an
//! ephemeral port: 5353 is never bound, so no Windows Firewall prompt and no
//! fight with the OS responder. The QU bit asks TVs to answer unicast to us.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use async_trait::async_trait;
use simple_dns::{Name, Packet, Question, CLASS, QCLASS, QTYPE, TYPE};
use socket2::{Domain, Protocol, Socket, Type};

use crate::parse::{SERVICE_ADB, SERVICE_CONNECT, SERVICE_PAIRING};

pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const MDNS_PORT: u16 = 5353;

/// PTR questions for the three adb services, class IN with the QU bit.
pub fn build_query() -> Vec<u8> {
    let mut pkt = Packet::new_query(0);
    for svc in [SERVICE_CONNECT, SERVICE_PAIRING, SERVICE_ADB] {
        pkt.questions.push(Question::new(
            Name::new_unchecked(svc),
            QTYPE::TYPE(TYPE::PTR),
            QCLASS::CLASS(CLASS::IN),
            true,
        ));
    }
    pkt.build_bytes_vec().expect("static query encodes")
}

/// One socket on one interface.
#[async_trait]
pub trait QuerySocket: Send + Sync {
    /// Send to the mDNS group on this socket's interface.
    async fn send_query(&self, packet: &[u8]) -> io::Result<()>;
    /// Next datagram and its IPv4 source (non-IPv4 sources are skipped).
    async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, Ipv4Addr)>;
}

/// Opens one socket per interface address: bound to `<addr>:0`, with
/// `IP_MULTICAST_IF` set to that address.
pub trait SocketFactory: Send + Sync {
    fn open(&self, iface: Ipv4Addr) -> io::Result<Box<dyn QuerySocket>>;
}

pub struct UdpQuerySocket {
    sock: tokio::net::UdpSocket,
}

impl UdpQuerySocket {
    pub fn bind(iface: Ipv4Addr) -> io::Result<Self> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        s.bind(&SocketAddr::V4(SocketAddrV4::new(iface, 0)).into())?;
        s.set_multicast_if_v4(&iface)?;
        s.set_multicast_ttl_v4(255)?;
        s.set_nonblocking(true)?;
        let sock = tokio::net::UdpSocket::from_std(s.into())?;
        Ok(Self { sock })
    }

    /// The interface address `IP_MULTICAST_IF` is set to.
    pub fn multicast_if(&self) -> io::Result<Ipv4Addr> {
        socket2::SockRef::from(&self.sock).multicast_if_v4()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }
}

#[async_trait]
impl QuerySocket for UdpQuerySocket {
    async fn send_query(&self, packet: &[u8]) -> io::Result<()> {
        self.sock
            .send_to(packet, SocketAddrV4::new(MDNS_GROUP, MDNS_PORT))
            .await
            .map(|_| ())
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, Ipv4Addr)> {
        loop {
            let (n, from) = self.sock.recv_from(buf).await?;
            if let SocketAddr::V4(v4) = from {
                return Ok((n, *v4.ip()));
            }
        }
    }
}

pub struct SystemSockets;

impl SocketFactory for SystemSockets {
    fn open(&self, iface: Ipv4Addr) -> io::Result<Box<dyn QuerySocket>> {
        Ok(Box::new(UdpQuerySocket::bind(iface)?))
    }
}

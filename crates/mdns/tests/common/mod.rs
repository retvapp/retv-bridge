//! Builds mDNS answers the way an Android TV sends them.
#![allow(dead_code)]

use std::net::Ipv4Addr;

use simple_dns::rdata::{RData, A, PTR, SRV};
use simple_dns::{Name, Packet, ResourceRecord, CLASS};

pub struct Announce<'a> {
    pub service: &'a str,
    pub instance: &'a str,
    pub port: u16,
    pub host: &'a str,
    /// `None` leaves the A record out.
    pub ip: Option<Ipv4Addr>,
}

pub fn response(items: &[Announce]) -> Vec<u8> {
    let mut pkt = Packet::new_reply(0);
    let mut owned: Vec<(String, String)> = Vec::new();
    for it in items {
        owned.push((
            it.service.to_string(),
            format!("{}.{}", it.instance, it.service),
        ));
    }
    for (it, (svc, inst)) in items.iter().zip(&owned) {
        pkt.answers.push(ResourceRecord::new(
            Name::new_unchecked(svc),
            CLASS::IN,
            4500,
            RData::PTR(PTR(Name::new_unchecked(inst))),
        ));
        pkt.additional_records.push(ResourceRecord::new(
            Name::new_unchecked(inst),
            CLASS::IN,
            120,
            RData::SRV(SRV {
                priority: 0,
                weight: 0,
                port: it.port,
                target: Name::new_unchecked(it.host),
            }),
        ));
        if let Some(ip) = it.ip {
            pkt.additional_records.push(ResourceRecord::new(
                Name::new_unchecked(it.host),
                CLASS::IN,
                120,
                RData::A(A { address: ip.into() }),
            ));
        }
    }
    pkt.build_bytes_vec().unwrap()
}

pub fn response_compressed(items: &[Announce]) -> Vec<u8> {
    let plain = response(items);
    let pkt = Packet::parse(&plain).unwrap();
    pkt.build_bytes_vec_compressed().unwrap()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

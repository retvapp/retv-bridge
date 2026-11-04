//! The little DER the certificate requests need: write TLVs, read them back.

pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = content.len();
    if n < 0x80 {
        v.push(n as u8);
    } else {
        let bytes: Vec<u8> = n
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        v.push(0x80 | bytes.len() as u8);
        v.extend_from_slice(&bytes);
    }
    v.extend_from_slice(content);
    v
}

pub fn seq(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(SEQUENCE, &items.concat())
}

pub fn set(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(SET, &items.concat())
}

pub fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &a in &arcs[2..] {
        let mut chunk = vec![(a & 0x7f) as u8];
        let mut rest = a >> 7;
        while rest > 0 {
            chunk.push(0x80 | (rest & 0x7f) as u8);
            rest >>= 7;
        }
        chunk.reverse();
        body.extend_from_slice(&chunk);
    }
    tlv(0x06, &body)
}

pub fn integer_zero() -> Vec<u8> {
    tlv(0x02, &[0])
}

pub fn null() -> Vec<u8> {
    vec![0x05, 0x00]
}

pub fn utf8(s: &str) -> Vec<u8> {
    tlv(0x0C, s.as_bytes())
}

pub fn ia5(s: &str) -> Vec<u8> {
    tlv(0x16, s.as_bytes())
}

pub fn octet_string(b: &[u8]) -> Vec<u8> {
    tlv(0x04, b)
}

/// A BIT STRING of whole bytes.
pub fn bit_string(b: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8];
    v.extend_from_slice(b);
    tlv(0x03, &v)
}

/// `[n]` context-specific, constructed.
pub fn context(n: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xA0 | n, content)
}

/// `[n]` context-specific, primitive (an IMPLICIT string such as a URI).
pub fn context_primitive(n: u8, content: &[u8]) -> Vec<u8> {
    tlv(0x80 | n, content)
}

/// (tag, content, rest) of the first TLV in `b`.
pub fn read_tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)? as usize;
    let (len, head) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let bytes = b.get(2..2 + n)?;
        (
            bytes.iter().fold(0usize, |a, x| (a << 8) | *x as usize),
            2 + n,
        )
    };
    let content = b.get(head..head + len)?;
    Some((tag, content, &b[head + len..]))
}

use base64::Engine;
use retv_bridge_samsung::csr::{author_csr, distributor_csr, key_from_pem, key_pem, new_key};
use retv_bridge_samsung::der::{self, read_tlv};
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::signature::Verifier;
use sha2::Sha512;

fn pem_body(pem: &str, label: &str) -> Vec<u8> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(pem.starts_with(&format!("-----BEGIN {label}-----\n")));
    assert!(pem.trim_end().ends_with(&format!("-----END {label}-----")));
    assert!(pem.lines().all(|l| l.len() <= 64));
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap()
}

/// (info TLV bytes, signature bytes) of a CertificationRequest.
fn split_csr(der_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (tag, body, rest) = read_tlv(der_bytes).unwrap();
    assert_eq!(tag, 0x30);
    assert!(rest.is_empty());
    let info_len = der_bytes.len() - rest.len();
    let _ = info_len;
    let (t1, _, after_info) = read_tlv(body).unwrap();
    assert_eq!(t1, 0x30);
    let info = body[..body.len() - after_info.len()].to_vec();
    let (t2, _, after_alg) = read_tlv(after_info).unwrap();
    assert_eq!(t2, 0x30);
    let (t3, sig, end) = read_tlv(after_alg).unwrap();
    assert_eq!(t3, 0x03);
    assert!(end.is_empty());
    assert_eq!(sig[0], 0, "no unused bits");
    (info, sig[1..].to_vec())
}

#[test]
fn der_lengths_long_form() {
    assert_eq!(&der::tlv(0x04, &[0u8; 5])[..2], &[0x04, 5]);
    assert_eq!(&der::tlv(0x04, &[0u8; 200])[..3], &[0x04, 0x81, 200]);
    assert_eq!(&der::tlv(0x04, &[0u8; 300])[..4], &[0x04, 0x82, 0x01, 0x2C]);
    assert_eq!(
        der::oid(&[1, 2, 840, 113549, 1, 1, 13]),
        vec![0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D]
    );
}

#[test]
fn csr_signature_verifies_and_carries_the_subject() {
    let key = new_key();
    let author = author_csr(&key, "ReTV");
    let bytes = pem_body(&author, "CERTIFICATE REQUEST");
    let (info, sig) = split_csr(&bytes);
    let verifier = VerifyingKey::<Sha512>::new(key.to_public_key());
    verifier
        .verify(&info, &Signature::try_from(sig.as_slice()).unwrap())
        .expect("the CSR's signature verifies with its key");
    // CN=ReTV as UTF8String.
    assert!(bytes
        .windows(6)
        .any(|w| w == [0x0C, 4, b'R', b'e', b'T', b'V']));

    let dist = distributor_csr(
        &key,
        "someone@example.com",
        &["2DCNLQ722RJQQ".to_string(), "ABCDEFGH12".to_string()],
    );
    let bytes = pem_body(&dist, "CERTIFICATE REQUEST");
    let (info, sig) = split_csr(&bytes);
    verifier
        .verify(&info, &Signature::try_from(sig.as_slice()).unwrap())
        .unwrap();
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    // SAN URIs: [6] IA5String, the package one first, then each DUID.
    let uri = |s: &str| {
        let mut v = vec![0x86, s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    };
    assert!(has(&uri("URN:tizen:packageid=")));
    assert!(has(&uri("URN:tizen:deviceid=2DCNLQ722RJQQ")));
    assert!(has(&uri("URN:tizen:deviceid=ABCDEFGH12")));
    assert!(has(b"TizenSDK"));
    assert!(has(b"someone@example.com"));
}

#[test]
fn key_pem_round_trip() {
    let key = new_key();
    let pem = key_pem(&key);
    assert!(pem.starts_with(concat!("-----BEGIN ", "PRIVATE KEY-----")));
    assert_eq!(key_from_pem(&pem).unwrap(), key);
}

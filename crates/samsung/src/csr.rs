//! RSA keys and the two certificate requests Samsung signs: the author
//! (who wrote the app) and the distributor (which TVs, by DUID, may run it).
//! Subjects and extensions as Samsung's Tizen tools send them (ported from
//! Apps2Samsung, MIT).

use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha2::Sha512;
use zeroize::Zeroizing;

use crate::der;

const OID_CN: &[u64] = &[2, 5, 4, 3];
const OID_EMAIL: &[u64] = &[1, 2, 840, 113549, 1, 9, 1];
const OID_EXTENSION_REQUEST: &[u64] = &[1, 2, 840, 113549, 1, 9, 14];
const OID_SUBJECT_ALT_NAME: &[u64] = &[2, 5, 29, 17];
const OID_SHA512_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 13];

pub fn new_key() -> RsaPrivateKey {
    RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("RSA key generation")
}

/// PKCS#8 PEM; zeroed when dropped.
pub fn key_pem(key: &RsaPrivateKey) -> Zeroizing<String> {
    key.to_pkcs8_pem(LineEnding::LF).expect("PKCS#8 encoding")
}

pub fn key_from_pem(pem: &str) -> Option<RsaPrivateKey> {
    RsaPrivateKey::from_pkcs8_pem(pem).ok()
}

/// `-----BEGIN <label>-----`, base64 in 64-column lines.
pub fn pem(label: &str, der_bytes: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der_bytes);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn rdn(oid: &[u64], value: Vec<u8>) -> Vec<u8> {
    der::set(&[der::seq(&[der::oid(oid), value])])
}

/// PKCS#10, SHA512withRSA, as PEM.
fn request(key: &RsaPrivateKey, subject: Vec<u8>, attributes: Vec<u8>) -> String {
    let spki = key
        .to_public_key()
        .to_public_key_der()
        .expect("public key encoding");
    let info = der::seq(&[
        der::integer_zero(),
        subject,
        spki.as_bytes().to_vec(),
        der::context(0, &attributes),
    ]);
    let signature = SigningKey::<Sha512>::new(key.clone()).sign(&info).to_vec();
    let csr = der::seq(&[
        info,
        der::seq(&[der::oid(OID_SHA512_RSA), der::null()]),
        der::bit_string(&signature),
    ]);
    pem("CERTIFICATE REQUEST", &csr)
}

/// The author request: `CN=<cn>`.
pub fn author_csr(key: &RsaPrivateKey, cn: &str) -> String {
    request(key, der::seq(&[rdn(OID_CN, der::utf8(cn))]), Vec::new())
}

/// The distributor request: `CN=TizenSDK, emailAddress=<email>` and the
/// subject alternative names `URN:tizen:packageid=` and
/// `URN:tizen:deviceid=<duid>` for each TV.
pub fn distributor_csr(key: &RsaPrivateKey, email: &str, duids: &[String]) -> String {
    let subject = der::seq(&[
        rdn(OID_CN, der::utf8("TizenSDK")),
        rdn(OID_EMAIL, der::ia5(email)),
    ]);
    let mut names = vec![der::context_primitive(6, b"URN:tizen:packageid=")];
    for duid in duids {
        names.push(der::context_primitive(
            6,
            format!("URN:tizen:deviceid={duid}").as_bytes(),
        ));
    }
    let san = der::seq(&[
        der::oid(OID_SUBJECT_ALT_NAME),
        der::octet_string(&der::seq(&names)),
    ]);
    let attribute = der::seq(&[
        der::oid(OID_EXTENSION_REQUEST),
        der::set(&[der::seq(&[san])]),
    ]);
    request(key, subject, attribute)
}

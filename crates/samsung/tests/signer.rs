use std::io::Read;

use base64::Engine;
use retv_bridge_samsung::csr::new_key;
use retv_bridge_samsung::signer::{
    prop_digest, sign_widget, uri_encode, zip_wgt, Role, Signer, WgtFile,
};
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::signature::Verifier;
use sha2::{Digest, Sha512};

const FAKE_CERT: &str = "-----BEGIN CERTIFICATE-----\nQUFBQQ==\n-----END CERTIFICATE-----\n";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Tizen's own digests of the `#prop` object for each role (the same in every
/// signed Tizen package, so a check that our object is byte-for-byte right).
#[test]
fn prop_digest_matches_tizen() {
    assert_eq!(
        prop_digest(Role::Author),
        "aXbSAVgmAz0GsBUeZ1UmNDRrxkWhDUVGb45dZcNRq429wX3X+x6kaXT3NdNDTSNVTU+ypkysPMGvQY10fG1EWQ=="
    );
    assert_eq!(
        prop_digest(Role::Distributor),
        "/r5npk2VVA46QFJnejgONBEh4BWtjrtu9x/IFeLksjWyGmB/cMWKSJWQl7aU3YRQRZ3AesG8gF7qGyvKX9Snig=="
    );
}

#[test]
fn uris_are_percent_encoded_but_keep_slashes() {
    assert_eq!(uri_encode("service/dist/index.js"), "service/dist/index.js");
    assert_eq!(uri_encode("a b/ü.png"), "a%20b/%C3%BC.png");
}

fn files() -> Vec<WgtFile> {
    vec![
        WgtFile::new("config.xml", b"<widget/>".to_vec()),
        WgtFile::new("index.html", b"<html></html>".to_vec()),
        WgtFile::new("service/dist/index.js", b"module.exports={}".to_vec()),
    ]
}

/// Checks one signature file: every reference's digest, and the RSA-SHA512
/// signature over SignedInfo canonicalised the way the TV does it.
fn check(xml: &str, key: &rsa::RsaPrivateKey, files: &[WgtFile], id: &str) {
    assert!(xml.starts_with(&format!(
        "<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\" Id=\"{id}\">"
    )));
    let start = xml.find("<SignedInfo>").unwrap();
    let end = xml.find("</SignedInfo>").unwrap() + "</SignedInfo>".len();
    let c14n = xml[start..end].replacen(
        "<SignedInfo>",
        "<SignedInfo xmlns=\"http://www.w3.org/2000/09/xmldsig#\">",
        1,
    );
    let sig_b64: String = xml
        .split("<SignatureValue>")
        .nth(1)
        .unwrap()
        .split("</SignatureValue>")
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    let sig = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .unwrap();
    VerifyingKey::<Sha512>::new(key.to_public_key())
        .verify(
            c14n.as_bytes(),
            &Signature::try_from(sig.as_slice()).unwrap(),
        )
        .expect("SignedInfo signature");
    for f in files {
        let marker = format!("<Reference URI=\"{}\">", f.uri);
        let at = xml
            .find(&marker)
            .unwrap_or_else(|| panic!("no reference to {}", f.uri));
        let digest: String = xml[at..]
            .split("<DigestValue>")
            .nth(1)
            .unwrap()
            .split("</DigestValue>")
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(digest, b64(&Sha512::digest(&f.data)), "{}", f.uri);
    }
    assert!(xml.contains("<Reference URI=\"#prop\">"));
    assert!(xml.contains("<X509Certificate>"));
}

#[test]
fn signatures_verify() {
    let author_key = new_key();
    let dist_key = new_key();
    let author = Signer::new(author_key.clone(), vec![FAKE_CERT.into(), FAKE_CERT.into()]);
    let dist = Signer::new(dist_key.clone(), vec![FAKE_CERT.into()]);
    let input = files();
    let signed = sign_widget(input.clone(), &author, &dist);
    let names: Vec<&str> = signed.iter().map(|f| f.uri.as_str()).collect();
    assert_eq!(
        names,
        [
            "signature1.xml",
            "author-signature.xml",
            "config.xml",
            "index.html",
            "service/dist/index.js"
        ]
    );
    let author_xml = String::from_utf8(signed[1].data.clone()).unwrap();
    check(&author_xml, &author_key, &input, "AuthorSignature");
    assert!(!author_xml.contains("URI=\"author-signature.xml\""));
    assert_eq!(author_xml.matches("<X509Certificate>").count(), 2);
    assert!(author_xml.contains("role-author"));

    let dist_xml = String::from_utf8(signed[0].data.clone()).unwrap();
    let mut with_author = vec![signed[1].clone()];
    with_author.extend(input);
    check(&dist_xml, &dist_key, &with_author, "DistributorSignature");
    assert!(dist_xml.contains("role-distributor"));
    assert!(!dist_xml.contains("URI=\"signature1.xml\""));
}

#[test]
fn zip_round_trip() {
    let wgt = zip_wgt(&files());
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(wgt)).unwrap();
    // Three files and the folders service/, service/dist/.
    assert_eq!(z.len(), 5);
    assert!(z.by_name("service/dist/").unwrap().is_dir());
    let mut s = String::new();
    z.by_name("service/dist/index.js")
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    assert_eq!(s, "module.exports={}");
}

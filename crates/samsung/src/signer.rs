//! Signing a Tizen web app (`.wgt`): W3C XML Digital Signatures for Widgets
//! as Tizen checks them: `author-signature.xml` over every file, then
//! `signature1.xml` (the distributor) over those and the author signature.
//!
//! Tizen's choices: SHA-512 digests, RSA-SHA512, exclusive canonicalisation
//! of SignedInfo, and a `#prop` reference to the signature-properties object
//! (canonical XML 1.1). We write every element already in canonical form, so
//! the bytes we sign and digest are the bytes the TV's canonicaliser makes.

use std::io::Write;

use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer as _};
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha512};

const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgtFile {
    /// The path in the package, percent-encoded as in the signatures.
    pub uri: String,
    pub data: Vec<u8>,
}

impl WgtFile {
    /// `path` is the plain relative path (`service/dist/index.js`).
    pub fn new(path: &str, data: Vec<u8>) -> WgtFile {
        WgtFile {
            uri: uri_encode(path),
            data,
        }
    }

    /// The plain path (for the zip entry).
    pub fn path(&self) -> String {
        uri_decode(&self.uri)
    }
}

/// Like JavaScript's `encodeURIComponent`, keeping `/`.
pub fn uri_encode(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')'
            | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn uri_decode(uri: &str) -> String {
    let b = uri.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&uri[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Author,
    Distributor,
}

impl Role {
    fn id(self) -> &'static str {
        match self {
            Role::Author => "AuthorSignature",
            Role::Distributor => "DistributorSignature",
        }
    }
    fn role(self) -> &'static str {
        match self {
            Role::Author => "author",
            Role::Distributor => "distributor",
        }
    }
    fn file(self) -> &'static str {
        match self {
            Role::Author => "author-signature.xml",
            Role::Distributor => "signature1.xml",
        }
    }
}

pub struct Signer {
    key: RsaPrivateKey,
    /// PEM certificates, leaf first.
    chain_pem: Vec<String>,
}

impl Signer {
    pub fn new(key: RsaPrivateKey, chain_pem: Vec<String>) -> Signer {
        Signer { key, chain_pem }
    }
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Base64 in 76-column lines.
fn wrap76(s: &str) -> String {
    s.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The signature-properties object, written in canonical form (its parent's
/// default namespace is added for the digest, see `prop_digest`).
fn prop_object_inner(role: Role) -> String {
    let id = role.id();
    format!(
        "<SignatureProperties xmlns:dsp=\"http://www.w3.org/2009/xmldsig-properties\">\
<SignatureProperty Id=\"profile\" Target=\"#{id}\"><dsp:Profile URI=\"http://www.w3.org/ns/widgets-digsig#profile\"></dsp:Profile></SignatureProperty>\
<SignatureProperty Id=\"role\" Target=\"#{id}\"><dsp:Role URI=\"http://www.w3.org/ns/widgets-digsig#role-{}\"></dsp:Role></SignatureProperty>\
<SignatureProperty Id=\"identifier\" Target=\"#{id}\"><dsp:Identifier></dsp:Identifier></SignatureProperty>\
</SignatureProperties>",
        role.role()
    )
}

/// SHA-512 of the `#prop` object under canonical XML 1.1: the object as a
/// document subset carries its inherited default namespace.
pub fn prop_digest(role: Role) -> String {
    let c14n = format!(
        "<Object xmlns=\"{DSIG_NS}\" Id=\"prop\">{}</Object>",
        prop_object_inner(role)
    );
    b64(&Sha512::digest(c14n.as_bytes()))
}

fn reference(uri: &str, digest_b64: &str, prop: bool) -> String {
    let transforms = if prop {
        "<Transforms>\n<Transform Algorithm=\"http://www.w3.org/2006/12/xml-c14n11\"></Transform>\n</Transforms>\n"
    } else {
        ""
    };
    format!(
        "<Reference URI=\"{uri}\">\n{transforms}<DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha512\"></DigestMethod>\n<DigestValue>{}</DigestValue>\n</Reference>\n",
        wrap76(digest_b64)
    )
}

fn pem_body(pem: &str) -> String {
    pem.lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect()
}

/// One signature file over `files`, in their order.
pub fn signature_xml(role: Role, files: &[WgtFile], signer: &Signer) -> String {
    let mut refs = String::new();
    for f in files {
        refs.push_str(&reference(&f.uri, &b64(&Sha512::digest(&f.data)), false));
    }
    refs.push_str(&reference("#prop", &prop_digest(role), true));
    let signed_info = format!(
        "<SignedInfo>\n<CanonicalizationMethod Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></CanonicalizationMethod>\n<SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha512\"></SignatureMethod>\n{refs}</SignedInfo>"
    );
    // Exclusive c14n of SignedInfo: the default namespace it uses is
    // declared on it; nothing else changes.
    let c14n = signed_info.replacen(
        "<SignedInfo>",
        &format!("<SignedInfo xmlns=\"{DSIG_NS}\">"),
        1,
    );
    let signature = SigningKey::<Sha512>::new(signer.key.clone())
        .sign(c14n.as_bytes())
        .to_vec();
    let mut certs = String::new();
    for pem in &signer.chain_pem {
        certs.push_str(&format!(
            "<X509Certificate>\n{}\n</X509Certificate>\n",
            wrap76(&pem_body(pem))
        ));
    }
    format!(
        "<Signature xmlns=\"{DSIG_NS}\" Id=\"{}\">\n{signed_info}\n<SignatureValue>\n{}\n</SignatureValue>\n<KeyInfo>\n<X509Data>\n{certs}</X509Data>\n</KeyInfo>\n<Object Id=\"prop\">{}</Object>\n</Signature>\n",
        role.id(),
        wrap76(&b64(&signature)),
        prop_object_inner(role)
    )
}

/// The package with both signatures first: `signature1.xml`,
/// `author-signature.xml`, then `files` (signature files in them dropped).
pub fn sign_widget(files: Vec<WgtFile>, author: &Signer, distributor: &Signer) -> Vec<WgtFile> {
    let files: Vec<WgtFile> = files
        .into_iter()
        .filter(|f| {
            let name = f.uri.rsplit('/').next().unwrap_or("");
            !(name == "author-signature.xml"
                || (name.starts_with("signature") && name.ends_with(".xml")))
        })
        .collect();
    let author_sig = WgtFile::new(
        Role::Author.file(),
        signature_xml(Role::Author, &files, author).into_bytes(),
    );
    let mut for_dist = vec![author_sig];
    for_dist.extend(files);
    let dist_sig = WgtFile::new(
        Role::Distributor.file(),
        signature_xml(Role::Distributor, &for_dist, distributor).into_bytes(),
    );
    let mut out = vec![dist_sig];
    out.extend(for_dist);
    out
}

/// The `.wgt` (a zip, deflated), with folder entries before their files as
/// Samsung's tools write them.
pub fn zip_wgt(files: &[WgtFile]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut dirs = std::collections::BTreeSet::new();
    for f in files {
        let path = f.path();
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            let dir = format!("{}/", parts[..i].join("/"));
            if dirs.insert(dir.clone()) {
                w.add_directory(dir, opts).expect("zip folder");
            }
        }
        w.start_file(path, opts).expect("zip entry");
        w.write_all(&f.data).expect("zip write");
    }
    w.finish().expect("zip finish").into_inner()
}

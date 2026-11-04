//! The Samsung certificate for one TV: an author and a distributor
//! certificate from Samsung's certificate service, each with its own key and
//! Samsung's CA after it, the distributor one tied to the TV's DUID.
//!
//! Endpoints, form fields and CA choice are ported from Apps2Samsung (MIT):
//! author `POST /apis/v3/authors`; distributor `POST /apis/v1/distributors`
//! (the device profile XML) and `/apis/v3/distributors` (the certificate),
//! `privilege_level=public`, `developer_type=Individual`, `platform=VD`.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::csr::{author_csr, distributor_csr, key_pem, new_key};
use crate::signin::SignIn;
use crate::SamsungError;

/// Samsung's CA for author certificates (public, from Samsung's Tizen
/// certificate extension).
/// (`ca/*.txt`: Samsung's public CA certificates in PEM, kept as `.txt` so
/// the public repo's check for key files does not stop on them.)
pub const DEVELOPER_CA: &str = include_str!("../ca/vd_tizen_dev_author_ca.txt");
/// Samsung's CA for public-level distributor certificates.
pub const DISTRIBUTOR_CA: &str = include_str!("../ca/vd_tizen_dev_public2.txt");
pub const SAMSUNG_CERT_SERVICE: &str = "https://svdca.samsungqbe.com";
/// The author name in the certificate.
pub const CERT_NAME: &str = "ReTV";
/// Used when Samsung does not send the account's email (as `tools/samsung/cert.js`).
pub const FALLBACK_EMAIL: &str = "developer@klinkov.com";

/// A TV's DUID as `0 getduid` prints it.
pub fn valid_duid(duid: &str) -> bool {
    (8..=32).contains(&duid.len()) && duid.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub struct CertApi {
    base: String,
    client: reqwest::Client,
}

/// The form body, built in one buffer sized up front (no temporary copies of
/// the token, no regrowth leaving old copies behind); zeroed when dropped.
pub fn multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    file: (&str, &str, &str),
) -> Zeroizing<Vec<u8>> {
    let (fname, filename, data) = file;
    let mut parts: Vec<&str> = Vec::new();
    for (name, value) in fields {
        parts.extend([
            "--",
            boundary,
            "\r\nContent-Disposition: form-data; name=\"",
            name,
            "\"\r\n\r\n",
            value,
            "\r\n",
        ]);
    }
    parts.extend([
        "--",
        boundary,
        "\r\nContent-Disposition: form-data; name=\"",
        fname,
        "\"; filename=\"",
        filename,
        "\"\r\nContent-Type: application/octet-stream\r\n\r\n",
        data,
        "\r\n--",
        boundary,
        "--\r\n",
    ]);
    let mut b = Zeroizing::new(Vec::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for p in parts {
        b.extend_from_slice(p.as_bytes());
    }
    b
}

struct ZeroBody(Zeroizing<Vec<u8>>);

impl AsRef<[u8]> for ZeroBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl CertApi {
    pub fn new(base: String) -> CertApi {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("HTTP client");
        CertApi { base, client }
    }

    pub fn samsung() -> CertApi {
        CertApi::new(SAMSUNG_CERT_SERVICE.to_string())
    }

    async fn post(
        &self,
        path: &str,
        s: &SignIn,
        extra: &[(&str, &str)],
        filename: &str,
        csr: &str,
    ) -> Result<String, SamsungError> {
        let boundary = format!("retv{:032x}", rand::random::<u128>());
        let mut fields = vec![
            ("access_token", s.access.as_str()),
            ("user_id", s.user_id.as_str()),
            ("platform", "VD"),
        ];
        fields.extend_from_slice(extra);
        let body = multipart(&boundary, &fields, ("csr", filename, csr));
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            // The body owns the zeroing buffer: it is wiped when reqwest drops it.
            .body(bytes::Bytes::from_owner(ZeroBody(body)))
            .send()
            .await
            // reqwest's errors name the URL only; the token is in the body.
            .map_err(|e| SamsungError::Other(format!("Samsung's certificate service: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.as_u16() == 403 {
            return Err(SamsungError::TooManyRequests);
        }
        if !status.is_success() {
            let desc = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/description")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(SamsungError::Samsung(desc));
        }
        Ok(text)
    }

    /// The signed author certificate (PEM).
    pub async fn author(&self, s: &SignIn, csr: &str) -> Result<String, SamsungError> {
        let pem = self
            .post("/apis/v3/authors", s, &[], "author.csr", csr)
            .await?;
        expect_cert(pem)
    }

    /// (device profile XML, signed distributor certificate PEM).
    pub async fn distributor(
        &self,
        s: &SignIn,
        csr: &str,
    ) -> Result<(String, String), SamsungError> {
        let extra = [
            ("privilege_level", "public"),
            ("developer_type", "Individual"),
        ];
        let profile = self
            .post("/apis/v1/distributors", s, &extra, "distributor.csr", csr)
            .await?;
        let cert = self
            .post("/apis/v3/distributors", s, &extra, "distributor.csr", csr)
            .await?;
        Ok((profile, expect_cert(cert)?))
    }
}

fn expect_cert(pem: String) -> Result<String, SamsungError> {
    if pem.contains("-----BEGIN CERTIFICATE-----") {
        Ok(pem.replace("\r\n", "\n"))
    } else {
        Err(SamsungError::Samsung("no certificate in the answer".into()))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamsungCert {
    pub duid: String,
    pub author_key_pem: String,
    /// Leaf, then Samsung's author CA.
    pub author_chain: Vec<String>,
    pub distributor_key_pem: String,
    /// Leaf, then Samsung's distributor CA.
    pub distributor_chain: Vec<String>,
    pub profile_xml: String,
    pub created_ms: u64,
}

impl fmt::Debug for SamsungCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SamsungCert")
            .field("duid", &self.duid)
            .field("created_ms", &self.created_ms)
            .finish_non_exhaustive()
    }
}

/// Two new keys, two requests, Samsung's answers with its CAs after them.
pub async fn create(
    api: &CertApi,
    s: &SignIn,
    duid: &str,
    now_ms: u64,
) -> Result<SamsungCert, SamsungError> {
    if !valid_duid(duid) {
        return Err(SamsungError::Other("the TV's DUID looks wrong".into()));
    }
    let (author_key, dist_key) = tokio::task::spawn_blocking(|| (new_key(), new_key()))
        .await
        .map_err(|e| SamsungError::Other(e.to_string()))?;
    let email = s
        .email
        .clone()
        .unwrap_or_else(|| FALLBACK_EMAIL.to_string());
    let author = api.author(s, &author_csr(&author_key, CERT_NAME)).await?;
    let (profile_xml, distributor) = api
        .distributor(s, &distributor_csr(&dist_key, &email, &[duid.to_string()]))
        .await?;
    Ok(SamsungCert {
        duid: duid.to_string(),
        author_key_pem: key_pem(&author_key).to_string(),
        author_chain: vec![author, DEVELOPER_CA.replace("\r\n", "\n")],
        distributor_key_pem: key_pem(&dist_key).to_string(),
        distributor_chain: vec![distributor, DISTRIBUTOR_CA.replace("\r\n", "\n")],
        profile_xml,
        created_ms: now_ms,
    })
}

/// The app's private Samsung folder: `0700`, files `0600`.
pub struct CertStore {
    dir: PathBuf,
}

impl CertStore {
    pub fn new(dir: PathBuf) -> CertStore {
        CertStore { dir }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Writes `name` (a plain file name) with mode 0600, through a temp file.
    pub fn write_private(&self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(std::io::Error::other("bad file name"));
        }
        self.ensure_dir()?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, self.dir.join(name))
    }

    pub fn read_private(&self, name: &str) -> Option<Vec<u8>> {
        if name.contains(['/', '\\']) {
            return None;
        }
        std::fs::read(self.dir.join(name)).ok()
    }

    pub fn save(&self, cert: &SamsungCert) -> std::io::Result<()> {
        let json = serde_json::to_vec(cert).map_err(std::io::Error::other)?;
        self.write_private(&format!("cert-{}.json", cert.duid), &json)
    }

    pub fn load(&self, duid: &str) -> Option<SamsungCert> {
        if !valid_duid(duid) {
            return None;
        }
        let data = self.read_private(&format!("cert-{duid}.json"))?;
        serde_json::from_slice::<SamsungCert>(&data)
            .ok()
            .filter(|c| c.duid == duid)
    }
}

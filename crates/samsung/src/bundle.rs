//! ReTV Home for Samsung, bundled at build time, and installing it: sign
//! with the TV's certificate, push over sdb, `vd_appinstall`, `was_execute`
//! (the steps of `tools/samsung/install.js`, verified on a 2021 set).

use std::net::SocketAddr;

use crate::apps::{uninstall_command, uninstall_ok};
use crate::certs::{valid_duid, SamsungCert};
use crate::csr::key_from_pem;
use crate::sdb::{Sdb, PUSH_PATH};
use crate::signer::{sign_widget, zip_wgt, Signer, WgtFile};
use crate::SamsungError;

pub use crate::bundle_rules::shipped;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/wgt.rs"));
}

/// This build carries ReTV Home for Samsung (cheap; for the window's poll).
pub fn has_bundle() -> bool {
    !generated::FILES.is_empty()
}

/// The bundled release files, or `None` when this build has none.
pub fn bundled() -> Option<Vec<WgtFile>> {
    if generated::FILES.is_empty() {
        return None;
    }
    Some(
        generated::FILES
            .iter()
            .map(|(path, data)| WgtFile::new(path, data.to_vec()))
            .collect(),
    )
}

fn attr<'a>(xml: &'a str, element: &str, name: &str) -> Option<&'a str> {
    let at = xml.find(&format!("<{element} "))?;
    let tag = &xml[at..at + xml[at..].find('>')?];
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

/// (package id, app id) from `config.xml`'s `<tizen:application>`.
pub fn ids(config_xml: &str) -> Option<(String, String)> {
    let pkg = attr(config_xml, "tizen:application", "package")?;
    let app = attr(config_xml, "tizen:application", "id")?;
    let ok = pkg.len() == 10 && pkg.bytes().all(|b| b.is_ascii_alphanumeric());
    (ok && app.starts_with(&format!("{pkg}."))).then(|| (pkg.to_string(), app.to_string()))
}

/// The TV's DUID (`0 getduid`).
pub async fn read_duid(addr: SocketAddr) -> Result<String, SamsungError> {
    let mut sdb = Sdb::connect(addr).await?;
    let duid = sdb.shell("0 getduid").await?.trim().to_string();
    if valid_duid(&duid) {
        Ok(duid)
    } else {
        Err(SamsungError::Other("the TV did not give its DUID".into()))
    }
}

fn signer(key_pem: &str, chain: &[String]) -> Result<Signer, SamsungError> {
    let key = key_from_pem(key_pem)
        .ok_or_else(|| SamsungError::Other("the saved certificate key cannot be read".into()))?;
    Ok(Signer::new(key, chain.to_vec()))
}

/// The installer's last lines without its progress lines.
fn tail(out: &str) -> String {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("installing["))
        .collect();
    lines[lines.len().saturating_sub(4)..].join("\n")
}

/// The installer refused because the app on the TV is signed by another
/// author certificate (`install failed[118, -11], reason: Author
/// certificate not match`).
pub fn other_author(out: &str) -> bool {
    out.to_lowercase().contains("author certificate not match")
        || out.contains("install failed[118, -11]")
}

/// Signs `files` with `cert`, installs them on the TV at `addr` and starts
/// the app; returns its app id. Refuses before pushing when the certificate
/// is for another TV. An app already there from another author certificate
/// is [`SamsungError::OtherAuthor`]; [`reinstall`] replaces it.
pub async fn install(
    addr: SocketAddr,
    files: Vec<WgtFile>,
    cert: &SamsungCert,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<String, SamsungError> {
    install_with(addr, files, cert, false, progress).await
}

/// [`install`] after removing the app already on the TV (its settings and
/// sign-in there are lost). The old app is removed only after the DUID check
/// and the signing, right before the push.
pub async fn reinstall(
    addr: SocketAddr,
    files: Vec<WgtFile>,
    cert: &SamsungCert,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<String, SamsungError> {
    install_with(addr, files, cert, true, progress).await
}

async fn install_with(
    addr: SocketAddr,
    files: Vec<WgtFile>,
    cert: &SamsungCert,
    replace: bool,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<String, SamsungError> {
    let config = files
        .iter()
        .find(|f| f.uri == "config.xml")
        .map(|f| String::from_utf8_lossy(&f.data).into_owned())
        .ok_or_else(|| SamsungError::Other("the package has no config.xml".into()))?;
    let (pkg, app_id) =
        ids(&config).ok_or_else(|| SamsungError::Other("config.xml has no app id".into()))?;

    progress("connecting to the TV (sdb)");
    let mut sdb = Sdb::connect(addr).await?;
    let duid = sdb.shell("0 getduid").await?.trim().to_string();
    if duid != cert.duid {
        return Err(SamsungError::WrongCertificate);
    }

    progress("signing ReTV Home");
    let author = signer(&cert.author_key_pem, &cert.author_chain)?;
    let distributor = signer(&cert.distributor_key_pem, &cert.distributor_chain)?;
    let wgt =
        tokio::task::spawn_blocking(move || zip_wgt(&sign_widget(files, &author, &distributor)))
            .await
            .map_err(|e| SamsungError::Other(e.to_string()))?;

    if replace {
        progress("removing the ReTV Home already on the TV");
        let cmd = uninstall_command(&pkg)
            .ok_or_else(|| SamsungError::Other("config.xml has no app id".into()))?;
        let out = sdb.shell(&cmd).await?;
        if !uninstall_ok(&out) {
            return Err(SamsungError::InstallFailed(format!(
                "could not remove the old ReTV Home: {}",
                tail(&out)
            )));
        }
    }

    progress(&format!("sending the package ({} KB)", wgt.len() / 1024));
    sdb.push(&wgt, PUSH_PATH, 0o100644).await?;

    progress("installing");
    let out = sdb
        .shell(&format!("0 vd_appinstall {pkg} {PUSH_PATH}"))
        .await?;
    if !out.contains("install completed") {
        if other_author(&out) {
            return Err(SamsungError::OtherAuthor);
        }
        return Err(SamsungError::InstallFailed(tail(&out)));
    }
    progress("starting ReTV Home");
    let _ = sdb.shell(&format!("0 was_execute {app_id}")).await?;
    Ok(app_id)
}

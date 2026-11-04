//! Samsung (Tizen) TVs from the Mac: finding them on the home network, the
//! sdb client for Developer Mode, the Samsung certificate, signing and
//! installing ReTV Home for Samsung, app cleanup and the TV's remote keys.
//!
//! Every network piece takes its address, so tests run against local fakes.

pub mod apps;
pub mod bundle;
mod bundle_rules;
pub mod certs;
pub mod csr;
pub mod der;
pub mod privacy;
pub mod remote;
pub mod sdb;
pub mod signer;
pub mod signin;
pub mod ssdp;
pub mod tv;

use std::fmt;

/// What can go wrong talking to a Samsung TV or to Samsung's servers. The
/// texts never carry a token, a password or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamsungError {
    /// The TV did not answer on its REST API (port 8001).
    TvUnreachable(String),
    /// The TV answered something that is not Samsung's REST API.
    NotSamsung,
    /// Samsung's certificate service refused: too many requests for now.
    TooManyRequests,
    /// Samsung's certificate service refused, with its reason.
    Samsung(String),
    /// sdb (Developer Mode) failed.
    Sdb(sdb::SdbError),
    /// The saved certificate is for another TV (another DUID).
    WrongCertificate,
    /// The TV's installer did not finish; its last lines.
    InstallFailed(String),
    /// The app on the TV is signed by another author certificate: only
    /// removing it first lets this one in.
    OtherAuthor,
    /// The sign-in callback port could not be taken on both loopbacks.
    CallbackPort(String),
    /// Any other failure, with an English text for the log.
    Other(String),
}

impl fmt::Display for SamsungError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SamsungError::TvUnreachable(e) => write!(f, "the TV does not answer: {e}"),
            SamsungError::NotSamsung => write!(f, "not a Samsung TV"),
            SamsungError::TooManyRequests => {
                write!(f, "Samsung: too many requests, try again later")
            }
            SamsungError::Samsung(e) => write!(f, "Samsung: {e}"),
            SamsungError::Sdb(e) => write!(f, "{e}"),
            SamsungError::WrongCertificate => {
                write!(f, "the saved certificate is for another TV")
            }
            SamsungError::InstallFailed(tail) => write!(f, "install failed: {tail}"),
            SamsungError::OtherAuthor => write!(
                f,
                "install failed: the ReTV Home on the TV is signed by another author certificate"
            ),
            SamsungError::CallbackPort(e) => write!(f, "the sign-in port is not free: {e}"),
            SamsungError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SamsungError {}

impl From<sdb::SdbError> for SamsungError {
    fn from(e: sdb::SdbError) -> Self {
        SamsungError::Sdb(e)
    }
}

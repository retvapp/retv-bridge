//! «Войти в ReTV» for ReTV Bridge: the app sign-in of the ReTV server
//! (a device code confirmed on the site in the system browser, RFC 8628 and
//! RFC 8252), a rotating refresh token kept in the OS key store, and the
//! account's label, plan and TVs. No token is ever written to a file or the log.

pub mod api;
pub mod cell;
pub mod secret;
pub mod session;
pub mod store;

pub use api::{AccountView, Api, ApiError, CodeRequest, Tv, TvPrivacy};
pub use cell::Cell;
pub use secret::Secret;
pub use session::{Account, Endpoints, Fail, Phase, ReTvSites, SignInEnd, Snapshot, Timing};
pub use store::{os_store, KeyStore, MemoryStore};

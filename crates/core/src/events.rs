//! Events the core sends to the shell (tray, window, CLI log).

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeEvent {
    Log(String),
    PairCode { code: String, expires_at_ms: u64 },
    PairingDisabled,
    TvConnected { serial: String, model: String },
    TvDisconnected { serial: String },
}
